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
///
/// The two are matched differently because the relay writes them
/// differently. `actor not authorized` is the head of a sentence the relay
/// composes (`actor not authorized: must be admin or owner`, prefixed with
/// `invalid: ` by the ingest path), so it is matched as a substring of what
/// the relay said. `relay_membership_required` is a machine token the relay
/// emits in the body's `error` field and nowhere else (`api/mod.rs`,
/// `enforce_relay_membership`), so it is compared to that field exactly: a
/// proxied error page or a human sentence that merely quotes the token is not
/// the relay answering with it, and reading it as one sends the user to an
/// operator with a command that could never clear the card.
pub fn refusal_is_about_authority(refusal: &crate::relay::RelayRefusal) -> bool {
    refusal.haystack().contains("actor not authorized")
        || refusal.code_is("relay_membership_required")
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
pub async fn preflight_managed_agent_relay_membership<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
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
#[path = "relay_membership_tests.rs"]
mod tests;
