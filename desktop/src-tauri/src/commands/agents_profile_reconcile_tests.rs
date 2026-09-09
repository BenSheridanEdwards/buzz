//! The reconcile pass must publish on a relay that refuses the DESKTOP.
//!
//! `reconcile_agent_profile` is the only publisher a legacy agent has: an
//! agent created before registration existed never runs the create path
//! again, so if the reconcile cannot publish, that agent has no profile ever.
//! Its first step reads the agent's kind:0 as the workspace identity, and on
//! the relay this feature exists for (closed, with the operator having
//! admitted the AGENT and not the desktop) that read is refused on every
//! start. The whole recovery affordance was gated on the one call that can
//! never succeed there.

use std::sync::{Arc, Mutex};

use nostr::ToBech32 as _;

use super::{reconcile_agent_profile, ProfileReconcileData};
use crate::app_state::build_app_state;

/// Stub relay in the shape the PR's own manual recipe produces at step 4.
///
/// `/info` advertises NIP-43 (closed), `/query` answers the bridge's
/// membership refusal for the desktop identity (`api/mod.rs`,
/// `enforce_relay_membership`), `/.well-known/nostr.json` answers an empty
/// names map, and `/events` accepts, because the agent signs its own NIP-98
/// there. Returns the ws URL and everything the relay was actually sent.
/// `status` answers the well-known instead of an empty names map. Buzz's
/// relay always answers 200 there, so a status is an ingress or a proxy
/// answering in its place: the relay did not say anything about handles.
async fn spawn_relay_closed_to_the_desktop(
    status: Option<u16>,
) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    use axum::{http::header::CONTENT_TYPE, http::StatusCode, routing::get, routing::post, Router};

    let posted: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = posted.clone();
    let app = Router::new()
        .route(
            "/info",
            get(|| async {
                (
                    StatusCode::OK,
                    serde_json::json!({ "supported_nips": [1, 42, 43] }).to_string(),
                )
            }),
        )
        .route(
            "/query",
            post(|| async {
                (
                    StatusCode::FORBIDDEN,
                    [(CONTENT_TYPE, "application/json")],
                    serde_json::json!({
                        "error": "relay_membership_required",
                        "message": "You must be a relay member to access this relay"
                    })
                    .to_string(),
                )
            }),
        )
        .route(
            "/.well-known/nostr.json",
            get(move || async move {
                if let Some(code) = status {
                    return (
                        StatusCode::from_u16(code).expect("valid status"),
                        [(CONTENT_TYPE, "application/json")],
                        serde_json::json!({ "error": "upstream unavailable" }).to_string(),
                    );
                }
                (
                    StatusCode::OK,
                    [(CONTENT_TYPE, "application/json")],
                    serde_json::json!({ "names": {}, "relays": {} }).to_string(),
                )
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
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    seen.lock().unwrap().push(event);
                    (
                        StatusCode::OK,
                        [(CONTENT_TYPE, "application/json")],
                        serde_json::json!({ "event_id": id, "accepted": true, "message": "" })
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
    (format!("ws://{addr}"), posted)
}

fn reconcile_data(agent: &nostr::Keys, relay: &str) -> ProfileReconcileData {
    ProfileReconcileData {
        private_key_nsec: agent.secret_key().to_bech32().expect("nsec"),
        name: "Bob".to_string(),
        relay_url: relay.to_string(),
        target_relay_url: Some(relay.to_string()),
        avatar_url: Some("https://example.invalid/bob.png".to_string()),
        auth_tag: None,
        pubkey: agent.public_key().to_hex(),
        agent_command: "goose".to_string(),
        persona_id: None,
        about: None,
    }
}

/// Run one reconcile against a relay closed to the desktop, under an isolated
/// HOME, and return what the relay was sent.
///
/// Built as a blocking helper around an explicit runtime so the Tauri mock app
/// is constructed off the async executor, which is where the rest of the
/// desktop's mock-app tests build it. `seed` runs against the live `AppState`
/// before the reconcile, so a test can put the state the reconcile is supposed
/// to read there through the same production writer the app uses.
fn reconcile_once(
    well_known_status: Option<u16>,
    agent: &nostr::Keys,
    seed: impl FnOnce(&crate::app_state::AppState, &str),
) -> (
    Result<super::ProfileReconcileOutcome, String>,
    String,
    Vec<serde_json::Value>,
) {
    use tauri::Manager as _;

    let _guard = crate::managed_agents::lock_path_mutex();
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let old_home = std::env::var_os("HOME");
    let old_xdg = std::env::var_os("XDG_DATA_HOME");
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_DATA_HOME", &home);

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (relay, posted) = runtime.block_on(spawn_relay_closed_to_the_desktop(well_known_status));

    // The desktop identity is a stranger to this relay, exactly like the
    // plain-member or unlisted user in the manual recipe.
    let desktop = nostr::Keys::generate();
    let state = build_app_state();
    *state.keys.lock().unwrap() = desktop.clone();
    *state.relay_url_override.lock().unwrap() = Some(relay.clone());
    let app = tauri::test::mock_builder()
        .manage(state)
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock app builds headless");
    let state = app.state::<crate::app_state::AppState>();
    seed(&state, &relay);

    let data = reconcile_data(agent, &relay);
    let outcome = runtime.block_on(reconcile_agent_profile(
        &state,
        app.handle(),
        &agent.public_key().to_hex(),
        &data,
    ));

    std::env::remove_var("HOME");
    std::env::remove_var("XDG_DATA_HOME");
    match old_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match old_xdg {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }

    let events = posted.lock().unwrap().clone();
    (outcome, relay, events)
}

/// The `nip05` of the kind:0 the relay was sent, and the whole content.
fn published_profile(events: &[serde_json::Value]) -> serde_json::Value {
    let profile = events
        .iter()
        .find(|event| event.get("kind").and_then(serde_json::Value::as_u64) == Some(0))
        .unwrap_or_else(|| panic!("no kind:0 was posted; got {events:?}"));
    let mut content: serde_json::Value = serde_json::from_str(
        profile
            .get("content")
            .and_then(|value| value.as_str())
            .unwrap_or(""),
    )
    .expect("kind:0 content is JSON");
    content["__pubkey"] = profile
        .get("pubkey")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    content
}

/// The reconcile publishes even though its own pre-publish read is refused.
#[test]
fn a_refused_read_does_not_stop_the_reconcile_from_publishing() {
    let agent = nostr::Keys::generate();
    let (outcome, _relay, events) = reconcile_once(None, &agent, |_, _| {});

    outcome.unwrap_or_else(|error| {
        panic!("a refused read must not abandon the only publisher a legacy agent has: {error}")
    });

    let content = published_profile(&events);
    assert_eq!(
        content.get("__pubkey").and_then(|value| value.as_str()),
        Some(agent.public_key().to_hex().as_str()),
        "the profile is signed by the agent, which is why the desktop's read \
         permission cannot be a precondition for it"
    );
    assert_eq!(
        content
            .get("display_name")
            .or_else(|| content.get("name"))
            .and_then(|value| value.as_str()),
        Some("Bob")
    );
    // The handle is resolved from the relay's own public well-known, which
    // needs no membership, so the agent gets one on this relay too.
    assert!(
        content
            .get("nip05")
            .and_then(|value| value.as_str())
            .is_some_and(|handle| handle.starts_with("bob@")),
        "the handle must still be resolved and carried: {content}"
    );
}

/// The reconcile keeps the handle when every remote source is gone.
///
/// The reconcile is the pass that runs on every start, so it is the one most
/// likely to meet a relay that is still waking up: its `/query` is refused or
/// slower than the client will wait, its well-known cannot answer, and its
/// `/events` accepts anyway. kind:0 is absolute state, so the profile it then
/// publishes DELETES the handle. The last handle this process saw the relay
/// attribute to the agent is the only source left, and the reconcile must
/// consult it exactly as `sync_managed_agent_profile` does; it had its own
/// copy of that expression and only one of the two was wired.
#[test]
fn the_reconcile_keeps_the_handle_when_neither_the_read_nor_the_well_known_can_answer() {
    let agent = nostr::Keys::generate();
    let agent_hex = agent.public_key().to_hex();
    let held: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let seen = held.clone();
    let (outcome, _relay, events) = reconcile_once(Some(502), &agent, move |state, relay| {
        // Written through the production writer, the same one a successful
        // read or publish uses, so this test cannot pass against a seam the
        // app does not have.
        let handle = format!("bob-a1f3@{}", crate::relay::nip05::nip05_domain(relay));
        *seen.lock().unwrap() = handle.clone();
        crate::relay::remember_agent_nip05(state, relay, &agent_hex, &handle);
    });

    outcome.expect("neither read is a gate for the reconcile either");
    let content = published_profile(&events);
    let expected = held.lock().unwrap().clone();
    assert_eq!(
        content.get("nip05").and_then(|value| value.as_str()),
        Some(expected.as_str()),
        "the reconcile must republish the last confirmed handle rather than \
         clearing it: {content}"
    );
}
