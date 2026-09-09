//! `should_restart_after_install` is a pure predicate, so each clause gets
//! its own case.

use super::*;

/// Setup-mode agent on matching runtime that is now Ready → restart.
#[test]
fn test_should_restart_after_install_setup_mode_now_ready_is_candidate() {
    assert!(
        should_restart_after_install(true, true, true, true, true),
        "setup-mode codex agent that became Ready must be restarted after install"
    );
}

/// Setup-mode agent still NotReady after install (e.g. logged out) → no restart.
#[test]
fn test_should_restart_after_install_still_not_ready_is_not_candidate() {
    assert!(
        !should_restart_after_install(true, true, true, true, false),
        "setup-mode agent still NotReady must NOT be restarted (would re-enter setup mode)"
    );
}

/// Healthy in-pool agent (setup_mode=false) → no restart, even if now Ready.
#[test]
fn test_should_restart_after_install_healthy_agent_is_not_candidate() {
    assert!(
        !should_restart_after_install(true, true, true, false, true),
        "healthy in-pool agent (setup_mode=false) must NOT be bounced on install"
    );
}

/// Agent on a different runtime_id → no restart.
#[test]
fn test_should_restart_after_install_different_runtime_is_not_candidate() {
    assert!(
        !should_restart_after_install(true, true, false, true, true),
        "agent on a different runtime must NOT be restarted by this install"
    );
}

/// Remote/provider-backend agent → no restart (not local).
#[test]
fn test_should_restart_after_install_non_local_is_not_candidate() {
    assert!(
        !should_restart_after_install(false, true, true, true, true),
        "non-local (provider-backend) agent must NOT be restarted"
    );
}

/// Dead process (pid_alive=false) → no restart.
#[test]
fn test_should_restart_after_install_dead_pid_is_not_candidate() {
    assert!(
        !should_restart_after_install(true, false, true, true, true),
        "agent whose process is no longer running must NOT be restarted"
    );
}
