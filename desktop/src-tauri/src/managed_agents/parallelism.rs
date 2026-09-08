// ── Per-harness parallelism cap ───────────────────────────────────────────────
//
// Contract: stored = requested; effective = min(requested, harness cap).
//
// `ManagedAgentRecord.parallelism` stores the user's requested value verbatim,
// never clamped at persistence. The cap is applied only where the value
// becomes a running worker-pool size:
//
//   * local spawn  — `BUZZ_ACP_AGENTS` in the child environment
//   * remote deploy — `launch.policy_env["BUZZ_ACP_AGENTS"]` + legacy field
//   * restart hash  — `SpawnConfigSnapshot` stores the effective value
//   * display copy  — the UI derives effective for explanatory hints only
//
// `AgentDefinition.parallelism` is the portable requested value, unchanged
// at every boundary so it travels across devices and harness switches intact.

/// Maximum parallelism for the OpenClaw harness.
///
/// Each buzz-acp worker spawned by the Desktop is a client of the single
/// shared OpenClaw Gateway daemon — running more than this number of workers
/// is both resource-expensive and architecturally wrong per the OpenClaw
/// design. Tyler's ruling: "try 5 and lower if needed."
pub const OPENCLAW_MAX_PARALLELISM: u32 = 5;

/// Return the maximum allowed parallelism for the given harness command, or
/// `None` when the harness has no cap.
///
/// Keyed on [`super::discovery::normalize_command_identity`] so path prefixes,
/// the `.exe` suffix on Windows, and other cosmetic differences are ignored.
pub fn harness_max_parallelism(command: &str) -> Option<u32> {
    match super::discovery::normalize_command_identity(command).as_str() {
        "openclaw" => Some(OPENCLAW_MAX_PARALLELISM),
        _ => None,
    }
}

/// Return the parallelism a freshly minted record stores when neither the
/// create input nor the linked definition sets one.
///
/// The app-wide default is [`super::DEFAULT_AGENT_PARALLELISM`]; a preset can
/// carry its own (`PresetHarness::default_parallelism`), e.g. Hermes, where
/// every worker is a full Hermes process loading the profile's MCP servers.
/// Accepts a runtime id ("hermes") or a command ("hermes-acp", path-prefixed
/// or `.exe`-suffixed) and reads static data only, so it is safe inside
/// discovery and at the mint sites alike.
pub fn harness_default_parallelism(harness: &str) -> u32 {
    super::discovery::preset_default_parallelism(harness)
        .unwrap_or(super::DEFAULT_AGENT_PARALLELISM)
}

/// Resolve the parallelism to persist on a new record: the explicit or
/// definition-provided value when present, else the harness default.
///
/// Shared by every mint site (create, team snapshot adopt, persona snapshot
/// import) so they cannot drift on which default applies.
pub fn mint_parallelism(harness: &str, requested: Option<u32>) -> u32 {
    requested.unwrap_or_else(|| harness_default_parallelism(harness))
}

/// Return the effective parallelism for the given harness command and
/// requested value: `min(value, harness_max_parallelism(command))`.
///
/// For harnesses without a cap this is the identity function.
pub fn effective_parallelism(command: &str, value: u32) -> u32 {
    match harness_max_parallelism(command) {
        Some(cap) => value.min(cap),
        None => value,
    }
}

/// Return the value to emit as `BUZZ_ACP_AGENTS` for a spawn command.
///
/// Pure helper extracted from `spawn_agent_child` so both the production path
/// and tests can call it without spawning a process. The result is
/// `effective_parallelism(effective_command, record_parallelism)` formatted as
/// a decimal string ready for `command.env("BUZZ_ACP_AGENTS", …)`.
///
/// `effective_command` must be the already-resolved harness command (override →
/// runtime → persona runtime → default).
pub fn acp_agents_value(effective_command: &str, record_parallelism: u32) -> String {
    effective_parallelism(effective_command, record_parallelism).to_string()
}

#[cfg(test)]
mod tests {
    use crate::managed_agents::types::ManagedAgentRecord;

    fn record_with(runtime: Option<&str>, parallelism: u32) -> ManagedAgentRecord {
        ManagedAgentRecord {
            description: None,
            pubkey: String::new(),
            name: "r".to_string(),
            persona_id: None,
            private_key_nsec: String::new(),
            auth_tag: None,
            relay_url: String::new(),
            avatar_url: None,
            acp_command: String::new(),
            agent_command: String::new(),
            agent_command_override: None,
            agent_args: vec![],
            mcp_command: String::new(),
            turn_timeout_seconds: 0,
            idle_timeout_seconds: None,
            max_turn_duration_seconds: None,
            parallelism,
            system_prompt: None,
            model: None,
            provider: None,
            persona_source_version: None,
            start_on_app_launch: false,
            auto_restart_on_config_change: true,
            runtime_pid: None,
            backend: Default::default(),
            backend_agent_id: None,
            provider_policy_pending: false,
            provider_binary_path: None,
            team_id: None,
            persona_team_dir: None,
            persona_name_in_team: None,
            env_vars: std::collections::BTreeMap::new(),
            created_at: String::new(),
            updated_at: String::new(),
            last_started_at: None,
            last_stopped_at: None,
            last_exit_code: None,
            last_error: None,
            last_error_code: None,
            respond_to: Default::default(),
            respond_to_allowlist: vec![],
            display_name: None,
            slug: None,
            runtime: runtime.map(str::to_string),
            name_pool: Vec::new(),
            is_builtin: false,
            is_active: true,
            shared: false,
            source_team: None,
            source_team_persona_slug: None,
            catalog_source: None,
            team_catalog_source: None,
            definition_respond_to: None,
            definition_respond_to_allowlist: Vec::new(),
            definition_parallelism: None,
            relay_mesh: None,
            effort_level: None,
        }
    }

    fn persona_def(
        id: &str,
        runtime: Option<&str>,
    ) -> crate::managed_agents::types::AgentDefinition {
        use crate::managed_agents::types::AgentDefinition;
        AgentDefinition {
            description: None,
            id: id.to_string(),
            display_name: String::new(),
            avatar_url: None,
            system_prompt: String::new(),
            runtime: runtime.map(str::to_string),
            model: None,
            provider: None,
            name_pool: vec![],
            is_builtin: false,
            is_active: true,
            shared: false,
            source_team: None,
            source_team_persona_slug: None,
            catalog_source: None,
            team_catalog_source: None,
            env_vars: std::collections::BTreeMap::new(),
            respond_to: None,
            respond_to_allowlist: vec![],
            parallelism: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    // ── Policy table: harness_max_parallelism / effective_parallelism ─────────

    #[test]
    fn policy_table() {
        let cap = super::OPENCLAW_MAX_PARALLELISM;

        // harness_max_parallelism: openclaw variants → Some(cap); others → None.
        assert_eq!(super::harness_max_parallelism("openclaw"), Some(cap));
        assert_eq!(
            super::harness_max_parallelism("/usr/local/bin/openclaw"),
            Some(cap)
        );
        assert_eq!(super::harness_max_parallelism("openclaw.exe"), Some(cap));
        assert_eq!(
            super::harness_max_parallelism(r"C:\Tools\openclaw.exe"),
            Some(cap)
        );
        assert_eq!(super::harness_max_parallelism("goose"), None);
        assert_eq!(super::harness_max_parallelism("buzz-agent"), None);
        assert_eq!(super::harness_max_parallelism(""), None);

        // effective_parallelism: openclaw clamps above cap, honors at/below; goose passes through.
        assert_eq!(super::effective_parallelism("openclaw", cap + 5), cap);
        assert_eq!(super::effective_parallelism("openclaw", cap), cap);
        assert_eq!(super::effective_parallelism("openclaw", cap - 2), cap - 2);
        assert_eq!(super::effective_parallelism("goose", 99), 99);
        assert_eq!(super::effective_parallelism("buzz-agent", 32), 32);
    }

    // ── Policy table: harness_default_parallelism / mint_parallelism ──────────

    /// Hermes (by id, command, path, or `.exe`) defaults to 1; every other
    /// harness, including unknown and empty commands, uses the app default.
    #[test]
    fn default_parallelism_table() {
        let app_default = crate::managed_agents::DEFAULT_AGENT_PARALLELISM;
        for hermes in [
            "hermes",
            "hermes-acp",
            "/opt/hermes/bin/hermes-acp",
            "hermes-acp.exe",
            r"C:\Tools\Hermes\HERMES-ACP.EXE",
        ] {
            assert_eq!(
                super::harness_default_parallelism(hermes),
                1,
                "Hermes default parallelism for {hermes:?}"
            );
        }
        for other in [
            "goose",
            "claude-agent-acp",
            "codex-acp",
            "buzz-agent",
            "openclaw",
            "devin",
            "/opt/custom/my-agent",
            "",
        ] {
            assert_eq!(
                super::harness_default_parallelism(other),
                app_default,
                "non-Hermes default parallelism for {other:?}"
            );
        }
        assert_ne!(app_default, 1, "the Hermes default must be observable");
    }

    /// Explicit or definition-provided values always win over the harness
    /// default; the default fills in only when nothing was requested.
    #[test]
    fn mint_parallelism_prefers_requested_value() {
        assert_eq!(super::mint_parallelism("hermes-acp", None), 1);
        assert_eq!(super::mint_parallelism("hermes", None), 1);
        assert_eq!(super::mint_parallelism("hermes-acp", Some(4)), 4);
        assert_eq!(
            super::mint_parallelism("goose", None),
            crate::managed_agents::DEFAULT_AGENT_PARALLELISM
        );
        assert_eq!(super::mint_parallelism("goose", Some(2)), 2);
    }

    // ── acp_agents_value: spawn-env seam ──────────────────────────────────────
    //
    // Drives the pure helper extracted from spawn_agent_child.
    // Deleting or changing it breaks this test AND the production spawn env.

    /// Legacy OpenClaw record (parallelism 10, above cap): BUZZ_ACP_AGENTS must be "5".
    #[test]
    fn acp_agents_value_openclaw_above_cap_is_capped() {
        assert_eq!(
            super::acp_agents_value("openclaw", 10),
            "5",
            "BUZZ_ACP_AGENTS for openclaw with parallelism 10 must be \"5\""
        );
        assert_eq!(super::acp_agents_value("goose", 10), "10");
    }

    // ── Override-direction: summary seam agreement ────────────────────────────
    //
    // Tests effective_parallelism and record_agent_command agreement for both
    // override directions. Removing either direction loses the seam test for
    // that cap/uncap path through the summary resolver.

    /// OpenClaw runtime + Goose override: summary resolves goose → uncapped (10).
    #[test]
    fn override_direction_openclaw_runtime_goose_override_is_uncapped() {
        let mut record = record_with(Some("openclaw"), 10);
        record.agent_command_override = Some("goose".to_string());
        let cmd = crate::managed_agents::record_agent_command(&record, &[]);
        assert_eq!(cmd, "goose");
        assert_eq!(super::effective_parallelism(&cmd, record.parallelism), 10);
    }

    /// Goose runtime + OpenClaw override: summary resolves openclaw → capped (5).
    #[test]
    fn override_direction_goose_runtime_openclaw_override_is_capped() {
        let mut record = record_with(Some("goose"), 10);
        record.agent_command_override = Some("openclaw".to_string());
        let cmd = crate::managed_agents::record_agent_command(&record, &[]);
        assert_eq!(cmd, "openclaw");
        assert_eq!(
            super::effective_parallelism(&cmd, record.parallelism),
            super::OPENCLAW_MAX_PARALLELISM
        );
    }

    // ── Summary: persona-inherited runtime (runtime=None) ─────────────────────
    //
    // Covers the case where runtime was cleared by an "inherit from persona"
    // update: summary must resolve via the LIVE persona, not stale agent_command.

    /// Stale agent_command="openclaw", live persona=goose → summary resolves goose → uncapped.
    #[test]
    fn summary_persona_inherited_stale_openclaw_live_goose_is_uncapped() {
        let persona = persona_def("p-goose", Some("goose"));
        let mut record = record_with(None, 10);
        record.persona_id = Some("p-goose".to_string());
        record.agent_command = "openclaw".to_string();
        let cmd =
            crate::managed_agents::record_agent_command(&record, std::slice::from_ref(&persona));
        assert_eq!(
            cmd, "goose",
            "live persona must win over stale agent_command"
        );
        assert_eq!(super::effective_parallelism(&cmd, record.parallelism), 10);
    }

    /// Stale agent_command="goose", live persona=openclaw → summary resolves openclaw → capped.
    #[test]
    fn summary_persona_inherited_stale_goose_live_openclaw_is_capped() {
        let persona = persona_def("p-openclaw", Some("openclaw"));
        let mut record = record_with(None, 10);
        record.persona_id = Some("p-openclaw".to_string());
        record.agent_command = "goose".to_string();
        let cmd =
            crate::managed_agents::record_agent_command(&record, std::slice::from_ref(&persona));
        assert_eq!(
            cmd, "openclaw",
            "live persona must win over stale agent_command"
        );
        assert_eq!(
            super::effective_parallelism(&cmd, record.parallelism),
            super::OPENCLAW_MAX_PARALLELISM
        );
    }

    // ── Snapshot export: requested-definition / effective-instance contract ───

    fn snapshot_record(
        runtime: Option<&str>,
        parallelism: u32,
        definition_parallelism: Option<u32>,
    ) -> ManagedAgentRecord {
        use crate::managed_agents::types::{BackendKind, RespondTo};
        use std::collections::BTreeMap;
        let mut r = record_with(runtime, parallelism);
        r.name = "snap-test".to_string();
        r.definition_parallelism = definition_parallelism;
        r.backend = BackendKind::Local;
        r.respond_to = RespondTo::OwnerOnly;
        r.env_vars = BTreeMap::new();
        r
    }

    /// Snapshot export carries the requested definition parallelism verbatim.
    #[test]
    fn snapshot_export_carries_requested_definition_parallelism() {
        use crate::managed_agents::agent_snapshot::{build_snapshot, MemoryLevel};
        // definition_parallelism=Some(10) stored → exported as 10 unchanged.
        let snap = build_snapshot(
            &snapshot_record(Some("openclaw"), 10, Some(10)),
            MemoryLevel::None,
            vec![],
            None,
        );
        assert_eq!(snap.definition.parallelism, Some(10));
        // No definition_parallelism stored → falls back to record.parallelism.
        let snap2 = build_snapshot(
            &snapshot_record(Some("openclaw"), 10, None),
            MemoryLevel::None,
            vec![],
            None,
        );
        assert_eq!(snap2.definition.parallelism, Some(10));
    }
}
