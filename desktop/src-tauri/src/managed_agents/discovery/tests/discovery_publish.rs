//! Discovery publish via the PRODUCTION call path (stale-snapshot regression).
//!
//! These drive `discover_acp_runtimes_from` itself and land a save/delete in
//! the window between its directory scan and its registry publish (via the
//! `pre_publish_test_hook` seam). They red if discovery's final line reverts
//! to publishing its pre-probe `loaded_defs` snapshot — the original bug —
//! unlike the `custom_harnesses` seam tests, which pin only the fresh-read
//! contract of `warm_harness_registry_locked`.

use super::super::pre_publish_test_hook;

/// RAII guard: installs the pre-publish hook, clears it on drop (even on
/// panic) so a failing test cannot poison later ones.
struct PrePublishHookGuard;

impl PrePublishHookGuard {
    fn install(hook: Box<dyn Fn() + Send>) -> Self {
        pre_publish_test_hook::set(Some(hook));
        PrePublishHookGuard
    }
}

impl Drop for PrePublishHookGuard {
    fn drop(&mut self) {
        pre_publish_test_hook::set(None);
    }
}

fn harness_def(
    id: &str,
    label: &str,
    command: &str,
) -> crate::managed_agents::custom_harnesses::HarnessDefinition {
    crate::managed_agents::custom_harnesses::HarnessDefinition {
        id: id.to_string(),
        label: label.to_string(),
        command: command.to_string(),
        args: vec![],
        env: Default::default(),
        install_instructions_url: String::new(),
        install_hint: String::new(),
    }
}

/// A `save_and_warm` landing mid-discovery (after the scan, before the
/// publish) must survive discovery's registry publish — through the real
/// `discover_acp_runtimes_from` path.
#[test]
fn discovery_publish_path_survives_mid_flight_save() {
    use crate::managed_agents::custom_harnesses::{
        lookup_loaded_harness_by_id, registry_test_lock, save_and_warm,
    };
    use crate::managed_agents::discovery::discover_acp_runtimes_from;

    // Path lock first, then registry — discovery's probes touch the global
    // PATH caches (see the definition_env tests in `tests.rs` for the rationale).
    let _path_guard = crate::managed_agents::lock_path_mutex();
    let _lock = registry_test_lock();
    let dir = tempfile::tempdir().unwrap();

    // Discovery scans the dir while it is EMPTY; the save lands in the
    // pre-publish window. A stale-snapshot publish would clobber it.
    let hook_dir = dir.path().to_path_buf();
    let _guard = PrePublishHookGuard::install(Box::new(move || {
        let def = harness_def("mid-flight-save", "Mid Flight", "mid-flight-bin");
        save_and_warm(&hook_dir, &def, None).unwrap();
        assert!(lookup_loaded_harness_by_id("mid-flight-save").is_some());
    }));

    let _entries = discover_acp_runtimes_from(Some(dir.path()), true);

    assert!(
        lookup_loaded_harness_by_id("mid-flight-save").is_some(),
        "discovery's publish must re-read the directory — a stale-snapshot \
         publish clobbers a save that landed mid-discovery"
    );
}
/// A `delete_and_warm` landing mid-discovery must stay gone after discovery's
/// publish — a stale snapshot (taken while the file existed) would resurrect it.
#[test]
fn discovery_publish_path_drops_mid_flight_delete() {
    use crate::managed_agents::custom_harnesses::{
        delete_and_warm, lookup_loaded_harness_by_id, registry_test_lock, save_and_warm,
    };
    use crate::managed_agents::discovery::discover_acp_runtimes_from;

    // Path lock first, then registry (same rationale as the sibling test).
    let _path_guard = crate::managed_agents::lock_path_mutex();
    let _lock = registry_test_lock();
    let dir = tempfile::tempdir().unwrap();

    // File exists at scan time — discovery's snapshot would contain it.
    let def = harness_def("mid-flight-delete", "Mid Flight Del", "mid-flight-del-bin");
    save_and_warm(dir.path(), &def, None).unwrap();

    let hook_dir = dir.path().to_path_buf();
    let _guard = PrePublishHookGuard::install(Box::new(move || {
        delete_and_warm(&hook_dir, "mid-flight-delete").unwrap();
        assert!(lookup_loaded_harness_by_id("mid-flight-delete").is_none());
    }));

    let _entries = discover_acp_runtimes_from(Some(dir.path()), true);

    assert!(
        lookup_loaded_harness_by_id("mid-flight-delete").is_none(),
        "discovery's publish must not resurrect a harness deleted mid-discovery"
    );
}
