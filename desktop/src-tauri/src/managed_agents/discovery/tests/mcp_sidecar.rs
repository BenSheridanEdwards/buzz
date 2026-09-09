//! The bundled MCP sidecar: what a harness is configured for, what actually
//! resolves on this machine, and the memo that keeps the two from flapping
//! apart across a resolve-cache clear.

use std::path::PathBuf;

use super::super::{
    attached_mcp_command_with, clear_resolve_cache, clear_sidecar_path_memo, mcp_sidecar_with,
    sticky_sidecar_path,
};

/// `mcp_sidecar_with` reports the catalog name and, separately, whether the
/// binary behind it resolves. The two are distinct facts: a harness stays
/// "configured for buzz-dev-mcp" on a machine where that binary is missing,
/// and the spawn there starts without it.
#[test]
fn mcp_sidecar_separates_configured_from_resolvable() {
    let present = |_: &str| Some(PathBuf::from("/Applications/Buzz.app/Contents/MacOS/found"));
    let absent = |_: &str| None;

    assert_eq!(
        mcp_sidecar_with("codex-acp", present),
        Some((
            "buzz-dev-mcp",
            Some(PathBuf::from("/Applications/Buzz.app/Contents/MacOS/found"))
        )),
        "a resolvable sidecar reports its path"
    );
    assert_eq!(
        mcp_sidecar_with("codex-acp", absent),
        Some(("buzz-dev-mcp", None)),
        "a missing binary keeps the configured name but resolves to nothing"
    );
    assert_eq!(
        mcp_sidecar_with("goose", present),
        None,
        "a harness with no sidecar never consults the resolver"
    );
    assert_eq!(mcp_sidecar_with("/opt/custom/my-agent", present), None);
    assert_eq!(mcp_sidecar_with("", present), None);
}

/// The value the summary and the restart snapshot publish is empty whenever
/// the sidecar did not resolve, so the UI never reports a sidecar the agent
/// did not actually get, and the restart diff sees drift once it appears.
#[test]
fn attached_mcp_command_is_empty_when_the_sidecar_is_missing() {
    assert_eq!(
        attached_mcp_command_with("codex-acp", |_| Some(PathBuf::from(
            "/usr/bin/buzz-dev-mcp"
        ))),
        "buzz-dev-mcp"
    );
    assert_eq!(
        attached_mcp_command_with("codex-acp", |_| None),
        "",
        "a sidecar that cannot be launched must not be reported as attached"
    );
    assert_eq!(
        attached_mcp_command_with("buzz-agent", |_| None),
        "",
        "the bundled agent is not exempt: a missing sidecar is a missing sidecar"
    );
    assert_eq!(
        attached_mcp_command_with("goose", |_| Some(PathBuf::from("/usr/bin/x"))),
        ""
    );
}

// ── Sidecar memo: the restart badge must not flap on a cache clear ───────────

/// Serialises the tests that inject a resolver into the process-global sidecar
/// memo, so one test's answer cannot leak into another's.
fn sidecar_memo_test_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// A sidecar that only the spawn's full resolver could find stays visible to
/// the cheap read paths after a forced discovery clears the resolve cache.
///
/// Without the memo the stamped snapshot says `buzz-dev-mcp` and the
/// prospective one says `""`, and every running codex and buzz-agent agent
/// wears a "restart required" badge for a change that never happened.
#[test]
fn sidecar_path_survives_a_resolve_cache_clear() {
    let _lock = sidecar_memo_test_lock();
    clear_sidecar_path_memo();

    // A real file, so the memo's liveness re-check passes on reuse.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("buzz-dev-mcp");
    std::fs::write(&path, b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // The spawn resolves it once (this stands in for the login-shell probe).
    assert_eq!(
        sticky_sidecar_path("buzz-dev-mcp", |_| Some(path.clone())),
        Some(path.clone())
    );

    // A forced discovery empties the resolve cache; the read path, which only
    // consults that cache, would now find nothing.
    clear_resolve_cache();
    assert_eq!(
        sticky_sidecar_path("buzz-dev-mcp", |_| None),
        Some(path.clone()),
        "a remembered sidecar must survive the cache clear"
    );
    assert_eq!(
        attached_mcp_command_with("codex-acp", |name| sticky_sidecar_path(name, |_| None)),
        "buzz-dev-mcp",
        "so the prospective snapshot still agrees with the stamped one"
    );

    clear_sidecar_path_memo();
}

/// Absence is never memoized, so the badge still fires on the day the sidecar
/// is installed; and a remembered path that disappears is evicted rather than
/// handed to a spawn as a dead path.
#[test]
fn sidecar_memo_holds_only_live_paths() {
    let _lock = sidecar_memo_test_lock();
    clear_sidecar_path_memo();

    // Absent stays absent, and re-probes rather than sticking.
    assert_eq!(sticky_sidecar_path("buzz-dev-mcp", |_| None), None);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("buzz-dev-mcp");
    std::fs::write(&path, b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(
        sticky_sidecar_path("buzz-dev-mcp", |_| Some(path.clone())),
        Some(path.clone()),
        "a sidecar installed after a negative answer must still be found"
    );

    // A remembered path that is deleted is evicted, not returned.
    drop(dir);
    assert_eq!(
        sticky_sidecar_path("buzz-dev-mcp", |_| None),
        None,
        "a vanished sidecar must not be handed to a spawn"
    );

    clear_sidecar_path_memo();
}
