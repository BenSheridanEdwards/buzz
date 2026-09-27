//! Durable return addresses for Hermes background work. No credentials are stored.
use crate::scope::SessionScope;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
#[path = "attachment_store.rs"]
pub(crate) mod attachment;

pub(crate) fn atomic_write(dir: &Path, dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let tmp = dest.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(tmp, dest)?;
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Route {
    pub session_id: String,
    pub scope: SessionScope,
    pub trigger: nostr::Event,
}

pub(crate) fn directory(home: &Path, relay: &str, pubkey: &str) -> PathBuf {
    let key = format!("{relay}\n{pubkey}");
    home.join("cache/buzz-return-routes")
        .join(hex::encode(Sha256::digest(key.as_bytes())))
}

fn path(dir: &Path, session: &str) -> PathBuf {
    dir.join(format!(
        "{}.json",
        hex::encode(Sha256::digest(session.as_bytes()))
    ))
}

pub(crate) fn save(
    dir: &Path,
    session: &str,
    scope: &SessionScope,
    trigger: &nostr::Event,
) -> std::io::Result<()> {
    let route = Route {
        session_id: session.into(),
        scope: scope.clone(),
        trigger: trigger.clone(),
    };
    // Retain one route per provider session. Its mtime is the last parent turn,
    // not the worker's completion time: age-based cleanup can delete the only
    // return address for a long-running worker or a newly completed result.
    // Hermes' marker is best-effort and omits running origins, so its absence
    // cannot authorize deletion either. Retire routes only with authoritative
    // session-lifecycle evidence, not an independent wall-clock horizon.
    //
    // A unique, fsynced temp file: concurrent saves of one session in this
    // process must never share (and truncate/rename) each other's temp file.
    atomic_write(dir, &path(dir, session), &serde_json::to_vec(&route)?)
}

pub(crate) fn load(dir: &Path, session: &str) -> Option<Route> {
    let route: Route = serde_json::from_slice(&std::fs::read(path(dir, session)).ok()?).ok()?;
    (route.session_id == session).then_some(route)
}

/// Routes whose session has canonical attachment state. An unreadable route
/// file is logged and skipped: one bad entry must not block every scope.
pub(crate) fn canonical_routes(dir: &Path) -> Result<Vec<Route>, crate::acp::AcpError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut routes = Vec::new();
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(error) => {
                tracing::warn!(%error, "skipping unreadable return-route entry");
                continue;
            }
        };
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.len() != 69 || !name.ends_with(".json") {
            continue;
        }
        let route = std::fs::read(&path)
            .map_err(crate::acp::AcpError::from)
            .and_then(|bytes| Ok(serde_json::from_slice::<Route>(&bytes)?));
        match route {
            Ok(route) if attachment::path(dir, &route.session_id).exists() => routes.push(route),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "skipping unreadable return route");
            }
        }
    }
    Ok(routes)
}

/// How long a settled canonical session stays eligible for slow re-polling
/// after its last parent turn or durable frame.
pub(crate) const SETTLED_POLL_HORIZON: std::time::Duration =
    std::time::Duration::from_secs(7 * 24 * 3600);

/// Canonical sessions to observe during recovery.
#[derive(Default)]
pub(crate) struct RecoveryOrigins {
    /// An active gateway turn or an outbound record not yet published.
    /// Unreadable attachment state is included so its failure surfaces
    /// through the bounded recovery retries instead of silently.
    pub(crate) unfinished: Vec<String>,
    /// Nothing outstanding locally, but the gateway may still wake them
    /// (for example a multi-day task finishing after a harness restart).
    /// Bounded to sessions touched within [`SETTLED_POLL_HORIZON`] of `now`.
    pub(crate) settled: Vec<String>,
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

pub(crate) fn canonical_recovery_origins(
    dir: &Path,
    now: std::time::SystemTime,
) -> RecoveryOrigins {
    let routes = match canonical_routes(dir) {
        Ok(routes) => routes,
        Err(error) => {
            tracing::error!(%error, "canonical routes require reconciliation");
            return RecoveryOrigins::default();
        }
    };
    let mut origins = RecoveryOrigins::default();
    for route in routes {
        let sid = route.session_id;
        let unfinished = match attachment::load(dir, &sid) {
            Ok(state) => {
                state.active_turn.is_some()
                    || state
                        .outbound
                        .keys()
                        .any(|key| !state.published.contains(key))
            }
            Err(_) => true,
        };
        if unfinished {
            origins.unfinished.push(sid);
            continue;
        }
        let touched = [path(dir, &sid), attachment::path(dir, &sid)]
            .iter()
            .filter_map(|p| modified(p))
            .max();
        let recent = touched.is_some_and(|touched| {
            now.duration_since(touched)
                .map_or(true, |age| age <= SETTLED_POLL_HORIZON)
        });
        if recent {
            origins.settled.push(sid);
        }
    }
    origins
}

pub(crate) fn canonical_scope(
    dir: &Path,
    scope: &SessionScope,
) -> Result<Option<Route>, crate::acp::AcpError> {
    let mut routes = canonical_routes(dir)?
        .into_iter()
        .filter(|r| &r.scope == scope);
    let first = routes.next();
    if routes.next().is_some() {
        return Err(attachment::invalid(
            "multiple canonical sessions for scope; reconcile explicitly",
        ));
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persisted_route_survives_restart_and_is_tenant_scoped() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = nostr::Keys::generate();
        let event = nostr::EventBuilder::text_note("work")
            .sign_with_keys(&keys)
            .unwrap();
        let scope = SessionScope::Conversation {
            channel_id: uuid::Uuid::new_v4(),
        };
        let dir = directory(tmp.path(), "wss://one", "agent");
        save(&dir, "session", &scope, &event).unwrap();
        let route = load(&dir, "session").unwrap();
        assert_eq!(route.scope, scope);
        assert_eq!(route.trigger.id, event.id);
        assert!(load(&directory(tmp.path(), "wss://two", "agent"), "session").is_none());
        assert!(load(&directory(tmp.path(), "wss://one", "other"), "session").is_none());
        assert!(load(&dir, "../session").is_none());
    }
    fn event() -> nostr::Event {
        nostr::EventBuilder::text_note("work")
            .sign_with_keys(&nostr::Keys::generate())
            .unwrap()
    }

    #[test]
    fn concurrent_saves_of_one_session_never_share_a_temp_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("routes");
        let scope = SessionScope::Conversation {
            channel_id: uuid::Uuid::new_v4(),
        };
        let trigger = event();
        let failures = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|threads| {
            for _ in 0..8 {
                threads.spawn(|| {
                    for _ in 0..100 {
                        if save(&dir, "session", &scope, &trigger).is_err() {
                            failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        assert_eq!(failures.into_inner(), 0, "a concurrent save failed");
        assert_eq!(load(&dir, "session").unwrap().trigger.id, trigger.id);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files leaked: {leftovers:?}");
    }

    #[test]
    fn one_corrupt_route_does_not_block_other_canonical_scopes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("routes");
        let scope = SessionScope::Conversation {
            channel_id: uuid::Uuid::new_v4(),
        };
        save(&dir, "good", &scope, &event()).unwrap();
        attachment::save(&dir, "good", &Default::default()).unwrap();
        std::fs::write(path(&dir, "corrupt"), b"{not json").unwrap();
        assert_eq!(canonical_routes(&dir).unwrap().len(), 1);
        assert_eq!(
            canonical_scope(&dir, &scope).unwrap().unwrap().session_id,
            "good"
        );
    }

    #[test]
    fn recovery_origins_are_routes_with_unfinished_canonical_work() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("routes");
        let scope = SessionScope::Conversation {
            channel_id: uuid::Uuid::new_v4(),
        };
        // (session, active turn, outbound record, published)
        let cases: [(&str, Option<&str>, bool, bool); 5] = [
            ("idle", None, false, false),
            ("settled", None, true, true),
            ("active", Some("turn"), false, false),
            ("unpublished", None, true, false),
            ("active-unpublished", Some("turn"), true, false),
        ];
        for (sid, active, outbound, published) in cases {
            save(&dir, sid, &scope, &event()).unwrap();
            let mut state = attachment::State {
                active_turn: active.map(str::to_owned),
                ..Default::default()
            };
            if outbound {
                state.outbound.insert("turn:t".into(), "text".into());
            }
            if published {
                state.published.insert("turn:t".into());
            }
            attachment::save(&dir, sid, &state).unwrap();
        }
        // A route with no attachment state is not canonical at all.
        save(&dir, "native", &scope, &event()).unwrap();
        // A settled session untouched beyond the horizon is no longer polled.
        save(&dir, "aged", &scope, &event()).unwrap();
        attachment::save(&dir, "aged", &Default::default()).unwrap();
        let now = std::time::SystemTime::now();
        let old = now - SETTLED_POLL_HORIZON - std::time::Duration::from_secs(60);
        for file in [path(&dir, "aged"), attachment::path(&dir, "aged")] {
            std::fs::File::options()
                .write(true)
                .open(file)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
        let mut origins = canonical_recovery_origins(&dir, now);
        origins.unfinished.sort();
        origins.settled.sort();
        assert_eq!(
            origins.unfinished,
            ["active", "active-unpublished", "unpublished"]
        );
        assert_eq!(origins.settled, ["idle", "settled"]);
    }

    #[test]
    fn saving_another_session_retains_aged_worker_return_address() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = nostr::Keys::generate();
        let event = nostr::EventBuilder::text_note("long-running work")
            .sign_with_keys(&keys)
            .unwrap();
        let scope = SessionScope::Conversation {
            channel_id: uuid::Uuid::new_v4(),
        };
        let dir = directory(tmp.path(), "wss://one", "agent");
        save(&dir, "running-origin", &scope, &event).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(72 * 3600);
        std::fs::File::options()
            .write(true)
            .open(path(&dir, "running-origin"))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();

        // A different conversation becomes active after restart. The old
        // worker may have just completed, so its completion replay window has
        // only just begun even though the last parent turn was three days ago.
        save(&dir, "new-session", &scope, &event).unwrap();
        let route = load(&dir, "running-origin").unwrap();
        assert_eq!(route.scope, scope);
        assert_eq!(route.trigger.id, event.id);
        assert!(load(&dir, "new-session").is_some());
    }
}
