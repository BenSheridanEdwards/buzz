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
async fn spawn_relay_closed_to_the_desktop() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
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
            get(|| async {
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

/// The reconcile publishes even though its own pre-publish read is refused.
///
/// Built as a blocking test around an explicit runtime so the Tauri mock app
/// is constructed off the async executor, which is where the rest of the
/// desktop's mock-app tests build it.
#[test]
fn a_refused_read_does_not_stop_the_reconcile_from_publishing() {
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
    let (relay, posted) = runtime.block_on(spawn_relay_closed_to_the_desktop());

    // The desktop identity is a stranger to this relay, exactly like the
    // plain-member or unlisted user in the manual recipe.
    let desktop = nostr::Keys::generate();
    let agent = nostr::Keys::generate();
    let state = build_app_state();
    *state.keys.lock().unwrap() = desktop.clone();
    *state.relay_url_override.lock().unwrap() = Some(relay.clone());
    let app = tauri::test::mock_builder()
        .manage(state)
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock app builds headless");
    let state = app.state::<crate::app_state::AppState>();

    let data = reconcile_data(&agent, &relay);
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

    outcome.unwrap_or_else(|error| {
        panic!("a refused read must not abandon the only publisher a legacy agent has: {error}")
    });

    let events = posted.lock().unwrap();
    let profile = events
        .iter()
        .find(|event| event.get("kind").and_then(serde_json::Value::as_u64) == Some(0))
        .unwrap_or_else(|| panic!("no kind:0 was posted; got {events:?}"));
    assert_eq!(
        profile.get("pubkey").and_then(|value| value.as_str()),
        Some(agent.public_key().to_hex().as_str()),
        "the profile is signed by the agent, which is why the desktop's read \
         permission cannot be a precondition for it"
    );
    let content: serde_json::Value = serde_json::from_str(
        profile
            .get("content")
            .and_then(|value| value.as_str())
            .unwrap_or(""),
    )
    .expect("kind:0 content is JSON");
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
