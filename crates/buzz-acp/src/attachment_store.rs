//! Durable canonical ACP consumption. Cursor and staged replacements commit together.
use crate::acp::AcpError;
#[path = "attachment_publication.rs"]
mod publication;
pub(crate) use publication::{Publication, PERMANENTLY_REJECTED};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct State {
    #[serde(default)]
    pub(crate) revision: u64,
    pub cursor: i64,
    #[serde(default)]
    pub active_turn: Option<String>,
    pub groups: BTreeMap<String, Group>,
    pub finals: BTreeMap<String, String>,
    pub terminals: BTreeMap<String, Value>,
    #[serde(default)]
    pub admissions: BTreeMap<String, Value>,
    #[serde(default)]
    pub outbound: BTreeMap<String, String>,
    #[serde(default)]
    pub outbound_routes: BTreeMap<String, super::Route>,
    #[serde(default)]
    pub publications: BTreeMap<String, nostr::Event>,
    /// Recently settled keys. Bounded by `tombstones`; older replays are
    /// already fenced by `cursor`.
    #[serde(default)]
    pub published: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub(crate) tombstones: std::collections::VecDeque<String>,
    /// Delivery id of each outbound record, for publication order.
    #[serde(default)]
    pub(crate) outbound_order: BTreeMap<String, i64>,
    #[serde(default)]
    pub(crate) media_attempts: BTreeMap<String, u32>,
}

impl State {
    /// Unsettled outbound keys in delivery order (legacy records first).
    pub(crate) fn pending_in_delivery_order(&self) -> Vec<String> {
        let mut keys: Vec<&String> = self
            .outbound
            .keys()
            .filter(|key| !self.published.contains(*key))
            .collect();
        keys.sort_by_key(|key| (self.outbound_order.get(*key).copied().unwrap_or(0), *key));
        keys.into_iter().cloned().collect()
    }
}

/// Settled keys remembered to absorb a gateway's duplicate terminal.
const MAX_TOMBSTONES: usize = 256;

#[derive(Serialize, Deserialize)]
pub(crate) struct Group {
    turn: String,
    parts: usize,
    text: Vec<String>,
}

pub(crate) fn path(dir: &Path, sid: &str) -> PathBuf {
    dir.join(format!(
        "attachment-{}.json",
        hex::encode(Sha256::digest(sid.as_bytes()))
    ))
}

pub(crate) fn load(dir: &Path, sid: &str) -> Result<State, AcpError> {
    match std::fs::read(path(dir, sid)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn save(dir: &Path, sid: &str, state: &State) -> Result<(), AcpError> {
    std::fs::create_dir_all(dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path(dir, sid).with_extension("lock"))?;
    let _lock = lock_state(lock)?;
    let previous = load(dir, sid)?;
    if previous.revision != state.revision {
        return Err(invalid("stale attachment state; reload before retry"));
    }
    let mut value = serde_json::to_value(state)?;
    // Bind each new durable record before advancing its cursor. A later
    // foreground trigger must never retarget an older pending publication.
    for key in state.outbound.keys() {
        if !state.outbound_routes.contains_key(key) && !previous.outbound.contains_key(key) {
            if let Some(route) = super::load(dir, sid) {
                value["outbound_routes"][key] = serde_json::to_value(route)?;
            }
        }
    }
    value["revision"] = state
        .revision
        .checked_add(1)
        .ok_or_else(|| invalid("revision overflow"))?
        .into();
    let bytes = serde_json::to_vec(&value)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(AcpError::Protocol(
            "attachment consumer capacity: operator archival required".into(),
        ));
    }
    super::atomic_write(dir, &path(dir, sid), &bytes)?;
    Ok(())
}

pub(crate) fn accept(dir: &Path, msg: &Value) -> Result<(), AcpError> {
    accept_frame(dir, msg)
}

#[cfg(unix)]
fn lock_state(file: std::fs::File) -> Result<nix::fcntl::Flock<std::fs::File>, AcpError> {
    nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
        .map_err(|(_, e)| invalid(&format!("attachment writer busy: {e}")))
}

#[cfg(not(unix))]
fn lock_state(_file: std::fs::File) -> Result<std::fs::File, AcpError> {
    Err(invalid(
        "canonical attachment requires platform file locking; native ACP remains available",
    ))
}

fn accept_frame(dir: &Path, msg: &Value) -> Result<(), AcpError> {
    let p = &msg["params"];
    let Some(id) = p.pointer("/_meta/deliveryId").and_then(Value::as_i64) else {
        return Ok(());
    };
    let sid = p["sessionId"]
        .as_str()
        .ok_or_else(|| invalid("missing session identity"))?;
    let mut state = load(dir, sid)?;
    if id <= state.cursor {
        return Ok(());
    }
    match msg["method"].as_str() {
        Some("session/update") => {
            let meta = &p["_meta"];
            if meta["kind"] == "final" {
                if meta["operation"] != "replace" {
                    return Err(invalid("final is not replacement"));
                }
                let key = meta["messageId"]
                    .as_str()
                    .ok_or_else(|| invalid("missing message identity"))?;
                let turn = meta["turnId"]
                    .as_str()
                    .ok_or_else(|| invalid("missing turn identity"))?;
                let part = meta["part"]
                    .as_u64()
                    .ok_or_else(|| invalid("missing part"))? as usize;
                let parts = meta["parts"]
                    .as_u64()
                    .ok_or_else(|| invalid("missing parts"))? as usize;
                if parts == 0 || parts > 128 || part >= parts {
                    return Err(invalid("invalid part bounds"));
                }
                if part == 0 {
                    state.groups.insert(
                        key.into(),
                        Group {
                            turn: turn.into(),
                            parts,
                            text: Vec::new(),
                        },
                    );
                }
                let group = state
                    .groups
                    .get_mut(key)
                    .ok_or_else(|| invalid("incomplete final group"))?;
                if group.turn != turn || group.parts != parts || group.text.len() != part {
                    return Err(invalid("inconsistent final group"));
                }
                group.text.push(
                    p.pointer("/update/content/text")
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid("missing final text"))?
                        .into(),
                );
                if group.text.iter().map(String::len).sum::<usize>() > 1024 * 1024 {
                    return Err(invalid("final too large"));
                }
                if group.text.len() == parts {
                    state.finals.insert(turn.into(), group.text.concat());
                    state.groups.remove(key);
                }
            } else if meta["kind"] != "history" && meta["kind"] != "snapshot" {
                // Only message text is a reply. Other durable telemetry (tool
                // calls, plans) is consumed so it can never wedge the cursor.
                match p.pointer("/update/content/text").and_then(Value::as_str) {
                    Some(text) => enqueue(&mut state, format!("notice:{id}"), text.into(), id),
                    None if p["update"]["sessionUpdate"] == "agent_message_chunk" => enqueue(
                        &mut state,
                        format!("notice:{id}"),
                        "The agent sent a message Buzz cannot display.".into(),
                        id,
                    ),
                    None => tracing::debug!(id, "durable non-message update consumed"),
                }
            }
        }
        Some("_hermes/turn_complete") => {
            let turn = p["turnId"]
                .as_str()
                .ok_or_else(|| invalid("missing terminal identity"))?;
            if state.published.contains(&format!("turn:{turn}")) {
                // Already settled: a duplicate terminal must not republish.
                state.cursor = id;
                return save(dir, sid, &state);
            }
            state.terminals.insert(turn.into(), p.clone());
            // An unfinished final can never complete once its turn is terminal.
            state.groups.retain(|_, group| group.turn != turn);
            let text = terminal_text(p, state.finals.get(turn).map(String::as_str));
            enqueue(&mut state, format!("turn:{turn}"), text, id);
        }
        _ => return Err(invalid("unexpected durable frame")),
    }
    state.cursor = id;
    save(dir, sid, &state)
}

fn enqueue(state: &mut State, key: String, text: String, id: i64) {
    if !state.outbound.contains_key(&key) {
        state.outbound_order.insert(key.clone(), id);
        state.outbound.insert(key, text);
    }
}

/// Every terminal is a user-visible outcome. Unknown or final-less outcomes
/// get a generic notice rather than an error that would wedge the cursor.
fn terminal_text(p: &Value, fin: Option<&str>) -> String {
    if let Some(error) = p.get("error") {
        return format!("Canonical turn failed: {error}");
    }
    let reason = p["stopReason"].as_str().map(str::to_ascii_lowercase);
    let stop = match reason.as_deref() {
        Some("end_turn") => {
            return fin.map_or_else(
                || "The agent finished this turn without a reply.".into(),
                str::to_owned,
            )
        }
        Some("cancelled") => return "Canonical turn cancelled.".into(),
        Some("max_tokens") => "the agent reached its output limit".into(),
        Some("max_turn_requests") => "the agent reached its per-turn request limit".into(),
        Some("refusal") => "the agent declined to continue".into(),
        Some(other) => format!("the turn ended with an unrecognized outcome ({other})"),
        None => "the gateway reported no outcome".into(),
    };
    tracing::warn!(stop, "canonical turn ended without a normal completion");
    match fin {
        Some(text) => format!("{text}\n\n(Reply stopped: {stop}.)"),
        None => format!("Reply stopped: {stop}."),
    }
}

pub(crate) fn invalid(message: &str) -> AcpError {
    AcpError::Protocol(format!("Hermes attachment: {message}"))
}

pub(crate) fn prepare_publication(
    dir: &Path,
    sid: &str,
    key: &str,
    keys: &nostr::Keys,
) -> Result<Option<nostr::Event>, AcpError> {
    let mut state = load(dir, sid)?;
    if state.published.contains(key) {
        return Ok(None);
    }
    if let Some(event) = state.publications.get(key) {
        return Ok(Some(event.clone()));
    }
    let text = state
        .outbound
        .get(key)
        .ok_or_else(|| invalid("missing outbound record"))?;
    let text = crate::media_publish::text_without_media_directives(text);
    if text.trim().is_empty() {
        settle(&mut state, key);
        save(dir, sid, &state)?;
        return Ok(None);
    }
    let route = publication_route(dir, sid, key)?;
    let tags = crate::pool::harness_reply_thread_tags(route.scope.channel_id(), &route.trigger);
    let thread = tags
        .root_event_id
        .as_deref()
        .map(|root| {
            let id = nostr::EventId::from_hex(root).map_err(|e| invalid(&e.to_string()))?;
            Ok::<_, AcpError>(buzz_sdk::ThreadRef {
                root_event_id: id,
                parent_event_id: id,
            })
        })
        .transpose()?;
    let event = buzz_sdk::build_message(
        route.scope.channel_id(),
        &text,
        thread.as_ref(),
        &[],
        false,
        &[],
        &[],
    )
    .map_err(|e| invalid(&e.to_string()))?
    .sign_with_keys(keys)
    .map_err(|e| invalid(&e.to_string()))?;
    state.publications.insert(key.into(), event.clone());
    save(dir, sid, &state)?;
    Ok(Some(event))
}

pub(crate) fn publication_route(
    dir: &Path,
    sid: &str,
    key: &str,
) -> Result<super::Route, AcpError> {
    load(dir, sid)?
        .outbound_routes
        .remove(key)
        .ok_or_else(|| invalid("unbound publication route; reconciliation required"))
}

pub(crate) fn ack_publication(dir: &Path, sid: &str, key: &str) -> Result<(), AcpError> {
    let mut state = load(dir, sid)?;
    if !state.publications.contains_key(key) {
        return Err(invalid("publication not prepared"));
    }
    if key.starts_with("media:") {
        // The parent record still needs its route; it settles both keys.
        state.published.insert(key.into());
        state.publications.remove(key);
    } else {
        settle(&mut state, key);
    }
    save(dir, sid, &state)
}

/// Drop everything a settled record owned, leaving a bounded tombstone.
///
/// Replays of settled deliveries are impossible (their ids are at or below
/// the cursor), so the tombstone only absorbs a duplicate terminal for a
/// recent turn; evicting old ones cannot resurrect a replayed record.
pub(crate) fn settle(state: &mut State, key: &str) {
    state.outbound.remove(key);
    state.outbound_order.remove(key);
    state.media_attempts.remove(key);
    state.outbound_routes.remove(key);
    state.publications.remove(key);
    let media = format!("media:{key}");
    state.publications.remove(&media);
    state.published.remove(&media);
    if let Some(turn) = key.strip_prefix("turn:") {
        state.finals.remove(turn);
        state.admissions.retain(|_, a| a["turnId"] != turn);
    }
    if state.published.insert(key.into()) {
        state.tombstones.push_back(key.into());
    }
    while state.tombstones.len() > MAX_TOMBSTONES {
        let Some(old) = state.tombstones.pop_front() else {
            break;
        };
        state.published.remove(&old);
        if let Some(turn) = old.strip_prefix("turn:") {
            state.terminals.remove(turn);
        }
    }
}

/// Drop a record that can never publish, after the caller logs and observes it.
pub(crate) fn abandon(dir: &Path, sid: &str, key: &str) -> Result<(), AcpError> {
    let mut state = load(dir, sid)?;
    settle(&mut state, key);
    save(dir, sid, &state)
}

/// Count a failed media attempt. The last allowed one rewrites the record to
/// its text plus the failure notice, so the reply still reaches its thread.
pub(crate) fn media_failed(
    dir: &Path,
    sid: &str,
    key: &str,
    notice: &str,
    max: u32,
) -> Result<bool, AcpError> {
    let mut state = load(dir, sid)?;
    let attempts = state.media_attempts.entry(key.into()).or_insert(0);
    *attempts = attempts.saturating_add(1);
    let exhausted = *attempts >= max;
    if exhausted {
        state.media_attempts.remove(key);
        if let Some(text) = state.outbound.get_mut(key) {
            let body = crate::media_publish::text_without_media_directives(text);
            *text = format!("{}\n\n{notice}", body.trim_end())
                .trim_start()
                .to_owned();
        }
    }
    save(dir, sid, &state)?;
    Ok(exhausted)
}
