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
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let dest = path(dir, session);
    let tmp = dest.with_extension(format!("{}.tmp", std::process::id()));
    let route = Route {
        session_id: session.into(),
        scope: scope.clone(),
        trigger: trigger.clone(),
    };
    std::fs::write(&tmp, serde_json::to_vec(&route)?)?;
    std::fs::rename(tmp, dest)?;
    // Retain one route per provider session. Its mtime is the last parent turn,
    // not the worker's completion time: age-based cleanup can delete the only
    // return address for a long-running worker or a newly completed result.
    // Hermes' marker is best-effort and omits running origins, so its absence
    // cannot authorize deletion either. Retire routes only with authoritative
    // session-lifecycle evidence, not an independent wall-clock horizon.
    Ok(())
}

pub(crate) fn load(dir: &Path, session: &str) -> Option<Route> {
    let route: Route = serde_json::from_slice(&std::fs::read(path(dir, session)).ok()?).ok()?;
    (route.session_id == session).then_some(route)
}

pub(crate) fn canonical_routes(dir: &Path) -> Result<Vec<Route>, crate::acp::AcpError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut routes = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.len() != 69 || !name.ends_with(".json") {
            continue;
        }
        let route: Route = serde_json::from_slice(&std::fs::read(&path)?)?;
        if attachment::path(dir, &route.session_id).exists() {
            routes.push(route);
        }
    }
    Ok(routes)
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
