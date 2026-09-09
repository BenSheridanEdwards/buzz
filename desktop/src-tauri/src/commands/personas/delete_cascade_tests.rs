//! Tests for cascade-delete filtering in `delete_persona`.
//!
//! `delete_persona` deletes all managed-agent records whose `persona_id`
//! matches the persona being deleted, mirroring the cleanup done by
//! `delete_managed_agent`. These tests verify the `collect_cascade_pubkeys`
//! helper that identifies the agents to cascade-delete, using plain
//! in-memory data structures (no `AppHandle` required).

use super::{collect_cascade_pubkeys, collect_remote_deployed, commit_cascade_agents};
use crate::managed_agents::{BackendKind, ManagedAgentRecord, RespondTo};
use std::collections::BTreeMap;
use std::collections::HashSet;

fn make_agent(
    pubkey: &str,
    persona_id: Option<&str>,
    runtime_pid: Option<u32>,
) -> ManagedAgentRecord {
    ManagedAgentRecord {
        description: None,
        pubkey: pubkey.to_string(),
        name: "Test Agent".to_string(),
        persona_id: persona_id.map(str::to_string),
        private_key_nsec: "".to_string(),
        auth_tag: None,
        relay_url: "ws://localhost:3000".to_string(),
        avatar_url: None,
        acp_command: "buzz-acp".to_string(),
        agent_command: "buzz-agent".to_string(),
        agent_command_override: None,
        agent_args: vec![],
        mcp_command: "".to_string(),
        turn_timeout_seconds: 300,
        idle_timeout_seconds: None,
        max_turn_duration_seconds: None,
        parallelism: 1,
        system_prompt: None,
        model: None,
        provider: None,
        persona_source_version: None,
        env_vars: BTreeMap::new(),
        start_on_app_launch: false,
        runtime_pid,
        backend: BackendKind::Local,
        backend_agent_id: None,
        provider_policy_pending: false,
        provider_binary_path: None,
        team_id: None,
        persona_team_dir: None,
        persona_name_in_team: None,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
        last_started_at: None,
        last_stopped_at: None,
        last_exit_code: None,
        last_error: None,
        last_error_code: None,
        respond_to: RespondTo::OwnerOnly,
        respond_to_allowlist: vec![],
        display_name: None,
        slug: None,
        runtime: None,
        name_pool: vec![],
        is_builtin: false,
        is_active: true,
        shared: false,
        source_team: None,
        source_team_persona_slug: None,
        catalog_source: None,
        team_catalog_source: None,
        relay_mesh: None,
        effort_level: None,
        auto_restart_on_config_change: false,
        definition_respond_to: None,
        definition_respond_to_allowlist: vec![],
        definition_parallelism: None,
    }
}

const PERSONA_ID: &str = "custom:test-persona";

// The sidecar store keys on a real agent pubkey, so the tests that touch it
// use 64-hex records rather than the readable placeholders above.
const HEX_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEX_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const HEX_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const RELAY: &str = "ws://localhost:3000";

/// Deleting a persona with two linked agents (one running) returns both their
/// pubkeys and leaves unlinked agents out of the cascade set.
#[test]
fn cascade_includes_linked_agents_and_excludes_others() {
    let agents = vec![
        make_agent("agent-a", Some(PERSONA_ID), Some(12345)), // running, linked
        make_agent("agent-b", Some(PERSONA_ID), None),        // stopped, linked
        make_agent("agent-c", Some("custom:other"), None),    // different persona
        make_agent("agent-d", None, None),                    // no persona
    ];

    let pubkeys = collect_cascade_pubkeys(&agents, PERSONA_ID);

    assert_eq!(
        pubkeys.len(),
        2,
        "exactly two agents linked to this persona"
    );
    assert!(
        pubkeys.contains(&"agent-a".to_string()),
        "running linked agent included"
    );
    assert!(
        pubkeys.contains(&"agent-b".to_string()),
        "stopped linked agent included"
    );
    assert!(
        !pubkeys.contains(&"agent-c".to_string()),
        "different-persona agent excluded"
    );
    assert!(
        !pubkeys.contains(&"agent-d".to_string()),
        "persona-less agent excluded"
    );
}

/// Deleting a persona with no linked agents returns an empty list (no cascade).
#[test]
fn cascade_empty_when_no_linked_agents() {
    let agents = vec![
        make_agent("agent-x", Some("custom:other"), None),
        make_agent("agent-y", None, None),
    ];

    let pubkeys = collect_cascade_pubkeys(&agents, PERSONA_ID);

    assert!(pubkeys.is_empty(), "no agents to cascade-delete");
}

/// Cascade targets all agents linked to the persona — not just stopped ones.
/// A running agent (runtime_pid set) must appear in the cascade set so the
/// command can stop it before removing the record.
#[test]
fn cascade_includes_running_agent() {
    let agents = vec![make_agent("running-agent", Some(PERSONA_ID), Some(99999))];

    let pubkeys = collect_cascade_pubkeys(&agents, PERSONA_ID);

    assert_eq!(pubkeys, vec!["running-agent".to_string()]);
}

/// A failing agent-store save in Phase 3 must be retry-safe: the error
/// propagates before any derived-state clear, keyring deletion or tombstone
/// (the first is inside `commit_cascade_agents` itself, after the `?` on
/// `save`; the rest appear after the `?` at the `delete_persona` call site).
/// Persona records and agent records are therefore untouched on disk, so the
/// command can be retried with no cleanup.
#[test]
fn failing_save_is_retry_safe() {
    use crate::managed_agents::{
        load_relay_memberships, record_relay_membership, relay_membership_for,
        ManagedAgentRelayMembership, RelayMembershipState,
    };

    let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("temp dir: {error}"));
    record_relay_membership(
        dir.path(),
        HEX_A,
        RELAY,
        Some(ManagedAgentRelayMembership {
            state: RelayMembershipState::NotMember,
            checked_at: "t".to_string(),
            detail: None,
            subject_pubkey: None,
        }),
    )
    .unwrap_or_else(|error| panic!("seed membership: {error}"));

    let mut agents = vec![
        make_agent(HEX_A, Some(PERSONA_ID), None),
        make_agent(HEX_B, Some(PERSONA_ID), None),
        make_agent(HEX_C, Some("custom:other"), None),
    ];
    let cascade: HashSet<String> = [HEX_A.to_string(), HEX_B.to_string()].into();

    let result = commit_cascade_agents(dir.path(), &mut agents, &cascade, |_| {
        Err("simulated disk failure".to_string())
    });

    assert!(
        result.is_err(),
        "commit must propagate the save error so callers can react"
    );
    assert!(
        relay_membership_for(&load_relay_memberships(dir.path()), HEX_A, RELAY).is_some(),
        "nothing may be destroyed when the records never left disk: the \
         derived row must still be there for the retry"
    );
}

/// The persona cascade clears each removed agent's relay-membership sidecar
/// row, and only theirs.
///
/// This is the cascade's own seam, not the single delete's: `delete_persona`
/// removes agent records through `commit_cascade_agents`, and that function
/// owns the clear. The cascade used to call `forget_managed_agent_derived_state`
/// from a loop inside the `tauri::AppHandle` body instead, which no test could
/// reach, so deleting that line failed nothing and it guarded nothing. A row
/// left behind is keyed by a pubkey with no record, is trusted by the next
/// reader of `relay-membership.json`, and grows the file without bound.
#[test]
fn cascade_commit_clears_the_relay_membership_sidecar_of_every_cascaded_agent() {
    use crate::managed_agents::{
        load_relay_memberships, record_relay_membership, relay_membership_for,
        ManagedAgentRelayMembership, RelayMembershipState,
    };

    let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("temp dir: {error}"));
    for pubkey in [HEX_A, HEX_B, HEX_C] {
        record_relay_membership(
            dir.path(),
            pubkey,
            RELAY,
            Some(ManagedAgentRelayMembership {
                state: RelayMembershipState::NotMember,
                checked_at: "t".to_string(),
                detail: None,
                subject_pubkey: None,
            }),
        )
        .unwrap_or_else(|error| panic!("seed membership: {error}"));
    }

    let mut agents = vec![
        make_agent(HEX_A, Some(PERSONA_ID), None),
        make_agent(HEX_B, Some(PERSONA_ID), None),
        make_agent(HEX_C, Some("custom:other"), None),
    ];
    let cascade: HashSet<String> = collect_cascade_pubkeys(&agents, PERSONA_ID)
        .into_iter()
        .collect();

    commit_cascade_agents(dir.path(), &mut agents, &cascade, |_| Ok(()))
        .unwrap_or_else(|error| panic!("cascade commit: {error}"));

    let store = load_relay_memberships(dir.path());
    for gone in [HEX_A, HEX_B] {
        assert_eq!(
            relay_membership_for(&store, gone, RELAY),
            None,
            "cascaded agent {gone} must not leave a membership row behind"
        );
    }
    assert!(
        relay_membership_for(&store, HEX_C, RELAY).is_some(),
        "an agent outside the cascade must keep its row"
    );
}

/// A provider-deployed cascade target (non-local backend with a live
/// `backend_agent_id`) must be detected by the pre-flight so `delete_persona`
/// refuses the cascade before any destructive work. Local agents and
/// never-deployed provider agents must not block.
#[test]
fn remote_deployed_cascade_target_blocks_delete() {
    let mut deployed = make_agent("pk-deployed", Some(PERSONA_ID), None);
    deployed.name = "Deployed Agent".to_string();
    deployed.backend = BackendKind::Provider {
        id: "blox".to_string(),
        config: serde_json::Value::Null,
    };
    deployed.backend_agent_id = Some("backend-1".to_string());

    // Provider backend but never deployed (no backend_agent_id) — not a blocker.
    let mut undeployed = make_agent("pk-undeployed", Some(PERSONA_ID), None);
    undeployed.backend = BackendKind::Provider {
        id: "blox".to_string(),
        config: serde_json::Value::Null,
    };

    let agents = vec![
        make_agent("pk-local", Some(PERSONA_ID), None),
        deployed,
        undeployed,
    ];
    let cascade: HashSet<String> = collect_cascade_pubkeys(&agents, PERSONA_ID)
        .into_iter()
        .collect();
    assert_eq!(cascade.len(), 3, "all three agents are cascade targets");

    let blockers = collect_remote_deployed(&agents, &cascade);

    assert_eq!(
        blockers,
        vec!["Deployed Agent".to_string()],
        "only the deployed provider agent blocks the cascade"
    );
}
