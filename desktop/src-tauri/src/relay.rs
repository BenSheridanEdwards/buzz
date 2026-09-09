use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use nostr::{EventBuilder, JsonUtil, Keys, Kind, Tag};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use sha2::{Digest, Sha256};

// nostr 0.36 alias — required for cross-version bridging with buzz-sdk.

use crate::app_state::AppState;

const DEFAULT_RELAY_WS_URL: &str = "ws://localhost:3000";

// A reached-but-malformed 2xx body is NOT a connectivity failure, so this
// message must never carry the "relay unreachable:" prefix the frontend
// classifier keys on. Extracted to a const so a test can pin that contract.
const MALFORMED_RESPONSE_MESSAGE: &str = "relay returned malformed response: not valid JSON";

// Per-request deadline for the `POST /query` HTTP bridge, covering both the
// header exchange and full body consumption. The shared `http_client` sets no
// client-level timeout — deliberately, because it is also used for long-running
// STT/TTS model downloads, builderlab auth, and the media proxy — so a stalled
// or half-open `/query` connection would otherwise leave the request pending
// forever, hanging the caller (e.g. a thread-history load that never resolves
// and shows a permanent skeleton). A per-request timeout scoped to `/query`
// bounds that without affecting the client's other users. A timeout surfaces
// through `classify_request_error` as the stable `"relay unreachable: request
// timed out"` string. Set above the 25s WS history timeout so a slow-but-live
// relay is not cut off before the WebSocket path would be.
const QUERY_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn configured_env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn relay_ws_url() -> String {
    configured_env_var("BUZZ_RELAY_URL")
        .or_else(|| option_env!("BUZZ_DESKTOP_BUILD_RELAY_URL").map(str::to_string))
        .unwrap_or_else(|| DEFAULT_RELAY_WS_URL.to_string())
}

/// Read the workspace relay URL override, if set. Returns `None` when no
/// override is active or when the mutex is poisoned (best-effort).
pub(crate) fn workspace_relay_override(state: &AppState) -> Option<String> {
    state
        .relay_url_override
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
}

/// Returns the relay WebSocket URL, checking the workspace override first.
/// Precedence: workspace override > env vars > build-time vars > default.
pub fn relay_ws_url_with_override(state: &AppState) -> String {
    workspace_relay_override(state).unwrap_or_else(relay_ws_url)
}

/// Returns the relay HTTP API base URL, checking the workspace override first.
/// Precedence: workspace override > env vars > build-time vars > default.
pub fn relay_api_base_url_with_override(state: &AppState) -> String {
    match workspace_relay_override(state) {
        Some(url) => relay_http_base_url(&url),
        None => relay_api_base_url(),
    }
}

/// Selects the relay a managed agent should use for a relay operation.
///
/// Always the active workspace relay. The legacy per-record `relay_url` pin is
/// deliberately IGNORED (agents-everywhere, #2122): every agent is eligible on
/// every community, and the pair the caller is acting on is identified by the
/// workspace relay, never by a stored pin. The record field is still parsed
/// and persisted untouched — old records need no migration and a rollback to a
/// pin-honoring build reads the same file — so the parameter stays in the
/// signature as documentation of what is being ignored at the one choke point
/// all agent relay resolution flows through. Resolving at read-time also means
/// a stale stored value can never leak into reconcile, spawn, or profile sync.
/// Uniform for both Local and Provider backends.
pub fn effective_agent_relay_url(_record_relay: &str, workspace_relay: &str) -> String {
    workspace_relay.to_string()
}

pub fn relay_http_base_url(relay_url: &str) -> String {
    let trimmed = relay_url.trim().trim_end_matches('/');

    if let Some(suffix) = trimmed.strip_prefix("wss://") {
        return format!("https://{}", suffix);
    }

    if let Some(suffix) = trimmed.strip_prefix("ws://") {
        return format!("http://{}", suffix);
    }

    trimmed.to_string()
}

mod scope;
pub use scope::{
    assert_expected_relay_scope, assert_expected_signer, bind_expected_relay_scope,
    bind_expected_signer, ScopedWorkspaceRelay,
};

pub fn relay_api_base_url() -> String {
    if let Some(base) = configured_env_var("BUZZ_RELAY_HTTP") {
        return base.trim_end_matches('/').to_string();
    }

    if let Some(base) = option_env!("BUZZ_DESKTOP_BUILD_RELAY_HTTP") {
        return base.trim().trim_end_matches('/').to_string();
    }

    relay_http_base_url(&relay_ws_url())
}

// ── NIP-98 HTTP auth ────────────────────────────────────────────────────────

pub fn build_nip98_auth_header(
    method: &Method,
    url: &str,
    body: &[u8],
    state: &AppState,
) -> Result<String, String> {
    let keys = state.keys.lock().map_err(|error| error.to_string())?;
    build_nip98_auth_header_for_keys(&keys, method, url, body)
}

pub fn build_nip98_auth_header_for_keys(
    keys: &Keys,
    method: &Method,
    url: &str,
    body: &[u8],
) -> Result<String, String> {
    let payload_hash = hex::encode(Sha256::digest(body));

    // Nonce ensures unique event IDs even for identical requests in the same second.
    // Without this, rapid-fire calls (e.g. query → submit → re-query) with the same
    // body produce identical NIP-98 event hashes and trigger relay replay detection.
    let nonce_hex = uuid::Uuid::new_v4().to_string();

    let tags = vec![
        Tag::parse(vec!["u", url]).map_err(|error| format!("url tag failed: {error}"))?,
        Tag::parse(vec!["method", method.as_str()])
            .map_err(|error| format!("method tag failed: {error}"))?,
        Tag::parse(vec!["payload", &payload_hash])
            .map_err(|error| format!("payload tag failed: {error}"))?,
        Tag::parse(vec!["nonce", &nonce_hex])
            .map_err(|error| format!("nonce tag failed: {error}"))?,
    ];

    let event = EventBuilder::new(Kind::HttpAuth, "")
        .tags(tags)
        .sign_with_keys(keys)
        .map_err(|error| format!("sign failed: {error}"))?;

    Ok(format!(
        "Nostr {}",
        BASE64.encode(event.as_json().as_bytes())
    ))
}

// ── Error handling ──────────────────────────────────────────────────────────

/// Classify a `send()` failure into a stable, URL-free error string.
///
/// The returned string always starts with `"relay unreachable:"` so the
/// frontend connectivity classifier can detect it with a simple prefix check.
pub(crate) fn classify_request_error(e: &reqwest::Error) -> String {
    let display = e.to_string().to_lowercase();
    if e.is_timeout() {
        "relay unreachable: request timed out".to_string()
    } else if e.is_connect() {
        "relay unreachable: could not connect to relay".to_string()
    } else if display.contains("dns") || display.contains("failed to lookup") {
        "relay unreachable: relay host not found".to_string()
    } else {
        "relay unreachable: network error".to_string()
    }
}

/// Preserve a body-consumption timeout as the stable connectivity classification.
///
/// `send()` resolves once response headers arrive, so a body that stalls past
/// the request deadline trips the timeout during body consumption rather than
/// at `send()`. That is a connectivity failure, not a malformed body or a plain
/// status error. Both body-consumption paths — the 2xx `parse_json_response`
/// and the non-2xx `relay_error_message` — route their consumption error
/// through this one helper so a stalled body can never be classified as
/// "request timed out" on one path while the other buries it under a malformed
/// or status label. Returns `Some("relay unreachable: request timed out")` for
/// a timeout; `None` otherwise, leaving the caller to apply its own non-timeout
/// label.
fn classify_body_timeout(e: &reqwest::Error) -> Option<String> {
    e.is_timeout().then(|| classify_request_error(e))
}

/// Detect responses that were intercepted by a captive portal or auth proxy.
///
/// Returns `Some(msg)` when the response clearly did not come from the relay:
/// - Cloudflare Access redirect (final URL on `*.cloudflareaccess.com`)
/// - Any other HTML response (proxy login page, captive portal, etc.)
///
/// Pure function: takes the already-extracted host and content-type strings so
/// it can be unit-tested without constructing a real `reqwest::Response`.
fn classify_intercepted_response(final_host: &str, content_type: &str) -> Option<String> {
    let host = final_host.to_lowercase();
    let ct = content_type.to_lowercase();

    // Cloudflare Access intercepts requests and redirects to its own domain.
    // Label-boundary check prevents `notcloudflareaccess.com.evil.example` from
    // matching.
    if host == "cloudflareaccess.com" || host.ends_with(".cloudflareaccess.com") {
        return Some(
            "relay unreachable: network sign-in required (Cloudflare Access / VPN) \
             — re-authenticate and reconnect"
                .to_string(),
        );
    }

    // Generic HTML body from any other proxy or captive portal.
    if ct.contains("text/html") {
        return Some(
            "relay unreachable: relay returned an unexpected HTML page \
             (VPN or proxy sign-in?)"
                .to_string(),
        );
    }

    None
}

/// Deserialize a successful response as JSON, guarding against intercepted pages.
///
/// Extracts the final URL host and `Content-Type` header before consuming the
/// response body. If the response looks like a captive-portal page, returns the
/// appropriate `"relay unreachable:"` message instead of attempting JSON parsing.
/// URL details are deliberately omitted from error strings so raw URLs are never
/// surfaced in the UI.
pub(crate) async fn parse_json_response<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, String> {
    let final_host = response.url().host_str().unwrap_or("").to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if let Some(msg) = classify_intercepted_response(&final_host, &content_type) {
        return Err(msg);
    }

    // A successful HTTP response whose body fails to deserialize means the relay
    // was reached but returned something unexpected (protocol mismatch, relay bug,
    // corrupted body) — NOT a connectivity failure. Keep it off the
    // "relay unreachable:" bucket so it surfaces loudly instead of being treated
    // as a transient unreachable-relay condition. The reqwest error detail is
    // dropped because it contains the raw URL.
    //
    // A body-consumption timeout is the exception: `send()` resolves once
    // headers arrive, so a body that stalls past the request deadline trips the
    // timeout HERE rather than at send(). That is a connectivity failure, not a
    // malformed body, so route it through `classify_body_timeout` — the same
    // helper the non-2xx error-body path uses — to preserve the stable
    // "relay unreachable: request timed out" label.
    response.json::<T>().await.map_err(|e| {
        classify_body_timeout(&e).unwrap_or_else(|| MALFORMED_RESPONSE_MESSAGE.to_string())
    })
}

/// Extract the `retry in Ns` hint from a rate-limit error string.
///
/// Matches the canonical format emitted by the relay in both HTTP 429 bodies
/// and CLOSED/NOTICE messages: `quota exceeded; retry in 4s`.
fn extract_retry_in_hint(body: &str) -> Option<u64> {
    let re_match = body.find("retry in ")?;
    let after = &body[re_match + "retry in ".len()..];
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u64>().ok()
}

/// The relay's own reason for refusing a request, parsed once from the JSON
/// body it answered with.
///
/// The relay writes two different fields: `error` is the machine-readable
/// reason a caller can branch on (`relay_membership_required` from the HTTP
/// bridge's membership gate, `invalid: actor not authorized: ...` from
/// `handlers/relay_admin.rs`), and `message` is the human sentence it
/// sometimes adds. Callers that must classify a refusal read `code`; callers
/// that show it to a user read `message()`. Neither re-parses a rendered
/// string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayRefusal {
    /// The body's `error` field, verbatim.
    pub code: Option<String>,
    /// The body's `message` field, verbatim.
    pub detail: Option<String>,
}

/// Longest run of relay-authored text kept from a refusal, in characters.
///
/// The relay writes `error` and `message` and nothing upstream bounds the
/// body: `relay_error_details` reads it with `text()`, and this branch is the
/// first thing to *persist* it. `membership_record_for_outcome` writes the
/// refusal into `relay-membership.json` (one row per agent/relay pair, on a
/// fleet that multiplies) and the card renders it on every summary pass. A
/// relay answering megabytes would put megabytes there. Bound it at
/// construction so every consumer inherits the bound: [`RelayRefusal::message`],
/// [`RelayRefusal::haystack`], the rendered `relay returned <status>: <text>`
/// string, and the `*_detail` copy that embeds it.
pub const MAX_RELAY_TEXT_CHARS: usize = 200;

/// `text` cut to [`MAX_RELAY_TEXT_CHARS`] characters on a char boundary, with
/// a marker so a reader can tell the relay said more. Counted in `char`s, not
/// bytes, so a multi-byte body can never split a code point.
fn bound_relay_text(text: &str) -> String {
    match text.char_indices().nth(MAX_RELAY_TEXT_CHARS) {
        None => text.to_string(),
        Some((cut, _)) => format!("{}... (truncated)", &text[..cut]),
    }
}

impl RelayRefusal {
    /// The text to show a user: the human `message` when the relay sent one,
    /// else the machine reason.
    pub fn message(&self) -> String {
        self.detail
            .clone()
            .or_else(|| self.code.clone())
            .unwrap_or_default()
    }

    /// Everything the relay said, lowercased, for classification.
    pub fn haystack(&self) -> String {
        format!(
            "{} {}",
            self.code.as_deref().unwrap_or(""),
            self.detail.as_deref().unwrap_or("")
        )
        .to_ascii_lowercase()
    }
}

/// A non-2xx relay response, read once: the caller-facing string every
/// existing call site already used, plus the relay's structured refusal when
/// the answer was a client-side refusal it is safe to act on.
#[derive(Debug, Clone)]
pub struct RelayErrorDetails {
    /// Exactly what [`relay_error_message`] returns.
    pub error: String,
    /// Present only for a 4xx (never 429) whose body parsed as JSON with an
    /// `error` or `message` field. A 429, a 5xx, an intercepted page and a
    /// body that is not structured JSON all leave this `None`, so a caller
    /// can never mistake an outage for the relay's definitive answer.
    pub refusal: Option<RelayRefusal>,
}

pub async fn relay_error_message(response: reqwest::Response) -> String {
    relay_error_details(response).await.error
}

/// [`relay_error_message`] that also keeps the relay's structured refusal.
pub async fn relay_error_details(response: reqwest::Response) -> RelayErrorDetails {
    let status = response.status();

    // Check for intercepted/proxy responses before reading the body.
    let final_host = response.url().host_str().unwrap_or("").to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if let Some(msg) = classify_intercepted_response(&final_host, &content_type) {
        return RelayErrorDetails {
            error: msg,
            refusal: None,
        };
    }

    // Real relay error: extract the structured message field if available.
    // `text()` consumes the body, which — like the 2xx path — can trip the
    // request deadline if the relay sends status headers then stalls the body.
    // Preserve that timeout as the stable connectivity classification via the
    // shared helper instead of letting `unwrap_or_default` swallow it into a
    // bare status label. A non-timeout body error still degrades to an empty
    // body → status-only message, exactly as before.
    let body = match response.text().await {
        Ok(body) => body,
        Err(e) => {
            if let Some(timeout) = classify_body_timeout(&e) {
                return RelayErrorDetails {
                    error: timeout,
                    refusal: None,
                };
            }
            String::new()
        }
    };

    // 429 Too Many Requests → typed `relay rate-limited:` prefix so the TS
    // client can activate the rate-limit gate without confusing it with a
    // connectivity failure (`relay unreachable:`). Also arm the Rust-side
    // admission gate here — the one place every relay HTTP error funnels
    // through — so the next relay-backed command waits out the quota window
    // instead of burning it (see `relay_admission`).
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let hint = extract_retry_in_hint(&body);
        // Clamp the hint to MAX_HINT_SECONDS before arming the Rust gate AND
        // before embedding it in the returned string. Every consumer (Rust gate
        // via `activate_rate_limit` and TS gate via `applyTauriRateLimitIfNeeded`)
        // must see the same capped value — a single policy point prevents the TS
        // gate from receiving an uncapped hint from an untrusted relay.
        let capped_hint = hint.map(|s| s.min(crate::relay_admission::MAX_HINT_SECONDS));
        crate::relay_admission::activate_rate_limit(capped_hint);
        let error = match capped_hint {
            Some(secs) => format!("relay rate-limited: retry in {secs}s"),
            None => "relay rate-limited: quota exceeded".to_string(),
        };
        // Deliberately no `refusal`: a quota window says nothing about the
        // request itself, so no caller may treat it as the relay's answer.
        return RelayErrorDetails {
            error,
            refusal: None,
        };
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
        // Bounded HERE, at the one place a refusal is built, so the cap
        // cannot be applied on one consumer and dropped from another: the
        // rendered `error` string below is composed from these same two
        // values, and so is everything the sidecar persists.
        let code = value
            .get("error")
            .and_then(serde_json::Value::as_str)
            .map(bound_relay_text);
        let detail = value
            .get("message")
            .and_then(serde_json::Value::as_str)
            .map(bound_relay_text);
        if code.is_some() || detail.is_some() {
            // Message first in the rendered string, unchanged: it is the
            // sentence written for a human when the relay sends both.
            let rendered = detail.as_deref().or(code.as_deref()).unwrap_or_default();
            return RelayErrorDetails {
                error: format!("relay returned {status}: {rendered}"),
                // Only a client-side refusal is the relay's definitive answer
                // about this request. A 5xx is an outage and must stay one.
                refusal: status
                    .is_client_error()
                    .then_some(RelayRefusal { code, detail }),
            };
        }
    }

    // Non-JSON, non-HTML body: emit status only — no raw body in the UI.
    RelayErrorDetails {
        error: format!("relay returned {status}"),
        refusal: None,
    }
}

// ── HTTP bridge: POST /query ────────────────────────────────────────────────

/// Execute a one-shot query via the relay's HTTP bridge (`POST /query`).
///
/// Filters are serialized as a JSON array. The request is authenticated with
/// a NIP-98 event signed by the user's keys. Returns the deserialized array of
/// events.
pub async fn query_relay(
    state: &AppState,
    filters: &[serde_json::Value],
) -> Result<Vec<nostr::Event>, String> {
    query_relay_at(state, &relay_api_base_url_with_override(state), filters).await
}

/// Like [`query_relay`] but targets an explicit HTTP API base URL instead of
/// the workspace override. Used when a query must hit a specific relay (e.g.
/// reconciling an agent's profile on the relay where it was published).
pub async fn query_relay_at(
    state: &AppState,
    api_base_url: &str,
    filters: &[serde_json::Value],
) -> Result<Vec<nostr::Event>, String> {
    query_relay_details_at(state, api_base_url, filters)
        .await
        .map_err(|details| details.error)
}

/// [`query_relay_at`] that keeps the relay's structured refusal.
///
/// A closed relay enforces membership on `/query` before it looks at the
/// filters (`api/bridge.rs`, `query_events_authed`), so a 403 here is the
/// relay's answer about the *querying identity*, not a transport failure.
/// The membership preflight needs that distinction; every other caller wants
/// the flat string and uses [`query_relay_at`].
///
/// Sent on the no-redirect [`AppState::relay_query_client`]: the membership
/// roster this returns is read as the relay's own assertion about who may
/// publish there, so a third origin must not be able to supply it.
pub async fn query_relay_details_at(
    state: &AppState,
    api_base_url: &str,
    filters: &[serde_json::Value],
) -> Result<Vec<nostr::Event>, RelayErrorDetails> {
    crate::relay_admission::wait_for_rate_limit().await;
    let url = format!("{}/query", api_base_url);
    let body_bytes = serde_json::to_vec(filters).map_err(|e| RelayErrorDetails {
        error: format!("filter serialization failed: {e}"),
        refusal: None,
    })?;
    let auth = build_nip98_auth_header(&Method::POST, &url, &body_bytes, state).map_err(|e| {
        RelayErrorDetails {
            error: e,
            refusal: None,
        }
    })?;
    send_query_request(
        &state.relay_query_client,
        &url,
        &auth,
        None,
        body_bytes,
        QUERY_REQUEST_TIMEOUT,
    )
    .await
}

pub async fn query_relay_at_with_keys(
    state: &AppState,
    api_base_url: &str,
    filters: &[serde_json::Value],
    keys: &Keys,
    auth_tag: Option<&str>,
) -> Result<Vec<nostr::Event>, String> {
    crate::relay_admission::wait_for_rate_limit().await;
    let url = format!("{}/query", api_base_url);
    let body_bytes =
        serde_json::to_vec(filters).map_err(|e| format!("filter serialization failed: {e}"))?;
    let auth = build_nip98_auth_header_for_keys(keys, &Method::POST, &url, &body_bytes)?;
    send_query_request(
        &state.relay_query_client,
        &url,
        &auth,
        auth_tag,
        body_bytes,
        QUERY_REQUEST_TIMEOUT,
    )
    .await
    .map_err(|details| details.error)
}

/// Issue an authenticated `POST /query` and parse the response, applying the
/// per-request `timeout` that bounds a stalled or half-open relay connection.
///
/// Both `/query` builders funnel through this one helper so the timeout can
/// never be applied to one builder and dropped from the other, and so a test
/// can drive the real send/timeout/classify path with a short deadline against
/// a stalled loopback. A timeout surfaces through `classify_request_error` as
/// the stable `"relay unreachable: request timed out"` string.
///
/// It is also the one place a 3xx is refused. Both builders send on the
/// no-redirect [`AppState::relay_query_client`], so the redirect arrives here
/// verbatim instead of being followed: a query answered by another origin is
/// not the relay's answer, and the NIP-43 roster read on top of this helper
/// turns a forged one into a permanent, uncheckable `Member`.
async fn send_query_request(
    http_client: &reqwest::Client,
    url: &str,
    auth: &str,
    auth_tag: Option<&str>,
    body_bytes: Vec<u8>,
    timeout: std::time::Duration,
) -> Result<Vec<nostr::Event>, RelayErrorDetails> {
    let mut request = http_client
        .post(url)
        .header("Authorization", auth)
        .header("Content-Type", "application/json")
        .timeout(timeout);
    if let Some(tag) = auth_tag {
        request = request.header("x-auth-tag", tag);
    }
    let response = request
        .body(body_bytes)
        .send()
        .await
        .map_err(|e| RelayErrorDetails {
            error: classify_request_error(&e),
            refusal: None,
        })?;
    // Checked BEFORE the non-success branch: a 3xx is not a success, so
    // without this it would be read as a relay error rather than as "another
    // origin was asked to answer", and the message would not say so. No
    // `refusal` either: a redirect is nothing the relay said about the
    // request, so the membership caller keeps the row `Unknown` and retries.
    if response.status().is_redirection() {
        return Err(RelayErrorDetails {
            error: format!(
                "the relay query was redirected off the relay ({}), so the relay did not answer it",
                response.status()
            ),
            refusal: None,
        });
    }
    if !response.status().is_success() {
        return Err(relay_error_details(response).await);
    }
    parse_json_response(response)
        .await
        .map_err(|error| RelayErrorDetails {
            error,
            refusal: None,
        })
}

// ── Command response parsing ────────────────────────────────────────────────

/// Parse a command-event OK message of the form `"response:<json>"`.
///
/// Buzz's command kinds (e.g. 41010, 30620, 46020) acknowledge writes via
/// relay OK messages whose payload is a `response:`-prefixed JSON document.
/// This helper strips the prefix and deserializes the remainder as `T`.
pub fn parse_command_response<T: DeserializeOwned>(message: &str) -> Result<T, String> {
    // Try the spec format first: "response:{...}".
    if let Some(json) = message.strip_prefix("response:") {
        return serde_json::from_str(json).map_err(|e| format!("response parse failed: {e}"));
    }
    // Fallback: raw JSON (backward compat for relays that omit the prefix).
    serde_json::from_str(message)
        .map_err(|e| format!("expected 'response:' prefix or valid JSON, got: {message} ({e})"))
}

// ── Profile event builder ───────────────────────────────────────────────────

/// Build a signed kind:0 profile event, optionally injecting a verified NIP-OA auth tag.
///
/// This is a pure function (no I/O) extracted from `sync_managed_agent_profile` so that
/// the event-building and auth-tag-injection logic can be unit tested without HTTP calls.
///
/// `buzz-sdk` uses `nostr 0.36` while the desktop crate uses `nostr 0.37`. Cross-version
/// bridging is done via hex-encoded public keys and raw tag slices — both versions share the
/// same wire format.
fn build_profile_event(
    agent_keys: &nostr::Keys,
    display_name: &str,
    avatar_url: Option<&str>,
    about: Option<&str>,
    nip05: Option<&str>,
    auth_tag_json: Option<&str>,
) -> Result<nostr::Event, String> {
    let builder = crate::events::build_profile(Some(display_name), None, avatar_url, about, nip05)?;

    let builder = if let Some(tag_json) = auth_tag_json {
        // Bridge nostr 0.37 PublicKey → nostr 0.36 PublicKey via hex encoding.
        let agent_pubkey_hex = agent_keys.public_key().to_hex();
        let compat_pubkey = nostr::PublicKey::from_hex(&agent_pubkey_hex)
            .map_err(|e| format!("failed to convert agent pubkey for auth verification: {e}"))?;

        // Verify Schnorr signature before injecting into profile event.
        buzz_sdk_pkg::nip_oa::verify_auth_tag(tag_json, &compat_pubkey)
            .map_err(|e| format!("auth tag verification failed for profile event: {e}"))?;

        // parse_auth_tag returns a nostr 0.36 Tag; bridge to nostr 0.37 via raw slice.
        let compat_tag = buzz_sdk_pkg::nip_oa::parse_auth_tag(tag_json)
            .map_err(|e| format!("failed to parse verified auth tag: {e}"))?;
        let tag = nostr::Tag::parse(compat_tag.as_slice())
            .map_err(|e| format!("failed to convert auth tag to nostr 0.37: {e}"))?;
        builder.tags([tag])
    } else {
        builder
    };

    builder
        .sign_with_keys(agent_keys)
        .map_err(|e| format!("failed to sign profile event: {e}"))
}

// ── Managed-agent profile sync ──────────────────────────────────────────────

/// Sync a managed agent's kind:0 profile event to the relay using NIP-98 auth.
///
/// The agent signs its own profile event and the NIP-98 HTTP-auth event, so no
/// API token is required. `about` carries the agent's authored public
/// description (see `managed_agents::record_effective_description`); the
/// relay treats kind:0
/// fields as absolute, so passing `None` clears any previously published about.
pub async fn sync_managed_agent_profile(
    state: &AppState,
    relay_url: &str,
    agent_keys: &nostr::Keys,
    display_name: &str,
    avatar_url: Option<&str>,
    about: Option<&str>,
    auth_tag: Option<&str>, // NIP-OA auth tag JSON
) -> Result<(), String> {
    // Egress guard BEFORE any network round trip: the NIP-05 resolution
    // below talks to the relay, and a key backup in the profile text must be
    // refused without ever leaving the device.
    for (bytes, context) in [
        (display_name.as_bytes(), "agent profile sync"),
        (about.unwrap_or("").as_bytes(), "agent profile sync"),
    ] {
        crate::egress_guard::assert_no_key_backup_bytes(bytes, context)?;
    }
    // kind:0 is absolute state on the relay, so EVERY managed-agent profile
    // publish must carry the handle or the relay clears it. The existing
    // handle is read back first so reconciles prefer whatever the agent
    // already carries, subject to the relay confirming it still attributes
    // that handle to this agent (see `nip05::resolve_managed_agent_nip05`).
    //
    // ADVISORY, never a gate. This read authenticates as the WORKSPACE
    // identity, while the kind:0 it precedes is signed by the AGENT's own
    // keys. On a closed relay those are different subjects: the operator
    // running `buzz-admin add-member --pubkey <agent hex>` admits the agent
    // and not the desktop, so a plain-member or unlisted user's `/query` is
    // refused with a 403 while the agent's own `/events` publish would
    // succeed. Propagating that refusal abandoned the publish and the agent
    // never got a name, avatar or handle at all, with no reconcile that could
    // ever fix it because the reconcile does the same read.
    //
    // Losing the hint is safe by construction: after the contested-handle
    // fix, `existing` decides nothing but the probe ORDER
    // (`nip05_probe_order`), and every candidate including that one is
    // confirmed against the relay's own attribution before it is returned.
    // The one cost is churn: an agent holding `bob-a1f3` while `bob` is free
    // moves to `bob` on a publish whose read failed. A missing profile beats
    // a stable one.
    //
    // On the relay this exists for, that is not a transient loss. A relay
    // closed to the desktop identity refuses this read on EVERY publish and
    // every reconcile, for as long as the user is not a member, so the hint
    // is absent for the whole life of the pair and the move to the plain slug
    // is guaranteed rather than rare. It is still one-way (the plain slug is
    // the first candidate, so nothing moves back), it happens once, it lands
    // on the nicer handle, and mentions bind pubkeys rather than handles, so
    // a moved handle breaks no reference.
    let agent_pubkey = agent_keys.public_key().to_hex();
    let existing = match query_agent_profile(state, relay_url, &agent_pubkey).await {
        Ok(profile) => profile.and_then(|info| info.nip05),
        Err(error) => {
            eprintln!(
                "buzz-desktop: could not read {agent_pubkey} kind:0 before profile sync, \
                 publishing without the existing-handle hint: {error}"
            );
            None
        }
    };
    let nip05 = nip05::resolve_managed_agent_nip05(
        state,
        relay_url,
        &agent_pubkey,
        display_name,
        existing.as_deref(),
    )
    .await?;
    sync_managed_agent_profile_with_nip05(
        state,
        relay_url,
        agent_keys,
        display_name,
        avatar_url,
        about,
        nip05.as_deref(),
        auth_tag,
    )
    .await
}

/// [`sync_managed_agent_profile`] with an already-resolved NIP-05 handle.
/// Callers that computed the handle to decide whether a sync is needed at
/// all (profile reconciliation) use this to publish exactly what they
/// compared against.
#[allow(clippy::too_many_arguments)]
pub async fn sync_managed_agent_profile_with_nip05(
    state: &AppState,
    relay_url: &str,
    agent_keys: &nostr::Keys,
    display_name: &str,
    avatar_url: Option<&str>,
    about: Option<&str>,
    nip05: Option<&str>,
    auth_tag: Option<&str>, // NIP-OA auth tag JSON
) -> Result<(), String> {
    crate::relay_admission::wait_for_rate_limit().await;
    // Build a signed kind:0 profile event (with optional NIP-OA auth tag).
    let event = build_profile_event(agent_keys, display_name, avatar_url, about, nip05, auth_tag)?;
    let event_json = event.as_json();
    let body_bytes = event_json.into_bytes();
    crate::egress_guard::assert_no_key_backup_bytes(&body_bytes, "agent profile sync")?;

    let url = format!("{}/events", relay_http_base_url(relay_url));
    let auth = build_nip98_auth_header_for_keys(agent_keys, &Method::POST, &url, &body_bytes)?;

    let mut request = state
        .http_client
        .post(&url)
        .header("Authorization", auth)
        .header("Content-Type", "application/json");
    if let Some(tag) = auth_tag {
        request = request.header("x-auth-tag", tag);
    }
    let response = request
        .body(body_bytes)
        .send()
        .await
        .map_err(|e| classify_request_error(&e))?;

    if !response.status().is_success() {
        let msg = relay_error_message(response).await;
        return Err(format!(
            "Could not sync the agent's profile metadata: {msg}"
        ));
    }

    Ok(())
}

// ── Agent profile query ─────────────────────────────────────────────────────

/// Query the relay for an agent's kind:0 profile event.
///
/// Queries the relay identified by `relay_url`. Callers uniformly pass the
/// relay resolved by `effective_agent_relay_url` for every agent regardless of
/// backend — always the active workspace relay — so the query targets the host
/// the profile is actually published to.
///
/// Returns the parsed profile content (display_name, picture, about) if a
/// kind:0 event exists for the given pubkey, or `None` if no profile is
/// published.
pub async fn query_agent_profile(
    state: &AppState,
    relay_url: &str,
    agent_pubkey: &str,
) -> Result<Option<AgentProfileInfo>, String> {
    let filter = serde_json::json!({
        "authors": [agent_pubkey],
        "kinds": [0],
        "limit": 1
    });

    let events = query_relay_at(state, &relay_http_base_url(relay_url), &[filter]).await?;

    let Some(event) = events.first() else {
        return Ok(None);
    };

    let Ok(content) = serde_json::from_str::<serde_json::Value>(&event.content) else {
        return Ok(None);
    };

    Ok(Some(AgentProfileInfo {
        display_name: content
            .get("display_name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        picture: content
            .get("picture")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        about: content
            .get("about")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        nip05: content
            .get("nip05")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string),
    }))
}

/// Parsed fields from a kind:0 profile event.
#[derive(Debug, Clone)]
pub struct AgentProfileInfo {
    pub display_name: Option<String>,
    pub picture: Option<String>,
    /// Published public description (kind:0 `about`).
    pub about: Option<String>,
    /// Published NIP-05 handle (`local@relay-host`), verbatim.
    pub nip05: Option<String>,
}

// ── NIP-11 membership advertisement ─────────────────────────────────────────

/// Whether the relay at `http_base_url` advertises NIP-43 (relay membership)
/// in its NIP-11 document. A closed relay advertises it; an open relay does
/// not, and no membership work is needed there.
///
/// The answer must actually BE a NIP-11 document: `supported_nips` has to be
/// present and an array, or this is an error rather than "the relay is open".
/// A `#[serde(default)]` here made any 200 JSON object (an ingress error
/// page like `{"error":"backend starting"}`, a relay mid-restart, a proxy's
/// own JSON) deserialize to an empty NIP list and read as `OpenRelay`, the one
/// outcome that CLEARS the agent's membership sidecar row, registers nothing
/// and leaves no card at all. That is the same end state the redirect guard
/// below exists to prevent, reachable with no redirect involved. An error
/// keeps the row `Unknown` and the next start retries.
///
/// Sent on the no-redirect [`AppState::relay_meta_client`], and any 3xx is an
/// error rather than an answer. "This relay is open" is a claim only the
/// relay may make about itself: a redirect target whose `supported_nips`
/// omits 43 would make a closed relay read as `OpenRelay`, which clears the
/// agent's membership sidecar row, registers nothing, and leaves it unable to
/// publish with no card state at all. An error keeps the row as `Unknown` and
/// the next start retries.
pub async fn relay_advertises_membership_at(
    state: &AppState,
    http_base_url: &str,
) -> Result<bool, String> {
    let url = format!("{}/info", http_base_url.trim_end_matches('/'));
    let response = state
        .relay_meta_client
        .get(url)
        .header("Accept", "application/nostr+json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|error| classify_request_error(&error))?;

    if response.status().is_redirection() {
        return Err(format!(
            "the relay information document was redirected off the relay ({}), so the relay did not answer it",
            response.status()
        ));
    }
    if !response.status().is_success() {
        return Err(relay_error_message(response).await);
    }

    let document = parse_json_response::<serde_json::Value>(response).await?;
    let Some(supported_nips) = document
        .get("supported_nips")
        .and_then(serde_json::Value::as_array)
    else {
        return Err(
            "the relay answered /info with something that is not a NIP-11 document \
             (no supported_nips list), so it did not say whether it is open"
                .to_string(),
        );
    };
    // A stringly-typed `"43"` counts too. NIP-11 says numbers and Buzz's own
    // relay emits numbers, so this only fires for a non-conforming document,
    // and "membership is advertised" is the safe reading of an ambiguous one:
    // it keeps the row instead of clearing it.
    Ok(supported_nips
        .iter()
        .any(|nip| nip.as_u64() == Some(43) || nip.as_str() == Some("43")))
}

// ── Signed-event submission ─────────────────────────────────────────────────

mod get;
pub use get::get_relay_json;

pub mod nip05;

mod submit;
pub use submit::{
    submit_event, submit_event_at_created_at, submit_event_at_with_keys,
    submit_event_with_keys_created_at, submit_signed_event_at_with_keys,
    submit_signed_event_verdict_at_with_keys, SubmitEventResponse, SubmitVerdict,
};

/// Sign an event with explicit keys and POST it to `/events` with NIP-98 auth.
///
/// Managed-agent flows use this to publish as the agent itself while still
/// including the stored NIP-OA auth tag when the relay requires owner-backed
/// membership.
pub async fn submit_event_with_keys(
    builder: nostr::EventBuilder,
    state: &AppState,
    keys: &Keys,
    auth_tag: Option<&str>,
) -> Result<SubmitEventResponse, String> {
    let event = builder
        .sign_with_keys(keys)
        .map_err(|e| format!("failed to sign event: {e}"))?;
    submit_signed_event_with_keys(&event, state, keys, auth_tag).await
}

/// POST an already-signed event using the same explicit identity for NIP-98.
pub async fn submit_signed_event_with_keys(
    event: &nostr::Event,
    state: &AppState,
    keys: &Keys,
    auth_tag: Option<&str>,
) -> Result<SubmitEventResponse, String> {
    if event.pubkey != keys.public_key() {
        return Err("signed event does not match the publishing identity".to_string());
    }
    crate::relay_admission::wait_for_rate_limit().await;
    let url = format!("{}/events", relay_api_base_url_with_override(state));
    let body_bytes = event.as_json().into_bytes();
    crate::egress_guard::assert_no_key_backup_bytes(&body_bytes, "signed event submit (keys)")?;
    let auth_header = build_nip98_auth_header_for_keys(keys, &Method::POST, &url, &body_bytes)?;

    let mut request = state
        .http_client
        .post(&url)
        .header("Authorization", auth_header)
        .header("Content-Type", "application/json");
    if let Some(tag) = auth_tag {
        request = request.header("x-auth-tag", tag);
    }

    let response = request
        .body(body_bytes)
        .send()
        .await
        .map_err(|e| classify_request_error(&e))?;

    if !response.status().is_success() {
        return Err(relay_error_message(response).await);
    }

    let result: SubmitEventResponse = parse_json_response(response).await?;

    if !result.accepted {
        return Err(format!("relay rejected event: {}", result.message));
    }

    Ok(result)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
