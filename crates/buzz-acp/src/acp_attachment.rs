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
                if terminal["stopReason"] == "cancelled" {
                    return Ok(super::StopReason::Cancelled);
                }
                if terminal["stopReason"] == "end_turn" {
                    return Ok(super::StopReason::EndTurn);
                }
                return Err(store::invalid(&format!(
                    "cancelled turn failed: {terminal}"
                )));
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
        let params = existing
            .clone()
            .unwrap_or_else(|| json!({"sessionId":sid,"admissionId":admission,"prompt":prompt}));
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
            let state = store::load(&dir, sid)?;
            if let Some(turn) = receipt["turnId"].as_str() {
                if let Some(terminal) = state.terminals.get(turn) {
                    if terminal.get("error").is_some() {
                        return Err(store::invalid(&format!("turn failed: {terminal}")));
                    }
                    if terminal["stopReason"] == "cancelled" {
                        return Ok(super::StopReason::Cancelled);
                    }
                    if terminal["stopReason"] == "end_turn" && state.finals.contains_key(turn) {
                        return Ok(super::StopReason::EndTurn);
                    }
                    return Err(store::invalid(
                        "terminal without complete authoritative final",
                    ));
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

    pub(super) fn accept_attachment_frame(&mut self, msg: &Value) -> Result<(), AcpError> {
        if !self.hermes_attachment {
            return Ok(());
        }
        if msg.pointer("/params/_meta/deliveryId").is_some() {
            let dir = self
                .background_routes_dir
                .as_ref()
                .ok_or_else(|| store::invalid("durable return routes required"))?;
            store::accept(dir, msg)?;
        }
        Ok(())
    }

    pub(super) async fn load_attachment(&mut self, sid: &str, cwd: &str) -> Result<(), AcpError> {
        let dir = self
            .background_routes_dir
            .clone()
            .ok_or_else(|| store::invalid("durable return routes required"))?;
        let mut cursor = store::load(&dir, sid)?.cursor;
        for _ in 0..128 {
            let result = self.send_request("session/load", json!({"sessionId":sid,"cwd":cwd,"mcpServers":[],"_meta":{"history":false,"afterDeliveryId":cursor}})).await?;
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
            let mut state = store::load(&dir, sid)?;
            if state.cursor > next {
                return Err(store::invalid("live delivery overtook replay fence"));
            }
            state.cursor = next;
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
