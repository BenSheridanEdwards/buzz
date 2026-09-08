//! Relay membership registration for managed agents.
//!
//! A closed relay (`BUZZ_REQUIRE_RELAY_MEMBERSHIP`) refuses every publish
//! from a pubkey that is not in its `relay_members` roster. The desktop mints
//! a fresh key for every managed agent, so without this step an agent can
//! read but never reply. The relay exposes the roster through NIP-43 admin
//! events (kind:9030 add, sent by an admin/owner over the ordinary signed
//! `POST /events` door), and that is what this module drives.
//!
//! The outcome per (agent, relay) pair is persisted in a small sidecar store
//! next to `managed-agents.json` so the Agents view can show "not a relay
//! member yet" with the npub to hand to the community owner, instead of the
//! agent silently failing to reply.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app_state::AppState;
use crate::managed_agents::ManagedAgentRuntimeKey;

/// Bound for each relay round trip in the preflight.
const MEMBERSHIP_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// NIP-43 membership list kind (single replaceable relay-signed event).
const KIND_NIP43_MEMBERSHIP_LIST: u32 = 13534;

/// Last known membership of a managed agent on one relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayMembershipState {
    /// The relay lists the agent (or does not enforce membership).
    Member,
    /// The relay enforces membership, the agent is not listed, and this
    /// identity is not allowed to add it.
    NotMember,
    /// The last check could not be completed; retried on the next start.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedAgentRelayMembership {
    pub state: RelayMembershipState,
    pub checked_at: String,
    /// Human-readable detail for `NotMember` / `Unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Result of one registration attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayMembershipOutcome {
    /// The relay does not advertise NIP-43: nothing to register.
    OpenRelay,
    /// Already in the roster.
    AlreadyMember,
    /// This identity added the agent as a `member` just now.
    Registered,
    /// This identity holds no admin/owner role on the relay, so it cannot
    /// add the agent. `actor_role` is its role when the roster lists it as
    /// a plain member; `relay_message` is the relay's own refusal when the
    /// add was attempted and rejected.
    NotAuthorized {
        actor_role: Option<String>,
        relay_message: Option<String>,
    },
}

/// What the preflight should do given the roster it read. Pure so the
/// permission matrix is table-testable without a relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MembershipAction {
    Nothing,
    Add,
    Blocked { actor_role: Option<String> },
}

/// Decide from the roster alone. The relay is the authority on who may add:
/// only a roster row that positively says "plain member" skips the attempt.
/// A missing row is NOT proof of no role: a freshly provisioned relay whose
/// owner/admins were seeded by `buzz-admin` may not have published a
/// kind:13534 yet, and an admin must still be able to register agents there.
/// The relay refuses an unauthorized kind:9030 and that refusal is mapped to
/// [`RelayMembershipOutcome::NotAuthorized`] by the caller.
pub fn membership_action(agent_is_member: bool, actor_role: Option<&str>) -> MembershipAction {
    if agent_is_member {
        return MembershipAction::Nothing;
    }
    match actor_role {
        Some("owner") | Some("admin") | None => MembershipAction::Add,
        other => MembershipAction::Blocked {
            actor_role: other.map(str::to_string),
        },
    }
}

/// Lowercase-hex roster from a NIP-43 kind:13534 event: `pubkey -> role`.
fn roster_from_event(event: &nostr::Event) -> BTreeMap<String, String> {
    crate::nostr_convert::relay_members_from_event(event)
        .get("members")
        .and_then(|members| members.as_array())
        .map(|members| {
            members
                .iter()
                .filter_map(|member| {
                    let pubkey = member.get("pubkey")?.as_str()?.to_ascii_lowercase();
                    let role = member
                        .get("role")
                        .and_then(|role| role.as_str())
                        .unwrap_or("member")
                        .to_string();
                    Some((pubkey, role))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Make sure `agent_pubkey` can publish on `relay_ws_url`.
///
/// Reads the NIP-11 document, then the NIP-43 roster as the workspace
/// identity, and submits a kind:9030 when that identity is an admin or
/// owner. Errors are relay/network failures; a permission gap is a
/// successful `NotAuthorized` outcome, not an error.
pub async fn ensure_managed_agent_relay_membership(
    state: &AppState,
    relay_ws_url: &str,
    agent_pubkey: &str,
) -> Result<RelayMembershipOutcome, String> {
    let http_base = crate::relay::relay_http_base_url(relay_ws_url);
    let advertised = tokio::time::timeout(
        MEMBERSHIP_QUERY_TIMEOUT,
        crate::relay::relay_advertises_membership_at(state, &http_base),
    )
    .await
    .map_err(|_| "timed out reading the relay information document".to_string())??;
    if !advertised {
        return Ok(RelayMembershipOutcome::OpenRelay);
    }

    let owner_keys = state.signing_keys()?;
    let actor_pubkey = owner_keys.public_key().to_hex();
    let agent_pubkey = agent_pubkey.to_ascii_lowercase();

    let events = tokio::time::timeout(
        MEMBERSHIP_QUERY_TIMEOUT,
        crate::relay::query_relay_at(
            state,
            &http_base,
            &[serde_json::json!({
                "kinds": [KIND_NIP43_MEMBERSHIP_LIST],
                "limit": 1
            })],
        ),
    )
    .await
    .map_err(|_| "timed out reading the relay membership list".to_string())??;
    let roster = events.first().map(roster_from_event).unwrap_or_default();

    match membership_action(
        roster.contains_key(&agent_pubkey),
        roster.get(&actor_pubkey).map(String::as_str),
    ) {
        MembershipAction::Nothing => Ok(RelayMembershipOutcome::AlreadyMember),
        MembershipAction::Blocked { actor_role } => Ok(RelayMembershipOutcome::NotAuthorized {
            actor_role,
            relay_message: None,
        }),
        MembershipAction::Add => {
            let actor_role = roster.get(&actor_pubkey).cloned();
            let event = crate::events::build_relay_admin_add(&agent_pubkey, "member")?
                .sign_with_keys(&owner_keys)
                .map_err(|error| format!("failed to sign the relay admin command: {error}"))?;
            let verdict = tokio::time::timeout(
                MEMBERSHIP_QUERY_TIMEOUT,
                crate::relay::submit_signed_event_verdict_at_with_keys(
                    &event,
                    state,
                    &http_base,
                    &owner_keys,
                ),
            )
            .await
            .map_err(|_| "timed out adding the agent to the relay".to_string())??;
            match verdict {
                crate::relay::SubmitVerdict::Accepted(_) => Ok(RelayMembershipOutcome::Registered),
                // A structured refusal is the relay's definitive answer on
                // THIS identity's authority (the relay checks its own roster,
                // so an unpublished or stale kind:13534 cannot mislead it).
                // It is a `NotAuthorized` outcome, not a transport error: the
                // card shows the relay's message next to the operator
                // command, and the next start retries anyway. Outages stay
                // `Err` and are persisted as `Unknown` by the caller.
                crate::relay::SubmitVerdict::Refused { relay_message, .. } => {
                    Ok(RelayMembershipOutcome::NotAuthorized {
                        actor_role,
                        relay_message: Some(relay_message),
                    })
                }
            }
        }
    }
}

// ── Sidecar store ───────────────────────────────────────────────────────────

pub type RelayMembershipStore = BTreeMap<String, BTreeMap<String, ManagedAgentRelayMembership>>;

fn membership_store_path(base_dir: &Path) -> PathBuf {
    base_dir.join("relay-membership.json")
}

fn load_membership_store(base_dir: &Path) -> Result<RelayMembershipStore, String> {
    let path = membership_store_path(base_dir);
    if !path.exists() {
        return Ok(RelayMembershipStore::new());
    }
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn save_membership_store(base_dir: &Path, store: &RelayMembershipStore) -> Result<(), String> {
    let path = membership_store_path(base_dir);
    if store.is_empty() {
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|error| format!("failed to remove {}: {error}", path.display()))?;
        }
        return Ok(());
    }
    let payload = serde_json::to_vec_pretty(store)
        .map_err(|error| format!("failed to serialize relay membership store: {error}"))?;
    super::storage::atomic_write_json(&path, &payload)
}

/// Canonical relay key used by the sidecar (same normalization as runtime
/// pair keys, so `ws://localhost:3000` and `ws://127.0.0.1:3000` collapse).
fn membership_relay_key(agent_pubkey: &str, relay_ws_url: &str) -> Result<String, String> {
    Ok(ManagedAgentRuntimeKey::new(agent_pubkey.to_string(), relay_ws_url)?.relay_url)
}

/// Persist the outcome for one pair. `None` removes the entry (open relay:
/// membership is meaningless there, and stale "not a member" copy must not
/// survive a relay flipping open).
pub fn record_relay_membership(
    base_dir: &Path,
    agent_pubkey: &str,
    relay_ws_url: &str,
    membership: Option<ManagedAgentRelayMembership>,
) -> Result<(), String> {
    let relay_key = membership_relay_key(agent_pubkey, relay_ws_url)?;
    let agent_key = agent_pubkey.to_ascii_lowercase();
    let mut store = load_membership_store(base_dir)?;
    match membership {
        Some(membership) => {
            store
                .entry(agent_key)
                .or_default()
                .insert(relay_key, membership);
        }
        None => {
            if let Some(per_relay) = store.get_mut(&agent_key) {
                per_relay.remove(&relay_key);
                if per_relay.is_empty() {
                    store.remove(&agent_key);
                }
            }
        }
    }
    save_membership_store(base_dir, &store)
}

/// Drop every entry for an agent (deletion path).
pub fn clear_relay_membership(base_dir: &Path, agent_pubkey: &str) -> Result<(), String> {
    let mut store = load_membership_store(base_dir)?;
    if store.remove(&agent_pubkey.to_ascii_lowercase()).is_none() {
        return Ok(());
    }
    save_membership_store(base_dir, &store)
}

/// Read the whole store once for a summary pass.
pub fn load_relay_memberships(base_dir: &Path) -> RelayMembershipStore {
    load_membership_store(base_dir).unwrap_or_else(|error| {
        eprintln!("buzz-desktop: {error}");
        RelayMembershipStore::new()
    })
}

/// Membership of `agent_pubkey` on `relay_ws_url` from a loaded store.
pub fn relay_membership_for(
    store: &RelayMembershipStore,
    agent_pubkey: &str,
    relay_ws_url: &str,
) -> Option<ManagedAgentRelayMembership> {
    let relay_key = membership_relay_key(agent_pubkey, relay_ws_url).ok()?;
    store
        .get(&agent_pubkey.to_ascii_lowercase())?
        .get(&relay_key)
        .cloned()
}

/// Copy shown next to a `NotMember` agent. The frontend renders this detail
/// verbatim above the npub and the operator command.
pub fn not_member_detail(actor_role: Option<&str>, relay_message: Option<&str>) -> String {
    let why = match actor_role {
        Some(role) => format!(
            "This relay only accepts members and your role here is {role}, so Buzz could not add the agent."
        ),
        None => "This relay only accepts members and you are not one of its admins, so Buzz could not add the agent.".to_string(),
    };
    match relay_message.map(str::trim).filter(|m| !m.is_empty()) {
        Some(message) => format!("{why} Relay said: {message}"),
        None => why,
    }
}

/// Map an outcome onto the persisted state. `None` clears the entry.
pub fn membership_record_for_outcome(
    outcome: &RelayMembershipOutcome,
    checked_at: String,
) -> Option<ManagedAgentRelayMembership> {
    match outcome {
        RelayMembershipOutcome::OpenRelay => None,
        RelayMembershipOutcome::AlreadyMember | RelayMembershipOutcome::Registered => {
            Some(ManagedAgentRelayMembership {
                state: RelayMembershipState::Member,
                checked_at,
                detail: None,
            })
        }
        RelayMembershipOutcome::NotAuthorized {
            actor_role,
            relay_message,
        } => Some(ManagedAgentRelayMembership {
            state: RelayMembershipState::NotMember,
            checked_at,
            detail: Some(not_member_detail(
                actor_role.as_deref(),
                relay_message.as_deref(),
            )),
        }),
    }
}

/// Run the registration for one pair and persist the result. A relay or
/// network failure is persisted as `Unknown` (so the UI never claims
/// membership it did not verify) and returned, so callers log it; the next
/// start or profile reconcile retries.
pub async fn preflight_managed_agent_relay_membership(
    app: &tauri::AppHandle,
    state: &AppState,
    agent_pubkey: &str,
    relay_ws_url: &str,
) -> Result<RelayMembershipOutcome, String> {
    let base_dir = super::managed_agents_base_dir(app)?;
    let outcome = ensure_managed_agent_relay_membership(state, relay_ws_url, agent_pubkey).await;
    let now = crate::util::now_iso();
    let record = match &outcome {
        Ok(outcome) => membership_record_for_outcome(outcome, now),
        Err(error) => Some(ManagedAgentRelayMembership {
            state: RelayMembershipState::Unknown,
            checked_at: now,
            detail: Some(error.clone()),
        }),
    };
    {
        let _store_guard = state
            .managed_agents_store_lock
            .lock()
            .map_err(|error| error.to_string())?;
        record_relay_membership(&base_dir, agent_pubkey, relay_ws_url, record)?;
    }
    if let Ok(RelayMembershipOutcome::Registered) = &outcome {
        eprintln!(
            "buzz-desktop: registered managed agent {agent_pubkey} as a member of {relay_ws_url}"
        );
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT: &str = "a1f3c0ffee00000000000000000000000000000000000000000000000000beef";
    const OTHER: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    /// Full input space of the permission matrix. Mirrors the relay's own
    /// rule (kind:9030 needs admin or owner): the desktop must not try an
    /// add the roster proves will be refused, must not skip one it can do,
    /// and must still try when the roster says nothing about this identity
    /// (no kind:13534 published yet) because the relay, not the roster copy,
    /// is the authority.
    #[test]
    fn membership_action_table() {
        let cases = [
            (true, Some("owner"), MembershipAction::Nothing),
            (true, None, MembershipAction::Nothing),
            (false, Some("owner"), MembershipAction::Add),
            (false, Some("admin"), MembershipAction::Add),
            (false, None, MembershipAction::Add),
            (
                false,
                Some("member"),
                MembershipAction::Blocked {
                    actor_role: Some("member".to_string()),
                },
            ),
            (
                false,
                Some("Owner"),
                MembershipAction::Blocked {
                    actor_role: Some("Owner".to_string()),
                },
            ),
        ];
        for (is_member, role, expected) in cases {
            assert_eq!(
                membership_action(is_member, role),
                expected,
                "is_member={is_member} role={role:?}"
            );
        }
    }

    #[test]
    fn sidecar_store_round_trips_per_relay_and_clears_on_delete() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let member = ManagedAgentRelayMembership {
            state: RelayMembershipState::Member,
            checked_at: "t1".into(),
            detail: None,
        };
        let blocked = ManagedAgentRelayMembership {
            state: RelayMembershipState::NotMember,
            checked_at: "t2".into(),
            detail: Some("nope".into()),
        };
        record_relay_membership(base, AGENT, "ws://localhost:3000", Some(member.clone())).unwrap();
        record_relay_membership(base, AGENT, "wss://b.example", Some(blocked.clone())).unwrap();
        record_relay_membership(base, OTHER, "wss://b.example", Some(member.clone())).unwrap();

        let store = load_relay_memberships(base);
        // Canonical relay key: localhost and 127.0.0.1 are the same pair.
        assert_eq!(
            relay_membership_for(&store, AGENT, "ws://127.0.0.1:3000"),
            Some(member.clone())
        );
        assert_eq!(
            relay_membership_for(&store, &AGENT.to_ascii_uppercase(), "wss://b.example/"),
            Some(blocked)
        );
        assert_eq!(relay_membership_for(&store, AGENT, "wss://c.example"), None);

        // Open relay: the entry is removed, not left stale.
        record_relay_membership(base, AGENT, "wss://b.example", None).unwrap();
        let store = load_relay_memberships(base);
        assert_eq!(relay_membership_for(&store, AGENT, "wss://b.example"), None);
        assert!(relay_membership_for(&store, AGENT, "ws://localhost:3000").is_some());

        clear_relay_membership(base, AGENT).unwrap();
        let store = load_relay_memberships(base);
        assert!(!store.contains_key(AGENT));
        assert!(relay_membership_for(&store, OTHER, "wss://b.example").is_some());

        clear_relay_membership(base, OTHER).unwrap();
        assert!(!membership_store_path(base).exists());
    }

    #[test]
    fn outcome_maps_to_persisted_state() {
        assert_eq!(
            membership_record_for_outcome(&RelayMembershipOutcome::OpenRelay, "t".into()),
            None
        );
        for outcome in [
            RelayMembershipOutcome::AlreadyMember,
            RelayMembershipOutcome::Registered,
        ] {
            assert_eq!(
                membership_record_for_outcome(&outcome, "t".into()).map(|m| m.state),
                Some(RelayMembershipState::Member)
            );
        }
        let blocked = membership_record_for_outcome(
            &RelayMembershipOutcome::NotAuthorized {
                actor_role: Some("member".into()),
                relay_message: None,
            },
            "t".into(),
        )
        .unwrap();
        assert_eq!(blocked.state, RelayMembershipState::NotMember);
        assert!(blocked.detail.unwrap().contains("member"));

        let refused = membership_record_for_outcome(
            &RelayMembershipOutcome::NotAuthorized {
                actor_role: None,
                relay_message: Some("actor not authorized: must be admin or owner".into()),
            },
            "t".into(),
        )
        .unwrap();
        assert_eq!(refused.state, RelayMembershipState::NotMember);
        let detail = refused.detail.unwrap();
        assert!(detail.contains("not one of its admins"), "{detail}");
        assert!(
            detail.contains("Relay said: actor not authorized"),
            "{detail}"
        );
    }

    // Gated off Windows like the other stub-relay tests: `build_app_state()`
    // pulls native DLLs unavailable in the Windows CI runner.
    #[cfg(not(target_os = "windows"))]
    mod stub_relay {
        use super::*;
        use crate::app_state::build_app_state;
        use nostr::JsonUtil;
        use std::sync::{Arc, Mutex};

        struct StubRelay {
            ws_url: String,
            /// Raw JSON bodies posted to `/events`.
            posted: Arc<Mutex<Vec<serde_json::Value>>>,
            /// When set, `/events` answers HTTP 500 to everything (relay
            /// outage mid-request).
            events_outage: Arc<Mutex<bool>>,
        }

        /// The relay's own refusal for an unauthorized kind:9030, as the HTTP
        /// bridge renders it: `IngestError::Rejected` becomes HTTP 400 with
        /// `{"error": "invalid: <reason>"}` (`api/bridge.rs`, `api_error`),
        /// the reason coming from `handlers/relay_admin.rs`.
        const RELAY_NOT_AUTHORIZED: &str = "invalid: actor not authorized: must be admin or owner";

        /// Stub relay: `/info` advertises NIP-43 when `closed`; `/query`
        /// answers the kind:13534 filter with a relay-signed roster (or
        /// nothing when `publish_roster` is false, like a relay whose members
        /// were seeded out of band and never announced); `/events` records
        /// what was posted and enforces the relay's kind:9030 rule against
        /// `authority` (the roster the relay itself holds): only an admin or
        /// owner may add, and the reply is the bridge's refusal otherwise.
        async fn spawn_stub_relay_with(
            closed: bool,
            authority: Vec<(String, &'static str)>,
            publish_roster: bool,
        ) -> StubRelay {
            use axum::{
                http::header::CONTENT_TYPE, http::StatusCode, routing::get, routing::post, Router,
            };

            let events_outage = Arc::new(Mutex::new(false));
            let outage = events_outage.clone();

            let relay_keys = nostr::Keys::generate();
            let roster_event = {
                let tags: Vec<nostr::Tag> = authority
                    .iter()
                    .map(|(pubkey, role)| {
                        nostr::Tag::parse(["member", pubkey.as_str(), role]).expect("member tag")
                    })
                    .collect();
                nostr::EventBuilder::new(nostr::Kind::Custom(13534), "")
                    .tags(tags)
                    .sign_with_keys(&relay_keys)
                    .expect("sign roster")
                    .as_json()
            };
            let posted = Arc::new(Mutex::new(Vec::new()));
            let seen = posted.clone();
            let supported: Vec<u32> = if closed { vec![1, 42, 43] } else { vec![1, 42] };
            let stewards: Vec<String> = authority
                .iter()
                .filter(|(_, role)| *role == "admin" || *role == "owner")
                .map(|(pubkey, _)| pubkey.to_ascii_lowercase())
                .collect();
            let app = Router::new()
                .route(
                    "/info",
                    get(move || {
                        let supported = supported.clone();
                        async move {
                            (
                                StatusCode::OK,
                                serde_json::json!({ "supported_nips": supported }).to_string(),
                            )
                        }
                    }),
                )
                .route(
                    "/query",
                    post(move |body: String| {
                        let roster_event = roster_event.clone();
                        async move {
                            let filters: serde_json::Value =
                                serde_json::from_str(&body).unwrap_or_default();
                            let wants_roster = filters
                                .as_array()
                                .and_then(|f| f.first())
                                .and_then(|f| f.get("kinds"))
                                .and_then(|k| k.as_array())
                                .is_some_and(|k| k.iter().any(|v| v.as_u64() == Some(13534)));
                            if wants_roster && publish_roster {
                                (StatusCode::OK, format!("[{roster_event}]"))
                            } else {
                                (StatusCode::OK, "[]".to_string())
                            }
                        }
                    }),
                )
                .route(
                    "/events",
                    post(move |body: String| {
                        let seen = seen.clone();
                        let stewards = stewards.clone();
                        let outage = outage.clone();
                        async move {
                            let json = [(CONTENT_TYPE, "application/json")];
                            if *outage.lock().unwrap() {
                                return (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    json,
                                    serde_json::json!({ "error": "internal server error" })
                                        .to_string(),
                                );
                            }
                            let event: serde_json::Value =
                                serde_json::from_str(&body).unwrap_or_default();
                            let id = event
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let sender = event
                                .get("pubkey")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_ascii_lowercase();
                            let is_admin_command =
                                event.get("kind").and_then(|v| v.as_u64()) == Some(9030);
                            seen.lock().unwrap().push(event);
                            if is_admin_command && !stewards.contains(&sender) {
                                return (
                                    StatusCode::BAD_REQUEST,
                                    json,
                                    serde_json::json!({ "error": RELAY_NOT_AUTHORIZED })
                                        .to_string(),
                                );
                            }
                            (
                                StatusCode::OK,
                                json,
                                serde_json::json!({
                                    "event_id": id,
                                    "accepted": true,
                                    "message": ""
                                })
                                .to_string(),
                            )
                        }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind stub relay");
            let addr = listener.local_addr().expect("stub relay addr");
            tokio::spawn(async move {
                axum::serve(listener, app).await.ok();
            });
            StubRelay {
                ws_url: format!("ws://{addr}"),
                posted,
                events_outage,
            }
        }

        /// Stub relay whose roster is both enforced and published.
        async fn spawn_stub_relay(closed: bool, roster: Vec<(String, &'static str)>) -> StubRelay {
            spawn_stub_relay_with(closed, roster, true).await
        }

        fn state_with_identity(keys: &nostr::Keys) -> AppState {
            let state = build_app_state();
            *state.keys.lock().unwrap() = keys.clone();
            state
        }

        #[tokio::test]
        async fn owner_registers_a_new_agent_with_a_signed_kind_9030() {
            let owner = nostr::Keys::generate();
            let relay = spawn_stub_relay(true, vec![(owner.public_key().to_hex(), "owner")]).await;
            let state = state_with_identity(&owner);

            let outcome = ensure_managed_agent_relay_membership(&state, &relay.ws_url, AGENT)
                .await
                .unwrap();

            assert_eq!(outcome, RelayMembershipOutcome::Registered);
            let posted = relay.posted.lock().unwrap();
            assert_eq!(posted.len(), 1, "exactly one admin command posted");
            let event = nostr::Event::from_json(posted[0].to_string()).expect("valid event");
            assert!(event.verify().is_ok(), "posted 9030 must be validly signed");
            assert_eq!(event.kind, nostr::Kind::Custom(9030));
            assert_eq!(
                event.pubkey,
                owner.public_key(),
                "signed by the workspace owner"
            );
            let tags: Vec<Vec<String>> = event.tags.iter().map(|t| t.as_slice().to_vec()).collect();
            assert!(tags.contains(&vec!["p".to_string(), AGENT.to_string()]));
            assert!(
                tags.contains(&vec!["role".to_string(), "member".to_string()]),
                "managed agents are added as plain members, never admins: {tags:?}"
            );
        }

        #[tokio::test]
        async fn admin_registers_too() {
            let admin = nostr::Keys::generate();
            let relay = spawn_stub_relay(true, vec![(admin.public_key().to_hex(), "admin")]).await;
            let state = state_with_identity(&admin);
            let outcome = ensure_managed_agent_relay_membership(&state, &relay.ws_url, AGENT)
                .await
                .unwrap();
            assert_eq!(outcome, RelayMembershipOutcome::Registered);
            assert_eq!(relay.posted.lock().unwrap().len(), 1);
        }

        #[tokio::test]
        async fn plain_member_is_blocked_and_posts_nothing() {
            let member = nostr::Keys::generate();
            let relay =
                spawn_stub_relay(true, vec![(member.public_key().to_hex(), "member")]).await;
            let state = state_with_identity(&member);
            let outcome = ensure_managed_agent_relay_membership(&state, &relay.ws_url, AGENT)
                .await
                .unwrap();
            assert_eq!(
                outcome,
                RelayMembershipOutcome::NotAuthorized {
                    actor_role: Some("member".to_string()),
                    relay_message: None,
                }
            );
            assert!(relay.posted.lock().unwrap().is_empty());
        }

        /// A key the published roster does not list is not proven powerless
        /// (the roster copy may be stale), so the add is attempted and the
        /// relay's refusal becomes the outcome, verbatim, for the card.
        #[tokio::test]
        async fn stranger_is_refused_by_the_relay_and_the_refusal_is_kept() {
            let stranger = nostr::Keys::generate();
            let relay = spawn_stub_relay(true, vec![(OTHER.to_string(), "owner")]).await;
            let state = state_with_identity(&stranger);
            let outcome = ensure_managed_agent_relay_membership(&state, &relay.ws_url, AGENT)
                .await
                .unwrap();
            assert_eq!(
                outcome,
                RelayMembershipOutcome::NotAuthorized {
                    actor_role: None,
                    relay_message: Some(RELAY_NOT_AUTHORIZED.to_string()),
                }
            );
            assert_eq!(relay.posted.lock().unwrap().len(), 1, "one refused attempt");
        }

        /// Relay seeded by `buzz-admin add-member --role admin` that never
        /// published a kind:13534: the desktop admin must still register the
        /// agent because the relay checks its own roster, not the announced
        /// copy.
        #[tokio::test]
        async fn admin_registers_even_when_no_roster_event_was_published() {
            let admin = nostr::Keys::generate();
            let relay =
                spawn_stub_relay_with(true, vec![(admin.public_key().to_hex(), "admin")], false)
                    .await;
            let state = state_with_identity(&admin);
            let outcome = ensure_managed_agent_relay_membership(&state, &relay.ws_url, AGENT)
                .await
                .unwrap();
            assert_eq!(outcome, RelayMembershipOutcome::Registered);
            assert_eq!(relay.posted.lock().unwrap().len(), 1);
        }

        #[tokio::test]
        async fn listed_agent_is_left_alone() {
            let owner = nostr::Keys::generate();
            let relay = spawn_stub_relay(
                true,
                vec![
                    (owner.public_key().to_hex(), "owner"),
                    (AGENT.to_string(), "member"),
                ],
            )
            .await;
            let state = state_with_identity(&owner);
            let outcome = ensure_managed_agent_relay_membership(&state, &relay.ws_url, AGENT)
                .await
                .unwrap();
            assert_eq!(outcome, RelayMembershipOutcome::AlreadyMember);
            assert!(relay.posted.lock().unwrap().is_empty());
        }

        #[tokio::test]
        async fn open_relay_needs_no_registration() {
            let stranger = nostr::Keys::generate();
            let relay = spawn_stub_relay(false, vec![]).await;
            let state = state_with_identity(&stranger);
            let outcome = ensure_managed_agent_relay_membership(&state, &relay.ws_url, AGENT)
                .await
                .unwrap();
            assert_eq!(outcome, RelayMembershipOutcome::OpenRelay);
            assert!(relay.posted.lock().unwrap().is_empty());
        }

        /// A relay that answers 5xx to the add has NOT refused this identity.
        /// That must stay an error (persisted as `Unknown`, retried), never a
        /// `NotAuthorized` that would tell the user to go find an operator.
        #[tokio::test]
        async fn server_error_on_the_add_is_an_error_not_a_refusal() {
            let admin = nostr::Keys::generate();
            let relay = spawn_stub_relay(true, vec![(admin.public_key().to_hex(), "admin")]).await;
            *relay.events_outage.lock().unwrap() = true;
            let state = state_with_identity(&admin);
            let result = ensure_managed_agent_relay_membership(&state, &relay.ws_url, AGENT).await;
            assert!(
                result.is_err(),
                "a 5xx must not become an outcome: {result:?}"
            );
        }

        #[tokio::test]
        async fn unreachable_relay_is_an_error() {
            let state = state_with_identity(&nostr::Keys::generate());
            let result =
                ensure_managed_agent_relay_membership(&state, "ws://127.0.0.1:9", AGENT).await;
            assert!(result.is_err());
        }
    }
}
