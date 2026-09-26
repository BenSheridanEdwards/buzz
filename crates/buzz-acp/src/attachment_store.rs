//! Durable canonical ACP consumption. Cursor and staged replacements commit together.
use crate::acp::AcpError;
#[path = "attachment_publication.rs"]
mod publication;
pub(crate) use publication::Publication;
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
    #[serde(default)]
    pub published: std::collections::BTreeSet<String>,
}

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
                let text = p
                    .pointer("/update/content/text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("unsupported durable notice"))?;
                state.outbound.insert(format!("notice:{id}"), text.into());
            }
        }
        Some("_hermes/turn_complete") => {
            let turn = p["turnId"]
                .as_str()
                .ok_or_else(|| invalid("missing terminal identity"))?;
            state.terminals.insert(turn.into(), p.clone());
            let text = if let Some(error) = p.get("error") {
                format!("Canonical turn failed: {error}")
            } else if p["stopReason"] == "cancelled" {
                "Canonical turn cancelled.".into()
            } else if p["stopReason"] == "end_turn" {
                state
                    .finals
                    .get(turn)
                    .cloned()
                    .ok_or_else(|| invalid("terminal without complete authoritative final"))?
            } else {
                return Err(invalid("unsupported terminal outcome"));
            };
            state.outbound.entry(format!("turn:{turn}")).or_insert(text);
        }
        _ => return Err(invalid("unexpected durable frame")),
    }
    state.cursor = id;
    save(dir, sid, &state)
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
        state.published.insert(key.into());
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
    state.published.insert(key.into());
    save(dir, sid, &state)
}
