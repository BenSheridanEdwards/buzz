//! Negotiated Hermes gateway extensions. Native request IDs retain their ownership.
use super::{AcpClient, AcpError};
use crate::background_routes::attachment as store;
use serde_json::{json, Value};

pub(super) fn is_native_control(prompt: &Value) -> bool {
    prompt.as_array().is_some_and(|blocks| {
        !blocks.is_empty()
            && blocks[0]["type"] == "text"
            && matches!(
                blocks[0]["text"]
                    .as_str()
                    .and_then(|s| s.split_whitespace().next()),
                Some("/approve" | "/deny")
            )
    })
}

pub(super) fn negotiate(result: &Value) -> Result<bool, AcpError> {
    let Some(cap) = result.pointer("/agentCapabilities/_meta/hermesAttachment") else {
        return Ok(false);
    };
    if cap["version"] != 1
        || [
            "historyFreeReplay",
            "deliveryReplay",
            "activeTurnSnapshot",
            "terminalReceipts",
            "canonicalAsyncWake",
            "retainedAdmission",
        ]
        .iter()
        .any(|f| cap["features"][f] != true)
    {
        return Err(store::invalid(
            "unsupported version or incomplete feature set",
        ));
    }
    Ok(true)
}

/// Map a durable terminal to the ACP stop reason; failures stay scoped to this turn.
fn terminal_outcome(terminal: &Value) -> Result<super::StopReason, AcpError> {
    if terminal.get("error").is_some() {
        return Err(store::invalid(&format!("turn failed: {terminal}")));
    }
    terminal["stopReason"]
        .as_str()
        .and_then(super::StopReason::from_str)
        .ok_or_else(|| store::invalid(&format!("turn ended with unsupported outcome: {terminal}")))
}

/// Frames one replay page may buffer before the session is failed.
const MAX_REPLAY_BUFFER: usize = 8192;

/// Per-connection canonical delivery ownership.
#[derive(Default)]
pub(super) struct Tracking {
    /// Sessions whose replay completed (or were created) on this connection.
    pub(super) attached: std::collections::HashSet<String>,
    /// The session whose replay page is in flight, with its buffered frames.
    loading: Option<(String, Vec<Value>)>,
    /// First rejected frame per session, reported by that session's next load.
    faults: std::collections::HashMap<String, String>,
}

impl AcpClient {
    pub(super) async fn request_attachment_cancel(
        &mut self,
        sid: &str,
    ) -> Result<String, AcpError> {
        let dir = self
            .background_routes_dir
            .as_ref()
            .ok_or_else(|| store::invalid("missing cancellation state"))?;
        let turn = store::load(dir, sid)?
            .active_turn
            .ok_or_else(|| store::invalid("no correlated canonical turn to cancel"))?;
        self.send_request("session/cancel", json!({"sessionId":sid,"turnId":turn}))
            .await?;
        Ok(turn)
    }

    pub(super) async fn cancel_attachment_until(
        &mut self,
        sid: &str,
        deadline: tokio::time::Instant,
    ) -> Result<super::StopReason, AcpError> {
        let turn = self.request_attachment_cancel(sid).await?;
        let dir = self
            .background_routes_dir
            .clone()
            .ok_or_else(|| store::invalid("missing cancellation state"))?;
        loop {
            self.load_attachment(sid, "/").await?;
            if let Some(terminal) = store::load(&dir, sid)?.terminals.get(&turn) {
                return terminal_outcome(terminal);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(store::invalid(
                    "canonical cancellation unresolved; gateway still owns work",
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    pub(super) async fn admit_attachment(
        &mut self,
        sid: &str,
        prompt: Value,
        max_duration: std::time::Duration,
    ) -> Result<super::StopReason, AcpError> {
        let dir = self
            .background_routes_dir
            .clone()
            .ok_or_else(|| store::invalid("durable return routes required"))?;
        let route = crate::background_routes::load(&dir, sid)
            .ok_or_else(|| store::invalid("missing durable trigger/scope"))?;
        let admission = route.trigger.id.to_hex();
        let mut state = store::load(&dir, sid)?;
        let existing = state.admissions.get(&admission).cloned();
        let mut params = existing
            .clone()
            .unwrap_or_else(|| json!({"sessionId":sid,"admissionId":admission,"prompt":prompt}));
        // The turn binding is local bookkeeping, never part of the admission.
        if let Some(fields) = params.as_object_mut() {
            fields.remove("turnId");
        }
        if existing.is_none() {
            if state.admissions.len() >= 4096 {
                return Err(store::invalid("admission capacity"));
            }
            state.admissions.insert(admission.clone(), params.clone());
            store::save(&dir, sid, &state)?;
        }
        let status_params = json!({"sessionId":sid,"admissionId":admission});
        let mut receipt = if existing.is_some() {
            self.send_request("_hermes/turn/status", status_params.clone())
                .await?
        } else {
            self.send_request("_hermes/turn/admit", params.clone())
                .await?
        };
        // A status has no authority until both identities match this request.
        if receipt["sessionId"] != sid || receipt["admissionId"] != admission {
            return Err(store::invalid("admission receipt identity mismatch"));
        }
        if receipt["status"] == "not_found" {
            receipt = self.send_request("_hermes/turn/admit", params).await?;
        }
        let deadline = tokio::time::Instant::now() + max_duration;
        loop {
            if receipt["sessionId"] != sid || receipt["admissionId"] != admission {
                return Err(store::invalid("admission receipt identity mismatch"));
            }
            match receipt["status"].as_str() {
                Some("unknown" | "error" | "rejected" | "not_found") => {
                    return Err(store::invalid(&format!("admission unresolved: {receipt}")))
                }
                Some("pending" | "in_progress" | "completed" | "cancelled") => {}
                _ => return Err(store::invalid("invalid admission status")),
            }
            self.load_attachment(sid, "/").await?;
            let mut state = store::load(&dir, sid)?;
            if let Some(turn) = receipt["turnId"].as_str() {
                // Bind the admission to its turn so publication can release it.
                let record = state
                    .admissions
                    .get_mut(&admission)
                    .and_then(Value::as_object_mut);
                if let Some(record) = record.filter(|r| r.get("turnId") != Some(&json!(turn))) {
                    record.insert("turnId".into(), turn.into());
                    if state.published.contains(&format!("turn:{turn}")) {
                        state.admissions.remove(&admission);
                    }
                    store::save(&dir, sid, &state)?;
                }
                if let Some(terminal) = state.terminals.get(turn) {
                    // The durable outbox already holds this turn's visible outcome.
                    return terminal_outcome(terminal);
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(store::invalid(
                    "canonical work remains owned by gateway; observation deadline reached",
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            receipt = self
                .send_request("_hermes/turn/status", status_params.clone())
                .await?;
        }
    }

    pub(crate) fn canonical_attachment(&self) -> bool {
        self.hermes_attachment
    }

    /// Consume a durable frame without ever failing the in-flight request.
    ///
    /// Frames for the session being loaded are buffered and applied against
    /// the replay fence. Live frames are applied only for sessions whose
    /// replay completed on this connection; otherwise replay will deliver
    /// them in order. A rejected frame detaches only its own session: the
    /// cursor stays put, and that session's next load replays and reports it.
    pub(super) fn accept_attachment_frame(&mut self, msg: &Value) -> Result<(), AcpError> {
        if !self.hermes_attachment || msg.pointer("/params/_meta/deliveryId").is_none() {
            return Ok(());
        }
        let Some(sid) = msg.pointer("/params/sessionId").and_then(Value::as_str) else {
            self.attachment_fault("", "durable delivery without session identity".into());
            return Ok(());
        };
        let tracking = &mut self.attachment_tracking;
        if let Some((loading, buffer)) = &mut tracking.loading {
            if loading == sid {
                if buffer.len() < MAX_REPLAY_BUFFER {
                    buffer.push(msg.clone());
                } else {
                    let sid = sid.to_owned();
                    self.attachment_fault(&sid, "replay page exceeded buffer".into());
                }
                return Ok(());
            }
        }
        if !tracking.attached.contains(sid) {
            tracing::debug!(sid, "live canonical delivery deferred to replay");
            return Ok(());
        }
        let accepted = match &self.background_routes_dir {
            Some(dir) => store::accept(dir, msg),
            None => Err(store::invalid("durable return routes required")),
        };
        if let Err(error) = accepted {
            let sid = sid.to_owned();
            self.attachment_fault(&sid, error.to_string());
        }
        Ok(())
    }

    fn attachment_fault(&mut self, sid: &str, error: String) {
        tracing::error!(sid, %error, "canonical delivery rejected; session detached until replay");
        self.observe("attachment_warning", json!({"sessionId":sid,"error":error}));
        self.attachment_tracking.attached.remove(sid);
        self.attachment_tracking
            .faults
            .entry(sid.to_owned())
            .or_insert(error);
    }

    pub(super) async fn load_attachment(&mut self, sid: &str, cwd: &str) -> Result<(), AcpError> {
        self.attachment_tracking.faults.remove(sid);
        let result = self.load_attachment_pages(sid, cwd).await;
        self.attachment_tracking.loading = None;
        if result.is_ok() {
            self.attachment_tracking.attached.insert(sid.to_owned());
        } else {
            self.attachment_tracking.attached.remove(sid);
        }
        result
    }

    async fn load_attachment_pages(&mut self, sid: &str, cwd: &str) -> Result<(), AcpError> {
        let dir = self
            .background_routes_dir
            .clone()
            .ok_or_else(|| store::invalid("durable return routes required"))?;
        let mut cursor = store::load(&dir, sid)?.cursor;
        for _ in 0..128 {
            self.attachment_tracking.loading = Some((sid.to_owned(), Vec::new()));
            let result = self.send_request("session/load", json!({"sessionId":sid,"cwd":cwd,"mcpServers":[],"_meta":{"history":false,"afterDeliveryId":cursor}})).await;
            let frames = self
                .attachment_tracking
                .loading
                .take()
                .map(|(_, frames)| frames)
                .unwrap_or_default();
            let result = result?;
            if let Some(fault) = self.attachment_tracking.faults.remove(sid) {
                return Err(store::invalid(&fault));
            }
            let meta = &result["_meta"];
            let next = meta["lastDeliveryId"]
                .as_i64()
                .ok_or_else(|| store::invalid("missing replay cursor"))?;
            let more = meta["hasMoreDeliveries"]
                .as_bool()
                .ok_or_else(|| store::invalid("missing replay fence"))?;
            if next < cursor
                || (more && next == cursor)
                || meta["historyIncluded"] != false
                || meta["replayComplete"] != !more
            {
                return Err(store::invalid("invalid replay fence"));
            }
            for frame in &frames {
                let id = frame
                    .pointer("/params/_meta/deliveryId")
                    .and_then(Value::as_i64);
                // Beyond an incomplete page, the next page replays it in order.
                if more && id.is_some_and(|id| id > next) {
                    continue;
                }
                store::accept(&dir, frame)?;
            }
            let mut state = store::load(&dir, sid)?;
            // A live frame past a complete replay may precede the response.
            state.cursor = state.cursor.max(next);
            if !more {
                state.active_turn = meta
                    .pointer("/activeTurn/turnId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            store::save(&dir, sid, &state)?;
            cursor = next;
            if !more {
                if meta.pointer("/activeTurn/snapshotTruncated") == Some(&Value::Bool(true)) {
                    self.observe(
                        "attachment_warning",
                        json!({"sessionId":sid,"error":"active snapshot truncated"}),
                    );
                }
                return Ok(());
            }
        }
        Err(store::invalid("replay page budget exhausted"))
    }
}
