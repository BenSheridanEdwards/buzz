//! Tests for the managed-agent relay-membership preflight.
//!
//! Extracted from `relay_membership.rs` to keep that file under the desktop
//! Rust size ratchet. The stub relay here answers the same three doors a real
//! closed relay does (`/info`, `/query`, `/events`) on the same NIP-98 sender,
//! so every assertion below runs against the production seam.

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
                                serde_json::json!({ "error": "internal server error" }).to_string(),
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
                                serde_json::json!({ "error": RELAY_NOT_AUTHORIZED }).to_string(),
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
        let relay = spawn_stub_relay(true, vec![(member.public_key().to_hex(), "member")]).await;
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
        let record =
            membership_record_for_outcome(&outcome, "t".into()).expect("a refusal is persisted");
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
            spawn_stub_relay_with(true, vec![(admin.public_key().to_hex(), "admin")], false).await;
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
        let result = ensure_managed_agent_relay_membership(&state, "ws://127.0.0.1:9", AGENT).await;
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
                    (StatusCode::FOUND, [(axum::http::header::LOCATION, target)]).into_response()
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

    /// A redirected ROSTER READ must not be able to say "this agent is
    /// already a member".
    ///
    /// `AlreadyMember` persists as `Member`, and `Member` is the one state
    /// `should_preflight_membership` skips, so a forged roster stops the
    /// restore pass and every future reconcile from ever checking that pair
    /// again while the agent was never registered and no card says anything.
    /// The kind-13534-is-relay-only guarantee lives in the relay's
    /// `ingest_event` and does not travel with the bytes: the event below is
    /// signed by a key generated in this test, and origin B answers with it.
    #[tokio::test]
    async fn a_redirected_roster_read_cannot_make_a_stranger_look_like_a_member() {
        use axum::{
            http::header::CONTENT_TYPE, http::StatusCode, response::IntoResponse, routing::get,
            routing::post, Router,
        };

        // Origin B: any key at all, asserting the agent is a member.
        let impostor = nostr::Keys::generate();
        let forged = nostr::EventBuilder::new(nostr::Kind::Custom(13534), "")
            .tags([nostr::Tag::parse(["member", AGENT, "member"]).expect("member tag")])
            .sign_with_keys(&impostor)
            .expect("sign forged roster")
            .as_json();
        let b = Router::new().route(
            "/query",
            post(move || {
                let forged = forged.clone();
                async move {
                    (
                        StatusCode::OK,
                        [(CONTENT_TYPE, "application/json")],
                        format!("[{forged}]"),
                    )
                }
            }),
        );
        let b_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind forging origin");
        let b_addr = b_listener.local_addr().expect("forging origin addr");
        tokio::spawn(async move {
            axum::serve(b_listener, b).await.ok();
        });

        // Origin A: a real closed relay's `/info`, and a `/query` that hands
        // the question to origin B.
        let target = format!("http://{b_addr}/query");
        let a = Router::new()
            .route(
                "/info",
                get(|| async {
                    (
                        StatusCode::OK,
                        serde_json::json!({ "supported_nips": [43] }).to_string(),
                    )
                }),
            )
            .route(
                "/query",
                post(move || {
                    let target = target.clone();
                    async move {
                        (
                            StatusCode::TEMPORARY_REDIRECT,
                            [(axum::http::header::LOCATION, target)],
                        )
                            .into_response()
                    }
                }),
            );
        let a_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind redirecting relay");
        let a_addr = a_listener.local_addr().expect("redirecting relay addr");
        tokio::spawn(async move {
            axum::serve(a_listener, a).await.ok();
        });

        let state = state_with_identity(&nostr::Keys::generate());
        let result =
            ensure_managed_agent_relay_membership(&state, &format!("ws://{a_addr}"), AGENT).await;

        let error = result.expect_err("a 3xx is not the relay's answer");
        assert!(
            error.contains("redirected"),
            "the error must name the redirect: {error}"
        );
        let record = membership_record_for_result(&Err(error), "t".into())
            .expect("an unanswered check is persisted");
        assert_eq!(
            record.state,
            RelayMembershipState::Unknown,
            "the row must stay checkable, never become a Member nothing re-checks"
        );
    }

    /// A 200 that is not a NIP-11 document must not read as "this relay is
    /// open".
    ///
    /// `OpenRelay` is the one outcome that CLEARS the sidecar row, so an
    /// ingress error page or a relay mid-restart would silently delete the
    /// card that tells the user their agent cannot publish. No redirect is
    /// involved; the body below is exactly what a proxy answers while a
    /// backend starts.
    #[tokio::test]
    async fn a_200_that_is_not_a_nip11_document_cannot_clear_the_membership_row() {
        use axum::{http::StatusCode, routing::get, Router};

        let app = Router::new().route(
            "/info",
            get(|| async {
                (
                    StatusCode::OK,
                    serde_json::json!({ "error": "backend starting" }).to_string(),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub relay");
        let addr = listener.local_addr().expect("stub relay addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let state = state_with_identity(&nostr::Keys::generate());
        let result =
            ensure_managed_agent_relay_membership(&state, &format!("ws://{addr}"), AGENT).await;

        let error = result.expect_err("a non-NIP-11 body is not an answer about membership");
        assert!(
            error.contains("NIP-11"),
            "the error must say what was missing: {error}"
        );
        assert_eq!(
            membership_record_for_result(&Err(error), "t".into()).map(|record| record.state),
            Some(RelayMembershipState::Unknown),
            "an unanswered check keeps the row; only a real OpenRelay clears it"
        );
    }

    /// The relay's own words are bounded before they are persisted.
    ///
    /// This branch is the first thing to write relay-authored text into
    /// `relay-membership.json`, one row per agent/relay pair, re-read on
    /// every summary pass and rendered on the card. Nothing upstream caps the
    /// body, so without a bound a relay answering megabytes puts megabytes
    /// there, per agent.
    #[tokio::test]
    async fn a_huge_relay_refusal_is_bounded_before_it_is_persisted() {
        let admin = nostr::Keys::generate();
        let relay = spawn_stub_relay(true, vec![(admin.public_key().to_hex(), "admin")]).await;
        let huge = format!("actor not authorized: {}", "x".repeat(2 * 1024 * 1024));
        *relay.events_refusal.lock().unwrap() = Some(huge.clone());
        let state = state_with_identity(&admin);

        let outcome = ensure_managed_agent_relay_membership(&state, &relay.ws_url, AGENT)
            .await
            .expect("a structured refusal is an outcome, not an error");
        let record =
            membership_record_for_outcome(&outcome, "t".into()).expect("a refusal is persisted");
        let detail = record.detail.unwrap_or_default();
        assert!(
            detail.chars().count() < 1_000,
            "the persisted detail must be bounded, got {} chars",
            detail.chars().count()
        );
        assert!(
            !detail.contains(&"x".repeat(crate::relay::MAX_RELAY_TEXT_CHARS + 1)),
            "the relay's text must be cut, not merely wrapped: {detail}"
        );
        // Bounded, not discarded: the classification and the surviving prefix
        // are still the relay's, so the card still says what it said.
        assert!(
            detail.contains("actor not authorized"),
            "the relay's own words must survive the bound: {detail}"
        );
    }
}
