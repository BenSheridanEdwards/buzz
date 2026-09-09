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
    /// Whose pubkey the detail is about, when that is NOT the agent.
    ///
    /// Every state but one is the relay's answer about the agent, and the
    /// card shows the agent's npub with the `add-member` command for it.
    /// [`RelayMembershipOutcome::IdentityRefused`] is the exception: the
    /// relay refused the WORKSPACE identity at the roster read, so it never
    /// said anything about the agent, and the npub the operator needs is the
    /// user's. Carried here so the card can name the right subject without
    /// the frontend having to know which identity a check ran as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_pubkey: Option<String>,
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
    /// The relay refused THIS DESKTOP IDENTITY at the roster read, so the
    /// check never got as far as asking about the agent.
    ///
    /// The bridge gates `/query` on membership before it parses the filters
    /// (`api/bridge.rs`, `enforce_relay_membership`), so this 403 is the
    /// relay's answer about the user, not about the agent. Keeping it as
    /// `NotAuthorized` printed "you are not one of its admins, so Buzz could
    /// not add the agent" next to the AGENT's npub and an `add-member
    /// --pubkey <agent hex>` the operator may already have run: an agent the
    /// operator admitted would stay blocked forever on the card, for
    /// something it is not guilty of, under the wrong remedy. `actor_pubkey`
    /// is the identity the relay actually refused.
    IdentityRefused {
        actor_pubkey: String,
        relay_message: Option<String>,
    },
    /// The relay refused the command for a reason that is not about who may
    /// edit the roster: a `created_at` outside its 120s window (a laptop
    /// after sleep, a VM with clock drift), a banned actor, a NIP-98 failure.
    /// The refusal proves nothing about membership, so it is persisted as
    /// `Unknown` with the relay's own words and no operator command, and the
    /// next start retries.
    Refused { relay_message: String },
}

/// Is a relay refusal about THIS identity's authority over the roster?
///
/// The relay says so in exactly two ways, and they are the only two refusals
/// that justify telling the user to go find an operator:
/// `actor not authorized` from `handlers/relay_admin.rs`, and
/// `relay_membership_required` from the HTTP bridge's membership gate
/// (`api/mod.rs`, `enforce_relay_membership`). Everything else the same 4xx
/// door emits (clock skew, a ban, a NIP-98 failure) refuses the command
/// without saying anything about roles.
pub fn refusal_is_about_authority(refusal: &crate::relay::RelayRefusal) -> bool {
    let said = refusal.haystack();
    said.contains("actor not authorized") || said.contains("relay_membership_required")
}

/// Turn a relay refusal into the outcome it justifies.
fn outcome_for_refusal(
    actor_role: Option<String>,
    refusal: &crate::relay::RelayRefusal,
) -> RelayMembershipOutcome {
    if refusal_is_about_authority(refusal) {
        return RelayMembershipOutcome::NotAuthorized {
            actor_role,
            relay_message: Some(refusal.message()),
        };
    }
    RelayMembershipOutcome::Refused {
        relay_message: refusal.message(),
    }
}

/// Turn a refusal of the ROSTER READ into the outcome it justifies.
///
/// Same 4xx door as [`outcome_for_refusal`], different subject: nothing about
/// the agent has been asked yet, so an authority refusal here is the relay
/// declining `actor_pubkey`, not a verdict on the agent.
fn outcome_for_roster_read_refusal(
    actor_pubkey: &str,
    refusal: &crate::relay::RelayRefusal,
) -> RelayMembershipOutcome {
    if refusal_is_about_authority(refusal) {
        return RelayMembershipOutcome::IdentityRefused {
            actor_pubkey: actor_pubkey.to_ascii_lowercase(),
            relay_message: Some(refusal.message()),
        };
    }
    RelayMembershipOutcome::Refused {
        relay_message: refusal.message(),
    }
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
/// successful `NotAuthorized` outcome, not an error, and a refusal that is
/// not about permissions is a `Refused` outcome that says so instead of
/// borrowing the permission copy.
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

    // The roster read is the first thing a closed relay refuses: the bridge
    // enforces membership on `/query` before it looks at the filters, so a
    // 403 here is the relay's answer about THIS identity, not a transport
    // failure. A desktop identity that is not on the roster at all reaches
    // exactly this door, and the answer belongs in the card as `NotMember`
    // with the operator command, not as an unexplained `Unknown`.
    let roster_read = tokio::time::timeout(
        MEMBERSHIP_QUERY_TIMEOUT,
        crate::relay::query_relay_details_at(
            state,
            &http_base,
            &[serde_json::json!({
                "kinds": [KIND_NIP43_MEMBERSHIP_LIST],
                "limit": 1
            })],
        ),
    )
    .await
    .map_err(|_| "timed out reading the relay membership list".to_string())?;
    let events = match roster_read {
        Ok(events) => events,
        Err(details) => {
            let Some(refusal) = details.refusal else {
                return Err(details.error);
            };
            // The subject of a refusal HERE is the workspace identity, not
            // the agent: the gate runs before the filters are parsed, so the
            // relay has not been asked about the agent at all.
            return Ok(outcome_for_roster_read_refusal(&actor_pubkey, &refusal));
        }
    };
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
                // A structured refusal is the relay's definitive answer about
                // THIS command (the relay checks its own roster, so an
                // unpublished or stale kind:13534 cannot mislead it), but the
                // same 4xx door carries refusals that have nothing to do with
                // roles. Only an authorization refusal earns the "you are not
                // an admin" copy and the operator command; anything else is
                // kept verbatim as `Unknown`. Outages stay `Err`.
                crate::relay::SubmitVerdict::Refused { refusal, .. } => {
                    Ok(outcome_for_refusal(actor_role, &refusal))
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

/// Copy shown when the relay refused the WORKSPACE identity at the roster
/// read. The npub printed under it is the user's, not the agent's, so the
/// sentence must not blame the agent or imply the agent is the one to admit.
pub fn identity_refused_detail(relay_message: Option<&str>) -> String {
    let why = "This relay only accepts members and it did not accept your identity, so Buzz could not check or register the agent. The agent may already be admitted; the npub below is yours.";
    match relay_message.map(str::trim).filter(|m| !m.is_empty()) {
        Some(message) => format!("{why} Relay said: {message}"),
        None => why.to_string(),
    }
}

/// Copy shown for a refusal that says nothing about roles. No operator
/// command follows it: adding the agent by hand would not fix a skewed
/// clock, and the next start retries the real check.
pub fn refused_detail(relay_message: &str) -> String {
    let why = "Buzz could not register this agent on the relay, and the relay did not say it was a permission problem.";
    match relay_message.trim() {
        "" => why.to_string(),
        message => format!("{why} Relay said: {message}"),
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
                subject_pubkey: None,
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
            subject_pubkey: None,
        }),
        RelayMembershipOutcome::IdentityRefused {
            actor_pubkey,
            relay_message,
        } => Some(ManagedAgentRelayMembership {
            state: RelayMembershipState::NotMember,
            checked_at,
            detail: Some(identity_refused_detail(relay_message.as_deref())),
            subject_pubkey: Some(actor_pubkey.clone()),
        }),
        RelayMembershipOutcome::Refused { relay_message } => Some(ManagedAgentRelayMembership {
            state: RelayMembershipState::Unknown,
            checked_at,
            detail: Some(refused_detail(relay_message)),
            subject_pubkey: None,
        }),
    }
}

/// Map the whole result of one check onto the persisted state.
///
/// Extracted from [`preflight_managed_agent_relay_membership`] so the
/// `Err -> Unknown` rule is reachable without a `tauri::AppHandle`: a relay
/// or network failure must never leave the last verified state on disk, or
/// the card would keep claiming a membership this check did not confirm.
pub fn membership_record_for_result(
    result: &Result<RelayMembershipOutcome, String>,
    checked_at: String,
) -> Option<ManagedAgentRelayMembership> {
    match result {
        Ok(outcome) => membership_record_for_outcome(outcome, checked_at),
        Err(error) => Some(ManagedAgentRelayMembership {
            state: RelayMembershipState::Unknown,
            checked_at,
            detail: Some(error.clone()),
            subject_pubkey: None,
        }),
    }
}

/// Should this pair be checked against the relay again?
///
/// Only a sidecar record that positively says `Member` skips the check: that
/// is the one state the relay already confirmed and that cannot silently
/// become false while the agent runs. `NotMember`, `Unknown` and no record at
/// all all retry, which is what makes the start path the retry seam for an
/// agent the operator has since admitted.
///
/// Consulted by the automatic, fleet-wide callers only: launch restore and
/// the profile reconcile. A manual single-agent start deliberately does not
/// consult it, so a `Member` that the relay has since revoked still gets
/// re-verified somewhere. See `preflight_relay_membership_for_start`.
pub fn should_preflight_membership(
    store: &RelayMembershipStore,
    agent_pubkey: &str,
    relay_ws_url: &str,
) -> bool {
    !relay_membership_for(store, agent_pubkey, relay_ws_url)
        .is_some_and(|membership| membership.state == RelayMembershipState::Member)
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
    let record = membership_record_for_result(&outcome, crate::util::now_iso());
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
            subject_pubkey: None,
        };
        let blocked = ManagedAgentRelayMembership {
            state: RelayMembershipState::NotMember,
            checked_at: "t2".into(),
            detail: Some("nope".into()),
            subject_pubkey: None,
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

    /// Only the relay's two authorization refusals earn the "not an admin"
    /// copy and the operator command. Every other refusal the same 4xx door
    /// carries is a different problem with a different remedy.
    #[test]
    fn refusal_classification_table() {
        let cases = [
            (
                "invalid: actor not authorized: must be admin or owner",
                true,
            ),
            ("relay_membership_required", true),
            ("Actor Not Authorized", true),
            (
                "invalid: event timestamp out of range: created_at=1, now=2, delta=-1s (max ±120s)",
                false,
            ),
            ("invalid: actor is banned", false),
            ("invalid nip-98 authorization", false),
            ("invalid: unknown kind", false),
            ("", false),
        ];
        for (code, expected) in cases {
            let refusal = crate::relay::RelayRefusal {
                code: Some(code.to_string()),
                detail: None,
            };
            assert_eq!(
                refusal_is_about_authority(&refusal),
                expected,
                "code={code:?}"
            );
        }
        // The bridge sends the machine reason and a human sentence; either
        // half naming the refusal is enough.
        assert!(refusal_is_about_authority(&crate::relay::RelayRefusal {
            code: Some("relay_membership_required".into()),
            detail: Some("You must be a relay member to access this relay".into()),
        }));
    }

    /// The `Err -> Unknown` rule: a check that could not be completed must
    /// overwrite whatever the sidecar held, never leave a stale `Member`
    /// behind for the card to keep asserting.
    #[test]
    fn a_failed_check_persists_unknown_with_the_failure() {
        let record = membership_record_for_result(
            &Err("relay unreachable: request timed out".to_string()),
            "t".into(),
        )
        .expect("a failure must still be recorded");
        assert_eq!(record.state, RelayMembershipState::Unknown);
        assert_eq!(
            record.detail.as_deref(),
            Some("relay unreachable: request timed out")
        );
        // An open relay still clears the entry through the same seam.
        assert_eq!(
            membership_record_for_result(&Ok(RelayMembershipOutcome::OpenRelay), "t".into()),
            None
        );
        assert_eq!(
            membership_record_for_result(&Ok(RelayMembershipOutcome::Registered), "t".into())
                .map(|record| record.state),
            Some(RelayMembershipState::Member)
        );
    }

    /// The start/reconcile skip guard: only a verified `Member` skips the
    /// check. Every other state retries, which is the only way an agent the
    /// operator has since admitted ever leaves the `NotMember` card.
    #[test]
    fn only_a_verified_member_skips_the_preflight() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        const RELAY: &str = "ws://localhost:3000";

        assert!(
            should_preflight_membership(&load_relay_memberships(base), AGENT, RELAY),
            "no record yet must check"
        );
        for (state, expected) in [
            (RelayMembershipState::Member, false),
            (RelayMembershipState::NotMember, true),
            (RelayMembershipState::Unknown, true),
        ] {
            record_relay_membership(
                base,
                AGENT,
                RELAY,
                Some(ManagedAgentRelayMembership {
                    state,
                    checked_at: "t".into(),
                    detail: None,
                    subject_pubkey: None,
                }),
            )
            .unwrap();
            assert_eq!(
                should_preflight_membership(&load_relay_memberships(base), AGENT, RELAY),
                expected,
                "state={state:?}"
            );
        }
        // A `Member` record on one relay says nothing about another.
        assert!(should_preflight_membership(
            &load_relay_memberships(base),
            AGENT,
            "wss://other.example"
        ));
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
            /// When set, `/events` refuses every kind:9030 with this 400 JSON
            /// `error`, whatever the sender's role.
            events_refusal: Arc<Mutex<Option<String>>>,
        }

        /// The relay's own refusal for an unauthorized kind:9030, as the HTTP
        /// bridge renders it: `IngestError::Rejected` becomes HTTP 400 with
        /// `{"error": "invalid: <reason>"}` (`api/bridge.rs`, `api_error`),
        /// the reason coming from `handlers/relay_admin.rs`.
        const RELAY_NOT_AUTHORIZED: &str = "invalid: actor not authorized: must be admin or owner";

        /// The relay's refusal of a command whose `created_at` is outside its
        /// 120s window (`handlers/relay_admin.rs`). Same 400 JSON door as the
        /// authorization refusal, entirely different meaning.
        const RELAY_CLOCK_SKEW: &str =
            "invalid: event timestamp out of range: created_at=1, now=2, delta=-1s (max ±120s)";

        /// The bridge's membership refusal on a closed relay, verbatim from
        /// `api/mod.rs`, `enforce_relay_membership`: HTTP 403 with both a
        /// machine `error` and a human `message`.
        fn membership_required_body() -> String {
            serde_json::json!({
                "error": "relay_membership_required",
                "message": "You must be a relay member to access this relay"
            })
            .to_string()
        }

        /// Sender pubkey of a NIP-98 `Authorization: Nostr <base64 event>`
        /// header, lowercased. The stub reads it for the same reason the
        /// relay does: to decide whether this caller may read at all.
        fn nip98_sender(headers: &axum::http::HeaderMap) -> String {
            use base64::Engine as _;

            let Some(encoded) = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Nostr "))
            else {
                return String::new();
            };
            base64::engine::general_purpose::STANDARD
                .decode(encoded.trim())
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .and_then(|event| {
                    event
                        .get("pubkey")
                        .and_then(|value| value.as_str())
                        .map(str::to_ascii_lowercase)
                })
                .unwrap_or_default()
        }

        /// Stub relay: `/info` advertises NIP-43 when `closed`; `/query`
        /// enforces membership before it looks at the filters, exactly as the
        /// bridge does (`api/bridge.rs`, `query_events_authed`), then answers
        /// the kind:13534 filter with a relay-signed roster (or nothing when
        /// `publish_roster` is false, like a relay whose members were seeded
        /// out of band and never announced); `/events` records what was
        /// posted and enforces the relay's kind:9030 rule against `authority`
        /// (the roster the relay itself holds): only an admin or owner may
        /// add, and the reply is the bridge's refusal otherwise.
        ///
        /// `authority` is the relay's own roster, so a key absent from it is
        /// a stranger to this relay and gets the 403 a real closed relay
        /// gives it. The stub must not be more permissive than the gate the
        /// production code is supposed to survive.
        async fn spawn_stub_relay_with(
            closed: bool,
            authority: Vec<(String, &'static str)>,
            publish_roster: bool,
        ) -> StubRelay {
            use axum::{
                http::header::CONTENT_TYPE, http::HeaderMap, http::StatusCode, routing::get,
                routing::post, Router,
            };

            let events_outage = Arc::new(Mutex::new(false));
            let outage = events_outage.clone();
            let events_refusal: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
            let refusal = events_refusal.clone();

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
            let relay_roster: Vec<String> = authority
                .iter()
                .map(|(pubkey, _)| pubkey.to_ascii_lowercase())
                .collect();
            let events_roster = relay_roster.clone();
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
                    post(move |headers: HeaderMap, body: String| {
                        let roster_event = roster_event.clone();
                        let relay_roster = relay_roster.clone();
                        async move {
                            let json = [(CONTENT_TYPE, "application/json")];
                            // Membership first, before the filters are even
                            // parsed: this is the door a desktop identity
                            // that is not on the roster hits on a closed
                            // relay.
                            if closed && !relay_roster.contains(&nip98_sender(&headers)) {
                                return (StatusCode::FORBIDDEN, json, membership_required_body());
                            }
                            let filters: serde_json::Value =
                                serde_json::from_str(&body).unwrap_or_default();
                            let wants_roster = filters
                                .as_array()
                                .and_then(|f| f.first())
                                .and_then(|f| f.get("kinds"))
                                .and_then(|k| k.as_array())
                                .is_some_and(|k| k.iter().any(|v| v.as_u64() == Some(13534)));
                            if wants_roster && publish_roster {
                                (StatusCode::OK, json, format!("[{roster_event}]"))
                            } else {
                                (StatusCode::OK, json, "[]".to_string())
                            }
                        }
                    }),
                )
                .route(
                    "/events",
                    post(move |headers: HeaderMap, body: String| {
                        let seen = seen.clone();
                        let stewards = stewards.clone();
                        let outage = outage.clone();
                        let refusal = refusal.clone();
                        let events_roster = events_roster.clone();
                        async move {
                            let json = [(CONTENT_TYPE, "application/json")];
                            // `api/bridge.rs` gates `/events` on membership
                            // too, not just `/query`, and on the same NIP-98
                            // sender. The stub used to check only its steward
                            // list here, so a post from an identity a real
                            // closed relay would have turned away at the door
                            // reached the kind:9030 rule and passed.
                            if closed && !events_roster.contains(&nip98_sender(&headers)) {
                                return (StatusCode::FORBIDDEN, json, membership_required_body());
                            }
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
                            if is_admin_command {
                                if let Some(reason) = refusal.lock().unwrap().clone() {
                                    return (
                                        StatusCode::BAD_REQUEST,
                                        json,
                                        serde_json::json!({ "error": reason }).to_string(),
                                    );
                                }
                            }
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
                events_refusal,
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

        /// A desktop identity the closed relay does not list at all never
        /// gets as far as the roster: the bridge refuses its `/query` with
        /// `relay_membership_required`. That 403 is the relay's answer about
        /// this identity, so the card says "not a member" with the operator
        /// command instead of an unexplained "unverified".
        #[tokio::test]
        async fn stranger_is_refused_at_the_roster_read_and_the_refusal_is_kept() {
            let stranger = nostr::Keys::generate();
            let relay = spawn_stub_relay(true, vec![(OTHER.to_string(), "owner")]).await;
            let state = state_with_identity(&stranger);
            let outcome = ensure_managed_agent_relay_membership(&state, &relay.ws_url, AGENT)
                .await
                .unwrap();
            let RelayMembershipOutcome::IdentityRefused {
                actor_pubkey,
                relay_message,
            } = outcome.clone()
            else {
                panic!("a 403 on the roster read is the relay's answer: {outcome:?}");
            };
            // The subject is the identity the relay actually turned away, not
            // the agent it was never asked about.
            assert_eq!(actor_pubkey, stranger.public_key().to_hex());
            assert_eq!(
                relay_message.as_deref(),
                Some("You must be a relay member to access this relay")
            );
            assert!(
                relay.posted.lock().unwrap().is_empty(),
                "a refused read must not be followed by a doomed 9030"
            );
            let record = membership_record_for_outcome(&outcome, "t".into())
                .expect("a refusal is persisted");
            assert_eq!(record.state, RelayMembershipState::NotMember);
            assert_eq!(
                record.subject_pubkey.as_deref(),
                Some(stranger.public_key().to_hex().as_str()),
                "the card must print the user's npub and the operator command \
                 for it, not the agent's"
            );
            let detail = record.detail.unwrap_or_default();
            assert!(
                detail.contains("did not accept your identity"),
                "the copy must name the right subject: {detail}"
            );
            assert!(
                !detail.contains("could not add the agent"),
                "and must not blame the agent the relay never saw: {detail}"
            );
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

        /// A relay member whose relay never published a roster event: the
        /// read succeeds (it is a member) and says nothing, so the add is
        /// attempted and the relay refuses it on authority. That refusal is
        /// the one that earns the "you are not an admin" copy.
        #[tokio::test]
        async fn member_without_a_published_roster_is_refused_on_authority() {
            let member = nostr::Keys::generate();
            let relay =
                spawn_stub_relay_with(true, vec![(member.public_key().to_hex(), "member")], false)
                    .await;
            let state = state_with_identity(&member);
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

        /// The relay refused the 9030 for a reason that has nothing to do
        /// with roles: an admin whose laptop woke up with a skewed clock is
        /// outside the relay's 120s window. Telling them they are not an
        /// admin, and handing them an `add-member` command that would not fix
        /// it, is wrong copy for a wrong diagnosis. It persists as `Unknown`
        /// with the relay's own words and retries on the next start.
        #[tokio::test]
        async fn a_refusal_that_is_not_about_authority_is_not_notmember() {
            let admin = nostr::Keys::generate();
            let relay = spawn_stub_relay(true, vec![(admin.public_key().to_hex(), "admin")]).await;
            *relay.events_refusal.lock().unwrap() = Some(RELAY_CLOCK_SKEW.to_string());
            let state = state_with_identity(&admin);
            let outcome = ensure_managed_agent_relay_membership(&state, &relay.ws_url, AGENT)
                .await
                .unwrap();
            assert_eq!(
                outcome,
                RelayMembershipOutcome::Refused {
                    relay_message: RELAY_CLOCK_SKEW.to_string(),
                }
            );
            let record = membership_record_for_outcome(&outcome, "t".into()).unwrap();
            assert_eq!(
                record.state,
                RelayMembershipState::Unknown,
                "a non-authorization refusal must not claim a role problem"
            );
            let detail = record.detail.unwrap();
            assert!(detail.contains("event timestamp out of range"), "{detail}");
            assert!(
                !detail.contains("not one of its admins"),
                "wrong diagnosis in the card: {detail}"
            );
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

        /// A redirected NIP-11 document must not be able to say "this relay
        /// is open".
        ///
        /// `OpenRelay` is the one outcome that CLEARS the sidecar row
        /// (`membership_record_for_outcome` returns `None`), registers
        /// nothing, and leaves no card at all. Letting a third origin assert
        /// it turns a closed relay the agent cannot publish on into a silent
        /// success. An error keeps the row `Unknown` and the next start
        /// retries.
        #[tokio::test]
        async fn a_redirected_info_document_cannot_make_a_closed_relay_look_open() {
            use axum::{http::StatusCode, response::IntoResponse, routing::get, Router};

            // Origin B is a real open relay: its answer must never count.
            let open = spawn_stub_relay(false, vec![]).await;
            let target = format!("{}/info", crate::relay::relay_http_base_url(&open.ws_url));
            let app = Router::new().route(
                "/info",
                get(move || {
                    let target = target.clone();
                    async move {
                        (StatusCode::FOUND, [(axum::http::header::LOCATION, target)])
                            .into_response()
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind redirecting relay");
            let addr = listener.local_addr().expect("redirecting relay addr");
            tokio::spawn(async move {
                axum::serve(listener, app).await.ok();
            });

            let state = state_with_identity(&nostr::Keys::generate());
            let result =
                ensure_managed_agent_relay_membership(&state, &format!("ws://{addr}"), AGENT).await;

            let error = result.expect_err("a 3xx is not the relay's answer");
            assert!(
                error.contains("redirected"),
                "the error must name the redirect: {error}"
            );
            assert_eq!(
                membership_record_for_result(&Err(error), "t".into()).map(|record| record.state),
                Some(RelayMembershipState::Unknown),
                "and it must be persisted as Unknown, never clear the row"
            );
        }
    }
}
