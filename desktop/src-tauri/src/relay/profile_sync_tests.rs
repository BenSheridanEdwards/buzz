//! Managed-agent profile sync: the pre-publish read is advisory.
//!
//! Split out of `relay/tests.rs` at its own module seam to keep that file
//! under the desktop Rust file-size ratchet. Still `relay::tests`'
//! `profile_sync_stub_relay` module, so every `super::` here still means
//! `relay::tests` and the test paths are unchanged.

use nostr::JsonUtil;
use std::sync::{Arc, Mutex};

/// Stub relay for the "closed to the desktop, open to the agent" shape
/// this PR's own manual recipe produces at step 4.
///
/// `POST /query` answers the bridge's membership refusal verbatim
/// (`api/mod.rs`, `enforce_relay_membership`): the operator ran
/// `buzz-admin add-member --pubkey <agent hex>`, so the AGENT is a member
/// and the desktop identity still is not. `POST /events` accepts, because
/// the agent signs its own NIP-98 there. `/.well-known/nostr.json`
/// answers an empty `names` map, so every handle is free.
///
/// Returns the ws URL and the events the relay actually received.
async fn spawn_relay_closed_to_the_desktop() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    spawn_stub_relay(QueryDoor::RefusesTheDesktop, WellKnown::EmptyNames).await
}

/// How the stub answers a `POST /query`.
#[derive(Clone)]
enum QueryDoor {
    /// The bridge's membership refusal: the operator admitted the agent,
    /// not the desktop.
    RefusesTheDesktop,
    /// A perfectly ordinary empty answer: this agent has no kind:0 yet.
    AnswersEmpty,
    /// The recipe's step-4 relay told apart by WHO is asking. The
    /// operator ran `buzz-admin add-member --pubkey <agent hex>`, so the
    /// relay refuses the desktop identity at the door and answers the
    /// AGENT, which is the whole point of the agent-authenticated
    /// fallback read. Carries the agent's hex and the kind:0 JSON the
    /// relay already holds for it.
    RefusesTheDesktopAnswersTheAgent {
        agent_hex: String,
        existing_kind_0: String,
    },
    /// A relay that answers a non-member's `/query` with `200 []` rather
    /// than a 403, which some proxies and some relay configurations do.
    /// An empty success is not an answer ABOUT the agent, and reading it
    /// as one skipped the agent-authenticated read entirely.
    AnswersTheDesktopNothingAnswersTheAgent {
        agent_hex: String,
        existing_kind_0: String,
    },
    /// [`QueryDoor::RefusesTheDesktopAnswersTheAgent`] until `stalling` is
    /// set, and after that a door that takes longer to answer than
    /// `QUERY_REQUEST_TIMEOUT` allows. A slow relay under load, or an
    /// ingress in front of `/query` only: `/events` still accepts the
    /// publish that the lost read then strips the handle out of.
    RefusesTheDesktopAndStalls {
        agent_hex: String,
        existing_kind_0: String,
        stalling: Arc<std::sync::atomic::AtomicBool>,
    },
}

/// The pubkey that signed a request's NIP-98 `Authorization` header, or
/// an empty string when there is none. The stub tells the desktop and the
/// agent apart exactly the way a real relay does.
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

/// How the stub answers `GET /.well-known/nostr.json`.
#[derive(Clone)]
enum WellKnown {
    /// 200 with an empty `names` map: every handle is free. What Buzz's
    /// own relay answers for an unknown name.
    EmptyNames,
    /// A status Buzz's relay never emits there (`api/nip05.rs` always
    /// answers 200), so it is an ingress, a rewrite or a proxy answering
    /// in the relay's place: the relay did not say anything about
    /// handles.
    Status(u16),
    /// [`WellKnown::EmptyNames`] until `broken` is set, and a 502 after.
    /// Lets one test watch a handle be minted while the endpoint works
    /// and then be republished once it stops.
    EmptyNamesUntilBroken(Arc<std::sync::atomic::AtomicBool>),
}

/// Stub relay for the "closed to the desktop, open to the agent" shape,
/// with each door answered independently so a test can break exactly one.
async fn spawn_stub_relay(
    query: QueryDoor,
    well_known: WellKnown,
) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub relay");
    spawn_stub_relay_on(listener, query, well_known).await
}

/// [`spawn_stub_relay`] on a listener the caller already bound.
///
/// A test that needs the relay's own domain BEFORE the relay exists (to
/// build the handle the relay is supposed to be holding) binds first and
/// serves on the same socket, so there is no unbind/rebind window another
/// process could take the port in.
async fn spawn_stub_relay_on(
    listener: tokio::net::TcpListener,
    query: QueryDoor,
    well_known: WellKnown,
) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    use axum::{http::header::CONTENT_TYPE, http::StatusCode, routing::get, routing::post, Router};

    let posted: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = posted.clone();
    let app = Router::new()
        .route(
            "/query",
            post(move |headers: axum::http::HeaderMap| {
                let query = query.clone();
                async move {
                    let refused = (
                        StatusCode::FORBIDDEN,
                        [(CONTENT_TYPE, "application/json")],
                        serde_json::json!({
                            "error": "relay_membership_required",
                            "message": "You must be a relay member to access this relay"
                        })
                        .to_string(),
                    );
                    match query {
                        QueryDoor::RefusesTheDesktop => refused,
                        QueryDoor::AnswersEmpty => (
                            StatusCode::OK,
                            [(CONTENT_TYPE, "application/json")],
                            "[]".to_string(),
                        ),
                        QueryDoor::RefusesTheDesktopAnswersTheAgent {
                            agent_hex,
                            existing_kind_0,
                        } => {
                            if nip98_sender(&headers) == agent_hex.to_ascii_lowercase() {
                                (
                                    StatusCode::OK,
                                    [(CONTENT_TYPE, "application/json")],
                                    format!("[{existing_kind_0}]"),
                                )
                            } else {
                                refused
                            }
                        }
                        QueryDoor::AnswersTheDesktopNothingAnswersTheAgent {
                            agent_hex,
                            existing_kind_0,
                        } => {
                            if nip98_sender(&headers) == agent_hex.to_ascii_lowercase() {
                                (
                                    StatusCode::OK,
                                    [(CONTENT_TYPE, "application/json")],
                                    format!("[{existing_kind_0}]"),
                                )
                            } else {
                                (
                                    StatusCode::OK,
                                    [(CONTENT_TYPE, "application/json")],
                                    "[]".to_string(),
                                )
                            }
                        }
                        QueryDoor::RefusesTheDesktopAndStalls {
                            agent_hex,
                            existing_kind_0,
                            stalling,
                        } => {
                            if stalling.load(std::sync::atomic::Ordering::Acquire) {
                                // Longer than the client's own deadline,
                                // so the read ends as a timeout and not
                                // as an answer.
                                tokio::time::sleep(
                                    crate::relay::QUERY_REQUEST_TIMEOUT
                                        + std::time::Duration::from_secs(30),
                                )
                                .await;
                            }
                            if nip98_sender(&headers) == agent_hex.to_ascii_lowercase() {
                                (
                                    StatusCode::OK,
                                    [(CONTENT_TYPE, "application/json")],
                                    format!("[{existing_kind_0}]"),
                                )
                            } else {
                                refused
                            }
                        }
                    }
                }
            }),
        )
        .route(
            "/events",
            post(move |body: String| {
                let seen = seen.clone();
                async move {
                    let event: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let id = event
                        .get("id")
                        .and_then(|value| value.as_str())
                        .unwrap_or("")
                        .to_string();
                    seen.lock().unwrap().push(event);
                    (
                        StatusCode::OK,
                        [(CONTENT_TYPE, "application/json")],
                        serde_json::json!({
                            "event_id": id,
                            "accepted": true,
                            "message": ""
                        })
                        .to_string(),
                    )
                }
            }),
        )
        .route(
            "/.well-known/nostr.json",
            get(move || {
                let well_known = well_known.clone();
                async move {
                    let empty = (
                        StatusCode::OK,
                        [(CONTENT_TYPE, "application/json")],
                        serde_json::json!({ "names": {}, "relays": {} }).to_string(),
                    );
                    let broken = |code: u16| {
                        (
                            StatusCode::from_u16(code).expect("valid status"),
                            [(CONTENT_TYPE, "application/json")],
                            serde_json::json!({ "error": "upstream unavailable" }).to_string(),
                        )
                    };
                    match well_known {
                        WellKnown::EmptyNames => empty,
                        WellKnown::Status(code) => broken(code),
                        WellKnown::EmptyNamesUntilBroken(flag) => {
                            if flag.load(std::sync::atomic::Ordering::Acquire) {
                                broken(502)
                            } else {
                                empty
                            }
                        }
                    }
                }
            }),
        );
    let addr = listener.local_addr().expect("stub relay addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("ws://{addr}"), posted)
}

/// The kind:0 must go out even when the relay refuses the desktop's read.
///
/// The pre-publish `query_agent_profile` authenticates as the WORKSPACE
/// identity while the profile event it precedes is signed by the AGENT.
/// Propagating that 403 abandoned the publish, so on the recipe's own
/// "you are not an admin" path the agent never got a name, avatar or
/// handle at all, and no reconcile could fix it because the reconcile
/// does the same read. The read is a probe-order hint, not a safety
/// property: every candidate is confirmed against the relay's own
/// attribution regardless.
#[tokio::test]
async fn a_refused_profile_read_still_publishes_the_agents_kind_0() {
    let (relay, posted) = spawn_relay_closed_to_the_desktop().await;
    let state = crate::app_state::build_app_state();
    let agent_keys = nostr::Keys::generate();

    crate::relay::sync_managed_agent_profile(&state, &relay, &agent_keys, "Bob", None, None, None)
        .await
        .unwrap_or_else(|error| {
            panic!("a refused read must not abandon the agent's own publish: {error}")
        });

    let events = posted.lock().unwrap();
    let profile = events
        .iter()
        .find(|event| event.get("kind").and_then(serde_json::Value::as_u64) == Some(0))
        .unwrap_or_else(|| panic!("no kind:0 was posted; got {events:?}"));
    assert_eq!(
        profile.get("pubkey").and_then(|value| value.as_str()),
        Some(agent_keys.public_key().to_hex().as_str()),
        "the profile is signed by the agent, which is why the desktop's \
         read permission cannot be a precondition for it"
    );
    let content: serde_json::Value = serde_json::from_str(
        profile
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    )
    .expect("kind:0 content is JSON");
    assert_eq!(
        content
            .get("display_name")
            .or_else(|| content.get("name"))
            .and_then(|v| v.as_str()),
        Some("Bob")
    );
    // The handle is still resolved and carried: losing the hint costs
    // probe order, never the handle itself.
    assert!(
        content
            .get("nip05")
            .and_then(|v| v.as_str())
            .is_some_and(|handle| handle.starts_with("bob@")),
        "the handle must still be resolved from the relay: {content}"
    );
}

/// A well-known that cannot answer must not abandon the whole profile.
///
/// The NIP-05 confirmation is a handle question, and every other field of
/// the kind:0 (name, avatar, about) is independent of it. Propagating the
/// lookup failure meant a 502 or a same-origin 301 in front of
/// `/.well-known/` cost the agent its entire profile to protect a handle
/// that, on a freshly created agent, does not exist yet. `/.well-known/`
/// is the path most likely to be rewritten by a proxy, and a 5xx there is
/// ordinary infrastructure, not an attack.
#[tokio::test]
async fn an_unanswerable_well_known_still_publishes_the_agents_kind_0() {
    let (relay, posted) = spawn_stub_relay(QueryDoor::AnswersEmpty, WellKnown::Status(502)).await;
    let state = crate::app_state::build_app_state();
    let agent_keys = nostr::Keys::generate();

    crate::relay::sync_managed_agent_profile(&state, &relay, &agent_keys, "Bob", None, None, None)
        .await
        .unwrap_or_else(|error| {
            panic!("a 502 on the handle lookup must not abandon the profile: {error}")
        });

    let events = posted.lock().unwrap();
    let profile = events
        .iter()
        .find(|event| event.get("kind").and_then(serde_json::Value::as_u64) == Some(0))
        .unwrap_or_else(|| panic!("no kind:0 was posted; got {events:?}"));
    let content: serde_json::Value = serde_json::from_str(
        profile
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    )
    .expect("kind:0 content is JSON");
    assert_eq!(
        content
            .get("display_name")
            .or_else(|| content.get("name"))
            .and_then(|v| v.as_str()),
        Some("Bob"),
        "the fields that owe the well-known nothing must still go out: {content}"
    );
}

/// ...and it must not strip the handle the relay already holds either.
///
/// The two arms used to disagree: a 502 abandoned the publish to protect
/// the handle, while a 404 published a kind:0 with no `nip05` at all,
/// which is absolute state on the relay and therefore deletes it. Both
/// are the same event (the relay did not answer), so both now keep what
/// the agent already carries. 404 is in the table because Buzz's relay
/// answers 200 with an empty map for an unknown name and never 404s that
/// route, so a 404 is a proxy, exactly like the 502.
#[tokio::test]
async fn an_unanswerable_well_known_keeps_the_handle_the_agent_already_carries() {
    let agent = nostr::Keys::generate();
    let agent_hex = agent.public_key().to_hex();
    for status in [404u16, 502, 429] {
        let (relay, _) = spawn_stub_relay(QueryDoor::AnswersEmpty, WellKnown::Status(status)).await;
        let state = crate::app_state::build_app_state();
        let domain = crate::relay::nip05::nip05_domain(&relay);
        let existing = format!("bob-a1f3@{domain}");

        let resolved = crate::relay::nip05::resolve_managed_agent_nip05(
            &state,
            &relay,
            &agent_hex,
            "Bob",
            Some(&existing),
        )
        .await
        .unwrap_or_else(|error| panic!("a {status} must not be a gate: {error}"));

        assert_eq!(
            resolved.as_deref(),
            Some(existing.as_str()),
            "a {status} on the well-known must republish the handle unchanged, never strip it"
        );
    }
}

/// Both reads failing together must still keep the handle, and on the
/// relay this feature exists for, both reads failing together is the
/// NORMAL case, not an edge one.
///
/// The pre-publish kind:0 read authenticates as the WORKSPACE identity,
/// and on a relay closed to the desktop that read is refused on every
/// publish and every reconcile, permanently: the operator ran
/// `buzz-admin add-member --pubkey <agent hex>`, so the agent is a member
/// and the desktop is not. That left `resolve_managed_agent_nip05` with
/// no `existing_handle` at all, so when the well-known ALSO could not
/// answer (same host, so one ingress fault takes both), the fallback had
/// nothing to keep, the kind:0 went out with no `nip05`, and the relay
/// CLEARED the handle it was holding.
///
/// The fix gives the fallback a source the closed relay does admit: ask
/// again as the AGENT. This drives the real `sync_managed_agent_profile`
/// against a stub that refuses the desktop at `/query`, answers the same
/// query for the agent with the kind:0 it already holds, and 502s the
/// well-known.
#[tokio::test]
async fn both_reads_failing_together_still_keeps_the_agents_handle() {
    let agent = nostr::Keys::generate();
    let agent_hex = agent.public_key().to_hex();

    // Bind first so the handle in the relay's stored kind:0 carries this
    // relay's own domain: `unconfirmable_handle` drops a handle from
    // another relay, correctly, and that must not be what this test
    // observes. The same listener is then served on, so there is no
    // window where the port is free.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub relay");
    let addr = listener.local_addr().expect("stub relay addr");
    let existing_handle = format!(
        "bob-a1f3@{}",
        crate::relay::nip05::nip05_domain(&format!("ws://{addr}"))
    );

    let stored = nostr::EventBuilder::new(
        nostr::Kind::Custom(0),
        serde_json::json!({
            "display_name": "Bob",
            "nip05": existing_handle,
        })
        .to_string(),
    )
    .sign_with_keys(&agent)
    .expect("sign the agent's existing kind:0")
    .as_json();

    let (relay, posted) = spawn_stub_relay_on(
        listener,
        QueryDoor::RefusesTheDesktopAnswersTheAgent {
            agent_hex: agent_hex.clone(),
            existing_kind_0: stored,
        },
        WellKnown::Status(502),
    )
    .await;
    let state = crate::app_state::build_app_state();

    crate::relay::sync_managed_agent_profile(&state, &relay, &agent, "Bob", None, None, None)
        .await
        .unwrap_or_else(|error| panic!("neither read is a gate: {error}"));

    let events = posted.lock().unwrap();
    let profile = events
        .iter()
        .find(|event| event.get("kind").and_then(serde_json::Value::as_u64) == Some(0))
        .unwrap_or_else(|| panic!("no kind:0 was posted; got {events:?}"));
    let content: serde_json::Value = serde_json::from_str(
        profile
            .get("content")
            .and_then(|value| value.as_str())
            .unwrap_or(""),
    )
    .expect("kind:0 content is JSON");
    assert_eq!(
        content.get("nip05").and_then(|value| value.as_str()),
        Some(existing_handle.as_str()),
        "kind:0 is absolute state, so a publish without the handle strips \
         it; the agent-authenticated read is what keeps it: {content}"
    );
}

/// Build the kind:0 the stub relay is holding for `agent`, carrying
/// `handle`, as the JSON the door serves.
fn stored_kind_0(agent: &nostr::Keys, handle: &str) -> String {
    nostr::EventBuilder::new(
        nostr::Kind::Custom(0),
        serde_json::json!({
            "display_name": "Bob",
            "nip05": handle,
        })
        .to_string(),
    )
    .sign_with_keys(agent)
    .expect("sign the agent's existing kind:0")
    .as_json()
}

/// The `nip05` of the last kind:0 the stub relay was sent.
fn last_published_handle(
    posted: &Arc<Mutex<Vec<serde_json::Value>>>,
) -> (Option<String>, serde_json::Value) {
    let events = posted.lock().unwrap();
    let profile = events
        .iter()
        .rev()
        .find(|event| event.get("kind").and_then(serde_json::Value::as_u64) == Some(0))
        .unwrap_or_else(|| panic!("no kind:0 was posted; got {events:?}"));
    let content: serde_json::Value = serde_json::from_str(
        profile
            .get("content")
            .and_then(|value| value.as_str())
            .unwrap_or(""),
    )
    .expect("kind:0 content is JSON");
    (
        content
            .get("nip05")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        content,
    )
}

/// A read that is merely SLOW must keep the handle exactly as a refused
/// one does.
///
/// The agent-authenticated fallback gave the handle a second source, but
/// both sources are `/query` on the same host and both are bounded by
/// `QUERY_REQUEST_TIMEOUT`. A relay slow enough to lose both reads still
/// answers `/events`, so the publish lands, and kind:0 is absolute state:
/// the profile goes out with no `nip05` and the relay CLEARS a handle the
/// agent was correctly holding. Silently, because the publish succeeded.
///
/// This drives the real `sync_managed_agent_profile` twice against one
/// stub: once while it answers the agent, which is how the handle becomes
/// known, and once after its `/query` starts taking longer than the
/// client will wait. Time is paused between the two so the second
/// publish pays the 30s deadline in timer ticks rather than in seconds;
/// the client's deadline is the earlier of the two pending timers, so it
/// is the one that fires.
#[tokio::test]
async fn a_timed_out_agent_read_keeps_the_handle_a_refused_one_would_have_kept() {
    let agent = nostr::Keys::generate();
    let agent_hex = agent.public_key().to_hex();

    // Bind first so the stored handle carries this relay's own domain:
    // `unconfirmable_handle` correctly drops a handle from another relay,
    // and that must not be what this test observes.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub relay");
    let addr = listener.local_addr().expect("stub relay addr");
    let existing_handle = format!(
        "bob-a1f3@{}",
        crate::relay::nip05::nip05_domain(&format!("ws://{addr}"))
    );
    let stalling = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (relay, posted) = spawn_stub_relay_on(
        listener,
        QueryDoor::RefusesTheDesktopAndStalls {
            agent_hex: agent_hex.clone(),
            existing_kind_0: stored_kind_0(&agent, &existing_handle),
            stalling: stalling.clone(),
        },
        WellKnown::Status(502),
    )
    .await;
    let state = crate::app_state::build_app_state();

    // First publish: the agent read answers, so the handle is confirmed
    // and republished, exactly as `both_reads_failing_together_...` pins.
    crate::relay::sync_managed_agent_profile(&state, &relay, &agent, "Bob", None, None, None)
        .await
        .expect("the answering relay must publish");
    assert_eq!(
        last_published_handle(&posted).0.as_deref(),
        Some(existing_handle.as_str()),
        "precondition: the answering relay republishes the handle"
    );

    // Now the relay is slow rather than refusing. Both reads run past the
    // deadline; the well-known has been 502ing throughout.
    stalling.store(true, std::sync::atomic::Ordering::Release);
    tokio::time::pause();
    crate::relay::sync_managed_agent_profile(&state, &relay, &agent, "Bob", None, None, None)
        .await
        .expect("a slow read is not a gate either");
    tokio::time::resume();

    let (handle, content) = last_published_handle(&posted);
    assert_eq!(
        handle.as_deref(),
        Some(existing_handle.as_str()),
        "a read that timed out knows nothing about the handle, so the \
         publish must carry the last one the relay confirmed rather than \
         deleting it: {content}"
    );
}

/// A handle learned by a READ alone, with no publish behind it, must survive
/// the same outage.
///
/// The reconcile returns early when the profile is already in sync, so on a
/// steady-state agent the only place the handle is ever observed is that
/// pre-publish read. Remembering it only when a publish carries it left
/// exactly that agent, the settled one, with nothing to fall back on the
/// first time the relay went slow.
#[tokio::test]
async fn a_handle_seen_only_by_a_read_survives_a_later_read_that_cannot_be_completed() {
    let agent = nostr::Keys::generate();
    let agent_hex = agent.public_key().to_hex();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub relay");
    let addr = listener.local_addr().expect("stub relay addr");
    let existing_handle = format!(
        "bob-a1f3@{}",
        crate::relay::nip05::nip05_domain(&format!("ws://{addr}"))
    );
    let stalling = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (relay, posted) = spawn_stub_relay_on(
        listener,
        QueryDoor::RefusesTheDesktopAndStalls {
            agent_hex: agent_hex.clone(),
            existing_kind_0: stored_kind_0(&agent, &existing_handle),
            stalling: stalling.clone(),
        },
        WellKnown::Status(502),
    )
    .await;
    let state = crate::app_state::build_app_state();

    // A read on its own, the shape the already-in-sync reconcile takes: it
    // observes the handle and publishes nothing.
    let seen =
        crate::relay::read_agent_profile_advisory(&state, &relay, Some(&agent), &agent_hex, None)
            .await;
    assert_eq!(
        seen.and_then(|profile| profile.nip05).as_deref(),
        Some(existing_handle.as_str()),
        "precondition: the agent-authenticated read sees the handle"
    );
    assert!(
        posted.lock().unwrap().is_empty(),
        "precondition: nothing was published, so only the read can be the source"
    );

    stalling.store(true, std::sync::atomic::Ordering::Release);
    tokio::time::pause();
    crate::relay::sync_managed_agent_profile(&state, &relay, &agent, "Bob", None, None, None)
        .await
        .expect("a slow read is not a gate");
    tokio::time::resume();

    let (handle, content) = last_published_handle(&posted);
    assert_eq!(
        handle.as_deref(),
        Some(existing_handle.as_str()),
        "a handle the relay showed us once must not be deleted by a publish \
         whose own read never came back: {content}"
    );
}

/// The handle this process MINTED must survive the same outage.
///
/// An agent created here has no kind:0 to read the handle back off until
/// one is published, so the only place its handle is ever known is the
/// publish that mints it. Remembering it only when a READ returns one
/// left exactly that agent, the newest one, with nothing to fall back on:
/// the first slow reconcile after its profile went out stripped the
/// handle it had just been given.
#[tokio::test]
async fn a_minted_handle_survives_a_later_read_that_cannot_be_completed() {
    let agent = nostr::Keys::generate();
    let agent_hex = agent.public_key().to_hex();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub relay");
    let addr = listener.local_addr().expect("stub relay addr");
    let domain = crate::relay::nip05::nip05_domain(&format!("ws://{addr}"));
    // The kind:0 the relay holds carries a name and NO handle, so the
    // read cannot be where the handle comes from.
    let handleless = nostr::EventBuilder::new(
        nostr::Kind::Custom(0),
        serde_json::json!({ "display_name": "Bob" }).to_string(),
    )
    .sign_with_keys(&agent)
    .expect("sign the agent's existing kind:0")
    .as_json();

    let stalling = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let broken = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (relay, posted) = spawn_stub_relay_on(
        listener,
        QueryDoor::RefusesTheDesktopAndStalls {
            agent_hex: agent_hex.clone(),
            existing_kind_0: handleless,
            stalling: stalling.clone(),
        },
        WellKnown::EmptyNamesUntilBroken(broken.clone()),
    )
    .await;
    let state = crate::app_state::build_app_state();

    crate::relay::sync_managed_agent_profile(&state, &relay, &agent, "Bob", None, None, None)
        .await
        .expect("the working relay must publish");
    let minted = format!("bob@{domain}");
    assert_eq!(
        last_published_handle(&posted).0.as_deref(),
        Some(minted.as_str()),
        "precondition: the free slug is minted from the well-known"
    );

    // Every remote source is now gone: both reads run past the deadline
    // and the well-known 502s.
    stalling.store(true, std::sync::atomic::Ordering::Release);
    broken.store(true, std::sync::atomic::Ordering::Release);
    tokio::time::pause();
    crate::relay::sync_managed_agent_profile(&state, &relay, &agent, "Bob", None, None, None)
        .await
        .expect("a slow read is not a gate");
    tokio::time::resume();

    let (handle, content) = last_published_handle(&posted);
    assert_eq!(
        handle.as_deref(),
        Some(minted.as_str()),
        "the handle this process published is the only record of it, so \
         the publish must remember it: {content}"
    );
}

/// An EMPTY success is not an answer about the agent.
///
/// A relay that answers a non-member's `/query` with `200 []` instead of
/// a 403, which some proxies and some relay configurations do, made the
/// workspace read return `Ok(None)`, which short-circuited the
/// agent-authenticated fallback entirely: the handle was stripped exactly
/// as it was before that fallback existed, with no refusal anywhere to
/// explain it. Only a NON-empty answer ends the search now.
#[tokio::test]
async fn an_empty_success_on_the_workspace_read_still_asks_the_agent() {
    let agent = nostr::Keys::generate();
    let agent_hex = agent.public_key().to_hex();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub relay");
    let addr = listener.local_addr().expect("stub relay addr");
    let existing_handle = format!(
        "bob-a1f3@{}",
        crate::relay::nip05::nip05_domain(&format!("ws://{addr}"))
    );
    let (relay, posted) = spawn_stub_relay_on(
        listener,
        QueryDoor::AnswersTheDesktopNothingAnswersTheAgent {
            agent_hex: agent_hex.clone(),
            existing_kind_0: stored_kind_0(&agent, &existing_handle),
        },
        WellKnown::Status(502),
    )
    .await;
    let state = crate::app_state::build_app_state();

    crate::relay::sync_managed_agent_profile(&state, &relay, &agent, "Bob", None, None, None)
        .await
        .expect("an empty read is not a gate");

    let (handle, content) = last_published_handle(&posted);
    assert_eq!(
        handle.as_deref(),
        Some(existing_handle.as_str()),
        "an empty answer to the workspace identity says nothing about the \
         agent, so the agent must still be asked: {content}"
    );
}

/// With nothing to protect, an unanswerable well-known publishes no
/// handle and still publishes everything else. The fallback is "change
/// nothing", not "invent a handle the relay never confirmed".
#[tokio::test]
async fn an_unanswerable_well_known_invents_no_handle_for_a_fresh_agent() {
    let (relay, _) = spawn_stub_relay(QueryDoor::AnswersEmpty, WellKnown::Status(502)).await;
    let state = crate::app_state::build_app_state();
    let agent = nostr::Keys::generate();

    let resolved = crate::relay::nip05::resolve_managed_agent_nip05(
        &state,
        &relay,
        &agent.public_key().to_hex(),
        "Bob",
        None,
    )
    .await
    .expect("an unanswerable lookup is not an error");
    assert_eq!(
        resolved, None,
        "an unconfirmed handle must not be published"
    );
}

/// A handle for another relay's domain is not a handle this relay could
/// attribute to the agent, so the fallback drops it rather than
/// publishing it here.
#[tokio::test]
async fn the_fallback_never_carries_a_handle_from_another_relay() {
    let (relay, _) = spawn_stub_relay(QueryDoor::AnswersEmpty, WellKnown::Status(502)).await;
    let state = crate::app_state::build_app_state();
    let agent = nostr::Keys::generate();

    let resolved = crate::relay::nip05::resolve_managed_agent_nip05(
        &state,
        &relay,
        &agent.public_key().to_hex(),
        "Bob",
        Some("bob@relay.example.com"),
    )
    .await
    .expect("an unanswerable lookup is not an error");
    assert_eq!(resolved, None, "a foreign-domain handle is not republished");
}
