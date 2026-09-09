//! Unit tests for the relay HTTP/command bridge helpers.
//! Extracted from `relay.rs` to keep that module under the file-size ratchet.

use super::{
    build_profile_event, classify_intercepted_response, effective_agent_relay_url,
    extract_retry_in_hint, parse_command_response, relay_http_base_url, MALFORMED_RESPONSE_MESSAGE,
};
use serde::Deserialize;

// ── extract_retry_in_hint ────────────────────────────────────────────────

#[test]
fn extracts_hint_from_429_body() {
    assert_eq!(
        extract_retry_in_hint(r#"{"error":"rate-limited: quota exceeded; retry in 4s"}"#),
        Some(4)
    );
}

#[test]
fn extracts_hint_when_no_json_wrapper() {
    assert_eq!(extract_retry_in_hint("retry in 30s"), Some(30));
}

#[test]
fn returns_none_when_no_hint_present() {
    assert_eq!(
        extract_retry_in_hint(r#"{"error":"rate-limited: quota exceeded"}"#),
        None
    );
    assert_eq!(extract_retry_in_hint(""), None);
}

#[test]
fn overlong_digit_string_returns_none() {
    // A digit sequence that exceeds u64::MAX cannot be parsed; the function
    // must return None (→ caller uses the default) rather than panicking.
    assert_eq!(
        extract_retry_in_hint("retry in 99999999999999999999999s"),
        None
    );
}

// ── relay_error_message: hint capping ────────────────────────────────────
//
// Verify that an oversized relay hint is capped in the returned message
// string, not just inside `activate_rate_limit()`. This guarantees every
// consumer — including the TS gate via `applyTauriRateLimitIfNeeded` —
// receives the capped value rather than the raw untrusted relay value.

#[tokio::test]
async fn oversized_hint_is_capped_in_relay_error_message_string() {
    use crate::relay_admission::{reset_rate_limit_gate, MAX_HINT_SECONDS, TEST_SERIAL};
    use std::io::{Read as _, Write as _};

    let _serial = TEST_SERIAL.lock().await;
    reset_rate_limit_gate();

    // Use a std::net listener on a std::thread — the same pattern as the
    // relay_admission loopback tests. This avoids two races that cause CI
    // failures with tokio::net + into_std():
    //  1. No request read: the client is still sending when the response
    //     arrives → hyper `UnexpectedMessage`/`Canceled` under load.
    //  2. into_std() leaves the socket in nonblocking mode → write_all
    //     may return WouldBlock and silently drop the response.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    // Serve a 429 with a hint far exceeding MAX_HINT_SECONDS (300).
    let oversized = 1_000_000u64;
    let body = format!(r#"{{"error":"rate-limited: quota exceeded; retry in {oversized}s"}}"#);
    let body_len = body.len();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            // Read the request first so the client finishes sending before
            // we write the response — mirrors relay_admission.rs pattern.
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {body_len}\r\nConnection: close\r\n\r\n{body}"
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://{addr}/"))
        .send()
        .await
        .expect("request must succeed");

    let msg = super::relay_error_message(response).await;

    // The message must embed the CAPPED hint, not the raw 1 000 000.
    assert_eq!(
        msg,
        format!("relay rate-limited: retry in {MAX_HINT_SECONDS}s"),
        "relay_error_message must embed the capped hint, not the raw untrusted value"
    );
    assert!(
        !msg.contains(&oversized.to_string()),
        "raw oversized hint must not appear in the message string"
    );
    reset_rate_limit_gate();
}

// ── effective_agent_relay_url: legacy pin ignored ─────────────────────────

#[test]
fn stored_relay_pin_is_ignored() {
    // Zero-touch cutover (#2122): a creation-era per-record relay pin is
    // parsed and persisted but never consulted — the workspace relay wins.
    assert_eq!(
        effective_agent_relay_url("wss://relay.other.com", "wss://staging.example.com"),
        "wss://staging.example.com"
    );
}

#[test]
fn empty_relay_resolves_to_workspace() {
    // A never-set record resolves to the active workspace relay at read-time,
    // so a stale stored default can never make it load-bearing.
    assert_eq!(
        effective_agent_relay_url("", "wss://staging.example.com"),
        "wss://staging.example.com"
    );
}

#[test]
fn whitespace_only_relay_resolves_to_workspace() {
    // Whitespace-only behaves identically — no value survives.
    assert_eq!(
        effective_agent_relay_url("   ", "wss://staging.example.com"),
        "wss://staging.example.com"
    );
}

// ── relay_http_base_url scheme conversion ────────────────────────────────

#[test]
fn loopback_ws_localhost_preserves_authority() {
    // Tenant host-binding keys off the HTTP Host/authority. The desktop must
    // not rewrite localhost to 127.0.0.1, or local dev HTTP calls target a
    // different unmapped community than the WebSocket URL.
    assert_eq!(
        relay_http_base_url("ws://localhost:3000"),
        "http://localhost:3000"
    );
}

#[test]
fn loopback_trailing_slash_removed_authority_preserved() {
    assert_eq!(
        relay_http_base_url("ws://localhost:3000/"),
        "http://localhost:3000"
    );
}

#[test]
fn remote_wss_host_unchanged() {
    assert_eq!(
        relay_http_base_url("wss://relay.example.com"),
        "https://relay.example.com"
    );
}

#[test]
fn loopback_ipv4_literal_unchanged() {
    assert_eq!(
        relay_http_base_url("ws://127.0.0.1:3000"),
        "http://127.0.0.1:3000"
    );
}

#[test]
fn localhost_substring_host_unchanged() {
    assert_eq!(
        relay_http_base_url("ws://localhost.evil.com:3000"),
        "http://localhost.evil.com:3000"
    );
}

#[test]
fn loopback_wss_localhost_preserves_authority() {
    assert_eq!(
        relay_http_base_url("wss://localhost:3000"),
        "https://localhost:3000"
    );
}

// ── classify_intercepted_response ────────────────────────────────────────

#[test]
fn intercepted_cloudflare_host_returns_some() {
    let result = classify_intercepted_response("sqprod.cloudflareaccess.com", "text/html");
    assert!(result.is_some());
    let msg = result.unwrap();
    assert!(
        msg.starts_with("relay unreachable:"),
        "should have unreachable prefix"
    );
    assert!(msg.contains("Cloudflare"), "should mention Cloudflare");
}

#[test]
fn intercepted_cloudflare_apex_host_returns_some() {
    // The apex domain itself should also match.
    let result = classify_intercepted_response("cloudflareaccess.com", "application/json");
    assert!(result.is_some());
    let msg = result.unwrap();
    assert!(msg.starts_with("relay unreachable:"));
    assert!(msg.contains("Cloudflare"));
}

#[test]
fn intercepted_non_cloudflare_html_returns_some() {
    let result =
        classify_intercepted_response("proxy.corporate.example", "text/html; charset=utf-8");
    assert!(result.is_some());
    let msg = result.unwrap();
    assert!(msg.starts_with("relay unreachable:"));
}

#[test]
fn normal_relay_json_returns_none() {
    let result = classify_intercepted_response("relay.myapp.example.com", "application/json");
    assert!(result.is_none());
}

#[test]
fn content_type_case_insensitive() {
    // Uppercase content-type must still be detected.
    let result = classify_intercepted_response("proxy.example.com", "TEXT/HTML");
    assert!(result.is_some());
    assert!(result.unwrap().starts_with("relay unreachable:"));
}

#[test]
fn evil_suffix_does_not_match_cloudflare() {
    // A host whose suffix happens to contain the Cloudflare string but is
    // not actually a subdomain must NOT match.
    let result =
        classify_intercepted_response("notcloudflareaccess.com.evil.example", "application/json");
    assert!(
        result.is_none(),
        "false suffix match should not trigger Cloudflare branch"
    );
}

// classify_request_error requires a real reqwest::Error (not publicly
// constructable) — tested indirectly through integration; skipped here.

// ── /query per-request timeout → classified error ────────────────────────
//
// A stalled `/query` connection (headers never arrive) must not hang the
// caller forever. Both production `/query` builders funnel through
// `send_query_request`, which owns the per-request `.timeout(...)`; this test
// drives that exact helper against a loopback server that accepts the
// connection but never responds. It asserts two things the frontend depends
// on: (1) the helper returns instead of hanging, and (2) the failure is the
// stable `"relay unreachable: request timed out"` classified string.
//
// The outer `tokio::time::timeout` is the regression guard: if the production
// `.timeout(...)` is ever removed from `send_query_request`, this call would
// hang forever, so the guard fires and the test fails fast rather than
// stalling CI. A short 200ms deadline keeps the happy path fast.
#[tokio::test]
async fn stalled_query_request_times_out_with_classified_error() {
    use std::io::Read as _;
    use std::time::Duration;

    // A listener that accepts the connection and then holds it open without
    // ever writing a response — the "headers never arrive" stall.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            // Drain the request but deliberately never respond, then hold
            // the socket until the client aborts on its own timeout.
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            std::thread::sleep(Duration::from_secs(2));
        }
    });

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/query");
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        super::send_query_request(
            &client,
            &url,
            "Nostr test-auth",
            None,
            b"[]".to_vec(),
            Duration::from_millis(200),
        ),
    )
    .await
    .expect(
        "send_query_request must honor its per-request timeout and resolve within 5s; \
         if this guard fires, the production .timeout(...) was lost",
    );

    let err = result
        .expect_err("a stalled /query must surface an error, not succeed")
        .error;

    assert_eq!(
        err, "relay unreachable: request timed out",
        "a timed-out /query must surface the stable classified string"
    );

    let _ = handle.join();
}

// ── /query body-stall timeout → classified error (not malformed) ─────────
//
// `send()` resolves once response headers arrive, so a relay that returns a
// valid 2xx JSON header block and then stalls the body trips the request
// deadline inside `response.json()` — the branch the pre-header stall above
// cannot reach. That is a connectivity failure, not a malformed body, so it
// must surface the stable "relay unreachable: request timed out" string rather
// than the malformed-response bucket. This drives `send_query_request` against
// a loopback that writes headers promising a body it never sends.
#[tokio::test]
async fn stalled_response_body_times_out_with_classified_error() {
    use std::io::{Read as _, Write as _};
    use std::time::Duration;

    // Accept, drain the request, write a complete 2xx JSON header block that
    // promises a body (Content-Length), then send nothing and hold the socket
    // — the "headers arrive, body stalls" half-open case.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 64\r\n\r\n",
            );
            let _ = stream.flush();
            // Never write the promised body; hold past the client deadline.
            std::thread::sleep(Duration::from_secs(2));
        }
    });

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/query");
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        super::send_query_request(
            &client,
            &url,
            "Nostr test-auth",
            None,
            b"[]".to_vec(),
            Duration::from_millis(200),
        ),
    )
    .await
    .expect(
        "send_query_request must honor its per-request timeout through body \
         consumption and resolve within 5s",
    );

    let err = result
        .expect_err("a stalled response body must surface an error, not succeed")
        .error;

    assert_eq!(
        err, "relay unreachable: request timed out",
        "a body-stall timeout must surface the classified timeout string, not the \
         malformed-response bucket"
    );

    let _ = handle.join();
}

// ── /query non-2xx body-stall timeout → classified error (not status) ────
//
// The 2xx path is not the only body-consuming path. A relay that returns a
// non-success status (500, 429, …) routes through `relay_error_message`, which
// consumes the body via `text()` to extract the structured error field. If the
// relay sends the status headers and then stalls the promised body, that
// consumption trips the same request deadline — and it must surface the stable
// "relay unreachable: request timed out" classification, not a bare
// "relay returned 500" that hides the connectivity failure. This drives
// `send_query_request` against a loopback that writes 500 headers promising a
// body it never sends.
#[tokio::test]
async fn stalled_error_response_body_times_out_with_classified_error() {
    use std::io::{Read as _, Write as _};
    use std::time::Duration;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            // 500 status headers promising a body (Content-Length) that never
            // arrives — the "error headers arrive, body stalls" half-open case.
            let _ = stream.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: 64\r\n\r\n",
            );
            let _ = stream.flush();
            std::thread::sleep(Duration::from_secs(2));
        }
    });

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/query");
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        super::send_query_request(
            &client,
            &url,
            "Nostr test-auth",
            None,
            b"[]".to_vec(),
            Duration::from_millis(200),
        ),
    )
    .await
    .expect(
        "send_query_request must honor its per-request timeout through error-body \
         consumption and resolve within 5s",
    );

    let err = result
        .expect_err("a stalled error-response body must surface an error, not succeed")
        .error;

    assert_eq!(
        err, "relay unreachable: request timed out",
        "a non-2xx body-stall timeout must surface the classified timeout string, not the \
         status bucket"
    );

    let _ = handle.join();
}

// ── /query non-stalled 500 → status message (timeout preservation is scoped) ─
//
// The timeout preservation above must not swallow genuine relay errors: a 500
// whose body arrives promptly still surfaces as "relay returned 500". This
// pins that `classify_body_timeout` only fires on an actual timeout, so the
// error-classification path stays intact for live relay failures.
#[tokio::test]
async fn non_stalled_error_response_yields_status_message() {
    use std::io::{Read as _, Write as _};
    use std::time::Duration;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            // A complete 500 with a non-JSON body delivered immediately.
            let body = "internal error";
            let response = format!(
                "HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/query");
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        super::send_query_request(
            &client,
            &url,
            "Nostr test-auth",
            None,
            b"[]".to_vec(),
            Duration::from_millis(200),
        ),
    )
    .await
    .expect("a promptly-served 500 must resolve well within 5s");

    let err = result
        .expect_err("a 500 must surface an error, not succeed")
        .error;

    assert_eq!(
        err, "relay returned 500 Internal Server Error",
        "a non-stalled 500 must keep its status classification, not be reclassified as a timeout"
    );

    let _ = handle.join();
}

// ── parse_json_response malformed-body contract ──────────────────────────

#[test]
fn malformed_response_message_stays_off_unreachable_bucket() {
    // A reached-but-malformed 2xx body is not a connectivity failure. If this
    // message ever regains the "relay unreachable:" prefix, the frontend
    // classifier would misroute it as unreachable — pin that it never does.
    assert!(
        !MALFORMED_RESPONSE_MESSAGE.starts_with("relay unreachable:"),
        "malformed-response message must not match the unreachable prefix"
    );
}

// ── parse_command_response ───────────────────────────────────────────────

#[derive(Debug, Deserialize, PartialEq)]
struct ChannelCreated {
    channel_id: String,
}

#[test]
fn parse_command_response_decodes_typed_payload() {
    let msg = r#"response:{"channel_id":"abc123"}"#;
    let parsed: ChannelCreated = parse_command_response(msg).expect("should parse");
    assert_eq!(
        parsed,
        ChannelCreated {
            channel_id: "abc123".to_string()
        }
    );
}

#[test]
fn parse_command_response_accepts_raw_json_fallback() {
    // Backward-compat: relays that emit raw JSON (no prefix) still work.
    let msg = r#"{"channel_id":"abc"}"#;
    let parsed: ChannelCreated = parse_command_response(msg).expect("fallback parse");
    assert_eq!(
        parsed,
        ChannelCreated {
            channel_id: "abc".to_string()
        }
    );
}

#[test]
fn parse_command_response_rejects_invalid_prefixed_json() {
    let msg = "response:not-json";
    let result: Result<ChannelCreated, _> = parse_command_response(msg);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("response parse failed"));
}

#[test]
fn parse_command_response_rejects_garbage() {
    let msg = "totally not json or response";
    let result: Result<ChannelCreated, _> = parse_command_response(msg);
    assert!(result.is_err());
}

// ── build_profile_event ──────────────────────────────────────────────────

/// Generate a valid NIP-OA auth tag JSON string signed by a fresh owner key
/// and addressed to `agent_keys`.
///
/// Uses `nostr_compat` (nostr 0.36) for the owner keys because
/// `buzz_sdk_pkg::nip_oa::compute_auth_tag` expects nostr 0.36 types.
/// The agent pubkey is bridged via hex encoding.
fn make_valid_auth_tag(agent_keys: &nostr::Keys) -> String {
    let owner_keys = nostr::Keys::generate();
    let agent_pubkey_hex = agent_keys.public_key().to_hex();
    let agent_compat_pubkey =
        nostr::PublicKey::from_hex(&agent_pubkey_hex).expect("valid hex pubkey should parse");
    buzz_sdk_pkg::nip_oa::compute_auth_tag(&owner_keys, &agent_compat_pubkey, "")
        .expect("compute_auth_tag should not fail with distinct keys")
}

#[test]
fn profile_event_with_valid_auth_tag() {
    let agent_keys = nostr::Keys::generate();
    let tag_json = make_valid_auth_tag(&agent_keys);
    let event = build_profile_event(&agent_keys, "TestBot", None, None, None, Some(&tag_json))
        .expect("should succeed with a valid auth tag");

    // Exactly one "auth" tag must be present.
    let auth_tags: Vec<_> = event
        .tags
        .iter()
        .filter(|t| t.as_slice().first().map(|s| s.as_str()) == Some("auth"))
        .collect();
    assert_eq!(auth_tags.len(), 1, "expected exactly 1 auth tag");

    // Must be a kind:0 (Metadata) event.
    assert_eq!(event.kind, nostr::Kind::Metadata);
}

#[test]
fn profile_event_without_auth_tag() {
    let agent_keys = nostr::Keys::generate();
    let event = build_profile_event(&agent_keys, "TestBot", None, None, None, None)
        .expect("should succeed without an auth tag");

    // No "auth" tags should be present.
    let auth_tags: Vec<_> = event
        .tags
        .iter()
        .filter(|t| t.as_slice().first().map(|s| s.as_str()) == Some("auth"))
        .collect();
    assert_eq!(auth_tags.len(), 0, "expected no auth tags");

    assert_eq!(event.kind, nostr::Kind::Metadata);
}

#[test]
fn profile_event_includes_about_when_description_present() {
    let agent_keys = nostr::Keys::generate();
    let event = build_profile_event(
        &agent_keys,
        "TestBot",
        None,
        Some("A meticulous code reviewer."),
        None,
        None,
    )
    .expect("should succeed with an about");
    let content: serde_json::Value =
        serde_json::from_str(&event.content).expect("kind:0 content is JSON");
    assert_eq!(
        content.get("about").and_then(|v| v.as_str()),
        Some("A meticulous code reviewer.")
    );
}

#[test]
fn profile_event_omits_about_when_absent() {
    let agent_keys = nostr::Keys::generate();
    let event = build_profile_event(&agent_keys, "TestBot", None, None, None, None)
        .expect("should succeed without an about");
    let content: serde_json::Value =
        serde_json::from_str(&event.content).expect("kind:0 content is JSON");
    assert!(content.get("about").is_none());
}

#[test]
fn profile_event_carries_nip05_handle_verbatim() {
    let agent_keys = nostr::Keys::generate();
    let event = build_profile_event(
        &agent_keys,
        "TestBot",
        None,
        None,
        Some("testbot@relay.example"),
        None,
    )
    .expect("should succeed with a nip05");
    let content: serde_json::Value =
        serde_json::from_str(&event.content).expect("kind:0 content is JSON");
    assert_eq!(
        content.get("nip05").and_then(|v| v.as_str()),
        Some("testbot@relay.example")
    );
}

#[test]
fn profile_event_rejects_invalid_auth_tag() {
    let agent_keys = nostr::Keys::generate();
    // Structurally valid JSON array but with a bogus signature — verification must fail.
    let bad_json = format!(r#"["auth","{}","","{}"]"#, "a".repeat(64), "b".repeat(128));
    let result = build_profile_event(&agent_keys, "TestBot", None, None, None, Some(&bad_json));
    assert!(result.is_err(), "should reject an invalid auth tag");
    assert!(
        result.unwrap_err().contains("verification failed"),
        "error message should mention verification failure"
    );
}

// ── relay_error_details: the relay's own refusal, parsed once ─────────────
//
// Callers that must classify a refusal (the managed-agent membership
// preflight) read the body's fields, never a rendered string split on ": ".
// A change to how `relay_error_message` renders must not silently change
// what those callers branch on.

/// Answer one request with `status`, `body`, and a JSON content type.
async fn serve_once(status: &'static str, body: String) -> String {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let len = body.len();
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}"
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://{addr}/")
}

#[tokio::test]
async fn a_4xx_json_body_keeps_the_relays_own_error_and_message() {
    // The bridge's membership refusal: a machine reason AND a human sentence.
    let url = serve_once(
        "403 Forbidden",
        r#"{"error":"relay_membership_required","message":"You must be a relay member to access this relay"}"#
            .to_string(),
    )
    .await;
    let response = reqwest::Client::new().get(&url).send().await.unwrap();
    let details = super::relay_error_details(response).await;

    let refusal = details.refusal.expect("a 4xx JSON body is a refusal");
    // Compared against the only constructor: `RelayRefusal`'s fields are
    // private precisely so no door can fill them without the bound.
    assert_eq!(
        refusal,
        super::RelayRefusal::new(
            Some("relay_membership_required".to_string()),
            Some("You must be a relay member to access this relay".to_string()),
        )
    );
    assert_eq!(
        refusal.message(),
        "You must be a relay member to access this relay",
        "the human sentence is what a card shows"
    );
    assert!(
        refusal.haystack().contains("relay_membership_required"),
        "the machine reason must stay reachable for classification"
    );
    // The rendered string is unchanged for every existing caller.
    assert_eq!(
        details.error,
        "relay returned 403 Forbidden: You must be a relay member to access this relay"
    );
}

#[tokio::test]
async fn an_error_only_body_reports_that_error_as_both() {
    // How `api_error` renders every ingest rejection: `error`, no `message`.
    let url = serve_once(
        "400 Bad Request",
        r#"{"error":"invalid: actor not authorized: must be admin or owner"}"#.to_string(),
    )
    .await;
    let response = reqwest::Client::new().get(&url).send().await.unwrap();
    let details = super::relay_error_details(response).await;

    let refusal = details.refusal.expect("a 4xx JSON body is a refusal");
    assert_eq!(
        refusal.message(),
        "invalid: actor not authorized: must be admin or owner",
        "with no human sentence the machine reason is the message"
    );
    assert_eq!(
        refusal,
        super::RelayRefusal::new(
            Some("invalid: actor not authorized: must be admin or owner".to_string()),
            None,
        ),
        "an error-only body leaves the human-sentence half empty"
    );
}

#[tokio::test]
async fn an_outage_is_never_a_refusal() {
    use crate::relay_admission::{reset_rate_limit_gate, TEST_SERIAL};

    // A 5xx says nothing about the request.
    let url = serve_once(
        "500 Internal Server Error",
        r#"{"error":"internal server error"}"#.to_string(),
    )
    .await;
    let response = reqwest::Client::new().get(&url).send().await.unwrap();
    let details = super::relay_error_details(response).await;
    assert!(
        details.refusal.is_none(),
        "a 5xx must never be read as the relay's answer: {details:?}"
    );

    // Neither does a quota window, even though 429 is a 4xx.
    let _serial = TEST_SERIAL.lock().await;
    reset_rate_limit_gate();
    let url = serve_once(
        "429 Too Many Requests",
        r#"{"error":"rate-limited: quota exceeded; retry in 4s"}"#.to_string(),
    )
    .await;
    let response = reqwest::Client::new().get(&url).send().await.unwrap();
    let details = super::relay_error_details(response).await;
    assert!(
        details.refusal.is_none(),
        "a 429 must never be read as the relay's answer: {details:?}"
    );
    reset_rate_limit_gate();
}

// ── Managed-agent profile sync: the pre-publish read is advisory ──────────
//
// Gated off Windows like the other stub-relay tests: `build_app_state()`
// pulls native DLLs unavailable in the Windows CI runner.

#[cfg(not(target_os = "windows"))]
mod profile_sync_stub_relay {
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
    #[derive(Clone, Copy)]
    enum WellKnown {
        /// 200 with an empty `names` map: every handle is free. What Buzz's
        /// own relay answers for an unknown name.
        EmptyNames,
        /// A status Buzz's relay never emits there (`api/nip05.rs` always
        /// answers 200), so it is an ingress, a rewrite or a proxy answering
        /// in the relay's place: the relay did not say anything about
        /// handles.
        Status(u16),
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
        use axum::{
            http::header::CONTENT_TYPE, http::StatusCode, routing::get, routing::post, Router,
        };

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
                        }
                    }
                }),
            )
            .route(
                "/events",
                post(move |body: String| {
                    let seen = seen.clone();
                    async move {
                        let event: serde_json::Value =
                            serde_json::from_str(&body).unwrap_or_default();
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
                get(move || async move {
                    match well_known {
                        WellKnown::EmptyNames => (
                            StatusCode::OK,
                            [(CONTENT_TYPE, "application/json")],
                            serde_json::json!({ "names": {}, "relays": {} }).to_string(),
                        ),
                        WellKnown::Status(code) => (
                            StatusCode::from_u16(code).expect("valid status"),
                            [(CONTENT_TYPE, "application/json")],
                            serde_json::json!({ "error": "upstream unavailable" }).to_string(),
                        ),
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

        crate::relay::sync_managed_agent_profile(
            &state,
            &relay,
            &agent_keys,
            "Bob",
            None,
            None,
            None,
        )
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
        let (relay, posted) =
            spawn_stub_relay(QueryDoor::AnswersEmpty, WellKnown::Status(502)).await;
        let state = crate::app_state::build_app_state();
        let agent_keys = nostr::Keys::generate();

        crate::relay::sync_managed_agent_profile(
            &state,
            &relay,
            &agent_keys,
            "Bob",
            None,
            None,
            None,
        )
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
            let (relay, _) =
                spawn_stub_relay(QueryDoor::AnswersEmpty, WellKnown::Status(status)).await;
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
}
