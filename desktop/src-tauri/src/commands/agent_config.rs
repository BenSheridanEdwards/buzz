use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::{
    app_state::AppState,
    managed_agents::{
        agent_readiness,
        config_bridge::{
            read_goose_file_config,
            reader::read_config_surface,
            types::{
                AcpConfigOptionEntry, AcpConfigOptionValue, AcpModelEntry, InheritedConfigTiers,
                RuntimeConfigSurface, SessionConfigCache,
            },
        },
        current_instance_id, is_reserved_env_key, is_safe_to_reveal, is_well_formed_env_key,
        known_acp_runtime, load_managed_agents, load_personas, resolve_effective_agent_env,
        save_managed_agents, sync_managed_agent_processes, AgentDefinition, AgentReadiness,
        BackendKind, GlobalAgentConfig, KnownAcpRuntime, ManagedAgentRecord,
        ManagedAgentRuntimeKey, Requirement, RespondTo, DEFAULT_ACP_COMMAND,
        DEFAULT_AGENT_PARALLELISM, DEFAULT_AGENT_TURN_TIMEOUT_SECONDS, MAX_ENV_VALUE_BYTES,
    },
};

/// Subset of the goose file config exposed to the frontend for gate evaluation.
///
/// Only the fields the dialog gate needs. This tracks which requirements are already satisfied in the
/// harness config file, so it can show "Set in goose config" rather than
/// surfacing a false missing-key marker.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeFileConfigSubset {
    /// Provider set in the harness config file, if any.
    pub provider: Option<String>,
    /// Model set in the harness config file, if any.
    pub model: Option<String>,
    /// Flat credential env keys in the harness config file's `extra` map (e.g. `DATABRICKS_HOST`); only non-empty values included.
    pub satisfied_env_keys: Vec<String>,
}

/// Sanitize a raw env map from an inherited tier (persona or global) with the
/// same rules `merged_user_env` applies at spawn time: reserved keys, malformed
/// keys, NUL-byte values, and oversize values are stripped silently.
fn sanitize_inherited_env(
    raw: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    raw.iter()
        .filter(|(k, v)| {
            !is_reserved_env_key(k)
                && is_well_formed_env_key(k)
                && !v.contains('\0')
                && v.len() <= MAX_ENV_VALUE_BYTES
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Normalize a structured field value: blank/whitespace-only collapses to
/// `None`, matching `effective_config`'s `non_blank` helper.
fn non_blank(v: Option<&str>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty()).map(str::to_owned)
}

fn trim_to_option(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

/// A proposed managed-agent configuration to evaluate before persistence.
///
/// Every field is an optional draft override of the same persisted field on
/// [`ManagedAgentRecord`]. Edit drafts include `pubkey` and inherit omitted
/// fields from that saved record; create drafts omit `pubkey` and are evaluated
/// from only the submitted draft plus global defaults. JSON `null` for
/// tri-state fields clears that value in the draft only. The resulting record
/// is never written to disk.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentReadinessDraft {
    /// Saved agent to use as the baseline for edit drafts. Omit for create
    /// drafts, which are evaluated from only the submitted proposed config and
    /// global defaults.
    #[serde(default)]
    pub pubkey: Option<String>,
    /// Draft runtime id. `null` clears the instance runtime so a linked agent
    /// inherits from its persona.
    #[serde(default, deserialize_with = "crate::util::double_option")]
    pub runtime: Option<Option<String>>,
    /// Draft explicit command pin. `null` clears the pin. Prefer `runtime` for
    /// catalog-known harnesses; this exists for Custom command drafts.
    #[serde(default, deserialize_with = "crate::util::double_option")]
    pub agent_command: Option<Option<String>>,
    /// Draft model selection. `null` clears the selection.
    #[serde(default, deserialize_with = "crate::util::double_option")]
    pub model: Option<Option<String>>,
    /// Draft provider selection. `null` clears the selection.
    #[serde(default, deserialize_with = "crate::util::double_option")]
    pub provider: Option<Option<String>>,
    /// Draft agent-local env vars. When present, replaces the record's env var
    /// map for evaluation only.
    #[serde(default)]
    pub env_vars: Option<std::collections::BTreeMap<String, String>>,
}

/// Presentation-ready readiness result for a saved or draft managed-agent
/// configuration.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentReadinessEvaluation {
    /// `true` when all backend-owned requirements are satisfied.
    pub ready: bool,
    /// Structured missing requirements, including the `surface` discriminator
    /// the UI uses to choose the resolution affordance.
    pub requirements: Vec<Requirement>,
}

fn readiness_to_evaluation(readiness: AgentReadiness) -> AgentReadinessEvaluation {
    match readiness {
        AgentReadiness::Ready => AgentReadinessEvaluation {
            ready: true,
            requirements: Vec::new(),
        },
        AgentReadiness::NotReady { requirements } => AgentReadinessEvaluation {
            ready: false,
            requirements,
        },
    }
}

fn apply_draft_override(record: &mut ManagedAgentRecord, draft: AgentReadinessDraft) {
    let AgentReadinessDraft {
        pubkey: _,
        runtime,
        agent_command,
        model,
        provider,
        env_vars,
    } = draft;
    let runtime_supplied = runtime.is_some();
    if let Some(runtime) = runtime {
        record.runtime = trim_to_option(runtime);
        record.agent_command_override = None;
    }
    if let Some(agent_command) = agent_command {
        if !runtime_supplied || record.runtime.is_none() {
            record.agent_command_override = trim_to_option(agent_command);
            if record.agent_command_override.is_some() {
                record.runtime = None;
            }
        }
    }
    if let Some(model) = model {
        record.model = trim_to_option(model);
    }
    if let Some(provider) = provider {
        record.provider = trim_to_option(provider);
    }
    if let Some(env_vars) = env_vars {
        record.env_vars = env_vars;
    }
}

fn evaluate_agent_record_readiness(
    record: &ManagedAgentRecord,
    personas: &[AgentDefinition],
    global: &GlobalAgentConfig,
) -> AgentReadinessEvaluation {
    let command = crate::managed_agents::record_agent_command(record, personas);
    let metadata = known_acp_runtime(&command);
    let effective = resolve_effective_agent_env(record, personas, metadata, global);
    readiness_to_evaluation(agent_readiness(&effective))
}

fn build_draft_baseline(draft: &AgentReadinessDraft) -> ManagedAgentRecord {
    let command = draft
        .agent_command
        .as_ref()
        .and_then(|value| value.as_ref())
        .and_then(|value| non_blank(Some(value)))
        .or_else(|| {
            draft
                .runtime
                .as_ref()
                .and_then(|value| value.as_ref())
                .and_then(|id| crate::managed_agents::command_for_runtime_id(id))
        })
        .unwrap_or_else(crate::managed_agents::default_agent_command);

    ManagedAgentRecord {
        pubkey: draft.pubkey.clone().unwrap_or_default(),
        name: "Draft Agent".to_string(),
        description: None,
        persona_id: None,
        team_id: None,
        private_key_nsec: String::new(),
        auth_tag: None,
        relay_url: String::new(),
        avatar_url: None,
        acp_command: DEFAULT_ACP_COMMAND.to_string(),
        agent_command: command,
        agent_command_override: None,
        agent_args: Vec::new(),
        mcp_command: String::new(),
        turn_timeout_seconds: DEFAULT_AGENT_TURN_TIMEOUT_SECONDS,
        idle_timeout_seconds: None,
        max_turn_duration_seconds: None,
        parallelism: DEFAULT_AGENT_PARALLELISM,
        system_prompt: None,
        model: None,
        provider: None,
        persona_source_version: None,
        env_vars: Default::default(),
        start_on_app_launch: false,
        auto_restart_on_config_change: true,
        runtime_pid: None,
        backend: BackendKind::Local,
        backend_agent_id: None,
        provider_policy_pending: false,
        provider_binary_path: None,
        persona_team_dir: None,
        persona_name_in_team: None,
        created_at: String::new(),
        updated_at: String::new(),
        last_started_at: None,
        last_stopped_at: None,
        last_exit_code: None,
        last_error: None,
        last_error_code: None,
        respond_to: RespondTo::OwnerOnly,
        respond_to_allowlist: Vec::new(),
        display_name: None,
        slug: None,
        runtime: None,
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

fn evaluate_saved_agent_readiness_for_app<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    pubkey: &str,
) -> Result<AgentReadinessEvaluation, String> {
    let _store_guard = state
        .managed_agents_store_lock
        .lock()
        .map_err(|e| e.to_string())?;
    let records = load_managed_agents(app)?;
    let record = records
        .iter()
        .find(|record| record.pubkey == pubkey)
        .ok_or_else(|| format!("agent {pubkey} not found"))?;
    let personas = load_personas(app).unwrap_or_default();
    let global = crate::managed_agents::load_global_agent_config(app).unwrap_or_default();
    Ok(evaluate_agent_record_readiness(record, &personas, &global))
}

fn evaluate_draft_agent_readiness_for_app<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    draft: AgentReadinessDraft,
) -> Result<AgentReadinessEvaluation, String> {
    let _store_guard = state
        .managed_agents_store_lock
        .lock()
        .map_err(|e| e.to_string())?;
    let records = load_managed_agents(app)?;
    let mut record = draft
        .pubkey
        .as_deref()
        .and_then(|pubkey| records.into_iter().find(|record| record.pubkey == pubkey))
        .unwrap_or_else(|| build_draft_baseline(&draft));
    if let Some(ref env_vars) = draft.env_vars {
        crate::managed_agents::validate_user_env_keys(env_vars)?;
    }
    apply_draft_override(&mut record, draft);
    let personas = load_personas(app).unwrap_or_default();
    let global = crate::managed_agents::load_global_agent_config(app).unwrap_or_default();
    Ok(evaluate_agent_record_readiness(&record, &personas, &global))
}

/// Build a sanitized `InheritedConfigTiers` snapshot at the command boundary.
///
/// Persona env, global env, and harness definition env are sanitized with
/// spawn-equivalent rules. Structured fields are normalized (blank → None).
/// A missing persona (orphaned link) yields empty persona tiers — the panel
/// still renders from record/global while spawn independently refuses.
fn build_inherited_tiers(
    record_persona_id: Option<&str>,
    record_runtime: Option<&str>,
    personas: &[AgentDefinition],
    global: &GlobalAgentConfig,
) -> InheritedConfigTiers {
    let persona = record_persona_id.and_then(|pid| personas.iter().find(|p| p.id == pid));

    let persona_env = persona
        .map(|p| sanitize_inherited_env(&p.env_vars))
        .unwrap_or_default();
    let global_env = sanitize_inherited_env(&global.env_vars);

    // Definition env: same resolution as spawn (record.runtime → persona.runtime → "").
    // Reserved keys stripped; no malformed-key / NUL / oversize check needed because
    // harness definitions are local admin-authored JSON, not user-provided data — but
    // we apply `sanitize_inherited_env` for defense-in-depth (same rules as the other tiers).
    let definition_env = {
        let runtime_id = record_runtime
            .or_else(|| persona.and_then(|p| p.runtime.as_deref()))
            .unwrap_or("");
        crate::managed_agents::custom_harnesses::lookup_loaded_harness_by_id(runtime_id)
            .map(|def| sanitize_inherited_env(&def.env))
            .unwrap_or_default()
    };

    let persona_model = persona.and_then(|p| non_blank(p.model.as_deref()));
    let persona_provider = persona.and_then(|p| non_blank(p.provider.as_deref()));
    let persona_prompt = persona.and_then(|p| non_blank(Some(&p.system_prompt)));
    let global_model = non_blank(global.model.as_deref());
    let global_provider = non_blank(global.provider.as_deref());

    InheritedConfigTiers {
        persona_env,
        global_env,
        definition_env,
        persona_model,
        persona_provider,
        persona_prompt,
        global_model,
        global_provider,
    }
}

/// Resolve the config surface with inherited persona and global tiers applied.
///
/// Persona-linked instances have their system_prompt/model/provider cleared
/// first (definition-authoritative): stale materialized snapshots can never
/// shadow live persona values. The reader then resolves each field through its
/// full candidate list (record env > ACP > persona env > global env > structured
/// persona/global > config file) via `resolve_with_override`.
fn resolve_config_surface(
    mut record: ManagedAgentRecord,
    personas: &[AgentDefinition],
    runtime_meta: Option<&KnownAcpRuntime>,
    session_cache: Option<&SessionConfigCache>,
    global: &GlobalAgentConfig,
    claude_config_dir: Option<&std::path::Path>,
) -> RuntimeConfigSurface {
    // Linked instances are definition-authoritative: clear stale materialized
    // model/provider/prompt so they can never masquerade as BuzzExplicit and
    // shadow definition values. Env var overrides are untouched.
    if record.persona_id.is_some() {
        record.system_prompt = None;
        record.model = None;
        record.provider = None;
    }

    let tiers = build_inherited_tiers(
        record.persona_id.as_deref(),
        record.runtime.as_deref(),
        personas,
        global,
    );

    read_config_surface(
        &record,
        runtime_meta,
        session_cache,
        &tiers,
        claude_config_dir,
    )
}

/// Get the file-layer config for a runtime — used by the Create/Edit/Persona
/// dialogs to know which requirements are already satisfied in the harness
/// config file (e.g. `~/.config/goose/config.yaml`), so they can show
/// "Set in goose config" instead of surfacing a false required-field marker.
///
/// Returns `null` when the runtime has no config file or it cannot be parsed.
/// Currently only "goose" is supported; other runtimes return `null`.
#[tauri::command]
pub async fn get_runtime_file_config(
    runtime_id: String,
) -> Result<Option<RuntimeFileConfigSubset>, String> {
    tokio::task::spawn_blocking(move || match runtime_id.as_str() {
        "goose" => {
            let cfg = read_goose_file_config()?;
            let satisfied_env_keys = cfg
                .extra
                .into_iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(k, _)| k)
                .collect();
            Some(RuntimeFileConfigSubset {
                provider: cfg.provider,
                model: cfg.model,
                satisfied_env_keys,
            })
        }
        _ => None,
    })
    .await
    .map_err(|e| format!("spawn_blocking failed: {e}"))
}

/// Return the key names of all non-empty baked build env vars.
///
/// Internal (Block) builds bake provider credentials and other env pairs into
/// the binary at compile time via `BUZZ_BUILD_AGENT_ENV`. The backend readiness
/// gate already treats these keys as satisfying their requirements (Layer 1 of
/// `resolve_effective_agent_env`). This command exposes the *key names only* —
/// never the values — so the frontend dialogs can apply the same logic and avoid
/// surfacing a spurious "Required" badge for keys that are covered by the baked
/// env.
///
/// OSS builds have no baked env, so this returns an empty list — OSS behavior
/// is unchanged.
#[tauri::command]
pub fn get_baked_build_env_keys() -> Vec<String> {
    crate::managed_agents::baked_build_env()
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, _)| k)
        .collect()
}

/// A single baked build env entry returned to the frontend.
///
/// Values are masked in Rust so unmasked secret values never cross the
/// Tauri IPC boundary. The `masked` flag lets the frontend style masked
/// rows distinctly.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BakedEnvEntry {
    pub key: String,
    /// The display value — real value for non-secret keys, `••••••` for
    /// secret keys whose names match the secret heuristic.
    pub value: String,
    /// `true` when the value was replaced by the mask placeholder.
    pub masked: bool,
}

/// Expose the baked build env to the frontend with values shown, but any
/// key not in the safe-to-reveal allowlist has its value replaced by `••••••`.
///
/// Provider and model arrive as `BUZZ_AGENT_PROVIDER` / `BUZZ_AGENT_MODEL`
/// keys in `baked_build_env()` and are included in the returned list like any
/// other key. Empty-value keys are filtered out (same as
/// `get_baked_build_env_keys`).
///
/// OSS builds return an empty list — the baked-env section is hidden entirely
/// in OSS installations.
#[tauri::command]
pub fn get_baked_build_env() -> Vec<BakedEnvEntry> {
    crate::managed_agents::baked_build_env()
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(key, value)| {
            let masked = !is_safe_to_reveal(&key);
            let display_value = if masked {
                "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}".to_string()
            } else {
                value
            };
            BakedEnvEntry {
                key,
                value: display_value,
                masked,
            }
        })
        .collect()
}

/// Evaluate the readiness of a saved managed-agent configuration.
///
/// This is the structured counterpart to the legacy runtime-status
/// `local_setup` boolean. It returns the exact backend-computed missing
/// requirements instead of forcing the frontend to rederive provider,
/// credential, CLI-login, or missing-binary readiness.
#[tauri::command]
pub async fn evaluate_agent_readiness(
    pubkey: String,
    app: AppHandle,
    _state: State<'_, AppState>,
) -> Result<AgentReadinessEvaluation, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        evaluate_saved_agent_readiness_for_app(&app, &state, &pubkey)
    })
    .await
    .map_err(|e| format!("spawn_blocking failed: {e}"))?
}

/// Evaluate a draft managed-agent configuration before it is persisted.
///
/// The saved agent is used only as a baseline for persona/global inheritance,
/// harness definition env, and omitted fields. Draft overrides are applied to a
/// clone and then passed through the same effective-env resolver and readiness
/// predicate that saved agents use. This command performs no write.
#[tauri::command]
pub async fn evaluate_agent_readiness_draft(
    draft: AgentReadinessDraft,
    app: AppHandle,
    _state: State<'_, AppState>,
) -> Result<AgentReadinessEvaluation, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        evaluate_draft_agent_readiness_for_app(&app, &state, draft)
    })
    .await
    .map_err(|e| format!("spawn_blocking failed: {e}"))?
}

/// Get the full config surface for a managed agent.
///
/// Returns normalized + advanced config from all available tiers.
/// Pre-spawn agents show config file values with ACP tiers marked as pending.
/// Persona-sourced values are resolved by `resolve_config_surface`.
#[tauri::command]
pub async fn get_agent_config_surface(
    pubkey: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RuntimeConfigSurface, String> {
    let record = {
        let _store_guard = state
            .managed_agents_store_lock
            .lock()
            .map_err(|e| e.to_string())?;
        let mut records = load_managed_agents(&app)?;
        let mut runtimes = state
            .managed_agent_processes
            .lock()
            .map_err(|e| e.to_string())?;
        let (sync_changed, exited_pubkeys) =
            sync_managed_agent_processes(&mut records, &mut runtimes, &current_instance_id(&app));
        if sync_changed {
            save_managed_agents(&app, &records)?;
        }
        for pubkey in &exited_pubkeys {
            state.clear_agent_session_caches(pubkey);
        }
        records
            .into_iter()
            .find(|r| r.pubkey == pubkey)
            .ok_or_else(|| format!("agent {pubkey} not found"))?
    };

    let personas = load_personas(&app).unwrap_or_default();
    let effective_cmd = crate::managed_agents::record_agent_command(&record, &personas);
    let runtime_meta = known_acp_runtime(&effective_cmd);
    let runtime_key = ManagedAgentRuntimeKey::new(
        pubkey.clone(),
        &crate::relay::effective_agent_relay_url(
            &record.relay_url,
            &crate::relay::relay_ws_url_with_override(&state),
        ),
    )?;
    let session_cache = state.get_session_cache(&runtime_key);
    let global = crate::managed_agents::load_global_agent_config(&app).unwrap_or_default();

    // #3493: for claude agents, resolve the settings.json and .claude.json paths
    // from the agent's effective CLAUDE_CONFIG_DIR env var (if set), falling
    // back to ~/.claude/ and ~/.claude.json. We never provision this dir
    // ourselves — we only respect what the user configured.
    //
    // Use resolve_effective_agent_env so the lookup covers all tiers (baked
    // floor → definition → global → persona → record) and cannot diverge from
    // what the spawned process actually sees.
    let claude_config_dir: Option<std::path::PathBuf> = if runtime_meta
        .is_some_and(|m| m.id == "claude")
    {
        let effective_env = resolve_effective_agent_env(&record, &personas, runtime_meta, &global);
        // Treat empty or blank CLAUDE_CONFIG_DIR as unset, matching Claude's
        // `CLAUDE_CONFIG_DIR || homedir()` resolver semantics.
        effective_env
            .env
            .get("CLAUDE_CONFIG_DIR")
            .filter(|v| !v.trim().is_empty())
            .map(std::path::PathBuf::from)
    } else {
        None
    };

    Ok(resolve_config_surface(
        record,
        &personas,
        runtime_meta,
        session_cache.as_ref(),
        &global,
        claude_config_dir.as_deref(),
    ))
}

/// Store a `session_config_captured` observer event payload into the session cache.
///
/// Called by the TypeScript observer relay when it decrypts a `session_config_captured`
/// event from a running agent. The payload contains raw ACP session/new fields.
#[tauri::command]
pub fn put_agent_session_config(
    pubkey: String,
    payload: serde_json::Value,
    app: AppHandle,
    state: State<'_, AppState>,
) {
    let record_relay_url = {
        let _guard = match state.managed_agents_store_lock.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        match load_managed_agents(&app) {
            Ok(records) => match records.into_iter().find(|r| r.pubkey == pubkey) {
                Some(record) => record.relay_url,
                None => return,
            },
            _ => return,
        }
    };

    // Pair identity: prefer the relay URL the harness attached to the payload
    // (same pattern as lifecycle frames). Older harnesses don't attach one;
    // fall back to the record's effective relay — with no attached URL the
    // frame can only have arrived over the active workspace relay, which is
    // exactly what effective_agent_relay_url resolves to absent a pin.
    let relay_url = payload
        .get("relayUrl")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            crate::relay::effective_agent_relay_url(
                &record_relay_url,
                &crate::relay::relay_ws_url_with_override(&state),
            )
        });

    let config_options = parse_config_options(payload.get("configOptions"));
    let available_modes = parse_modes(&config_options, payload.get("modes"));
    let (available_models, current_model) = parse_models(payload.get("models"));
    let model_overridden = payload
        .get("modelOverridden")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let cache = SessionConfigCache {
        config_options,
        available_modes,
        available_models,
        current_model,
        model_overridden,
        goose_native_config: None,
        captured_at: crate::util::now_iso(),
    };

    let Ok(runtime_key) = ManagedAgentRuntimeKey::new(pubkey, &relay_url) else {
        return;
    };
    state.put_session_cache(runtime_key, cache);
}

fn parse_config_options(raw: Option<&serde_json::Value>) -> Vec<AcpConfigOptionEntry> {
    let arr = match raw.and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|opt| {
            let config_id = opt
                .get("id")
                .or_else(|| opt.get("configId"))?
                .as_str()?
                .to_string();
            Some(AcpConfigOptionEntry {
                config_id,
                category: opt
                    .get("category")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                display_name: opt
                    .get("displayName")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                current_value: opt
                    .get("value")
                    .or_else(|| opt.get("currentValue"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                options: parse_option_values(opt.get("options")),
            })
        })
        .collect()
}

fn parse_option_values(raw: Option<&serde_json::Value>) -> Vec<AcpConfigOptionValue> {
    let arr = match raw.and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|o| {
            let value = o.get("value").and_then(|v| v.as_str())?.to_string();
            Some(AcpConfigOptionValue {
                value,
                display_name: o
                    .get("displayName")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })
        })
        .collect()
}

fn parse_modes(
    config_options: &[AcpConfigOptionEntry],
    raw: Option<&serde_json::Value>,
) -> Vec<String> {
    if let Some(arr) = raw.and_then(|v| v.as_array()) {
        return arr
            .iter()
            .filter_map(|m| m.as_str().map(str::to_string))
            .collect();
    }
    // Fall back: extract mode options from configOptions with category "mode".
    config_options
        .iter()
        .filter(|o| o.category.as_deref() == Some("mode"))
        .flat_map(|o| o.options.iter().map(|v| v.value.clone()))
        .collect()
}

fn parse_models(raw: Option<&serde_json::Value>) -> (Vec<AcpModelEntry>, Option<String>) {
    let raw = match raw {
        Some(v) => v,
        None => return (Vec::new(), None),
    };

    // Object shape: { currentModelId, availableModels: [...] }
    if let Some(obj) = raw.as_object() {
        let current_model = obj
            .get("currentModelId")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let models = obj
            .get("availableModels")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let model_id = m
                            .get("modelId")
                            .or_else(|| m.get("id"))
                            .and_then(|v| v.as_str())?
                            .to_string();
                        Some(AcpModelEntry {
                            model_id,
                            name: m.get("name").and_then(|v| v.as_str()).map(str::to_string),
                            description: m
                                .get("description")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        return (models, current_model);
    }

    // Array shape: [{ modelId, isCurrent, ... }]
    let arr = match raw.as_array() {
        Some(a) => a,
        None => return (Vec::new(), None),
    };
    let mut current_model = None;
    let models = arr
        .iter()
        .filter_map(|m| {
            let model_id = m
                .get("modelId")
                .or_else(|| m.get("id"))
                .and_then(|v| v.as_str())?
                .to_string();
            if m.get("isCurrent")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                current_model = Some(model_id.clone());
            }
            Some(AcpModelEntry {
                model_id,
                name: m.get("name").and_then(|v| v.as_str()).map(str::to_string),
                description: m
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })
        })
        .collect();
    (models, current_model)
}

/// Atomically set the record's canonical effort column and strip every stale
/// record-scope effort env alias. Split from the Tauri command so the invariant
/// — no leftover alias can outrank the just-set column — is directly testable.
pub(crate) fn apply_picker_effort_level(
    record: &mut ManagedAgentRecord,
    effort_level: Option<String>,
) {
    record.effort_level = effort_level;
    crate::managed_agents::remove_record_effort_aliases(&mut record.env_vars);
}

#[cfg(test)]
#[path = "agent_config_tests.rs"]
mod tests;
