use super::*;

/// Response from `POST /events`.
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct SubmitEventResponse {
    pub event_id: String,
    pub accepted: bool,
    pub message: String,
}

/// How the relay answered a submit, for callers that must tell a refusal
/// apart from a transport failure (a refusal is the relay's definitive
/// answer; a transport failure says nothing and must be retried).
#[derive(Debug)]
pub enum SubmitVerdict {
    /// Stored / processed.
    Accepted(SubmitEventResponse),
    /// The relay answered with a structured client-side refusal: HTTP 4xx
    /// with a JSON body, or HTTP 200 with `accepted: false`. `error` is the
    /// caller-facing message [`submit_signed_event_at_with_keys`] returns for
    /// the same response; `refusal` is the relay's own reason, parsed from
    /// the body rather than recovered from the rendered string, so a caller
    /// can classify it (see `managed_agents::relay_membership`).
    Refused {
        error: String,
        refusal: RelayRefusal,
    },
}

/// POST an already-signed event to an explicit relay with an explicit owner.
///
/// Deferred/scoped publication uses this form so a workspace or identity
/// switch cannot retarget either the event or its NIP-98 authentication after
/// the operation captured its `(relay, owner)` scope.
pub async fn submit_signed_event_at_with_keys(
    event: &nostr::Event,
    state: &AppState,
    api_base_url: &str,
    keys: &nostr::Keys,
) -> Result<SubmitEventResponse, String> {
    match submit_signed_event_verdict_at_with_keys(event, state, api_base_url, keys).await? {
        SubmitVerdict::Accepted(response) => Ok(response),
        SubmitVerdict::Refused { error, .. } => Err(error),
    }
}

/// [`submit_signed_event_at_with_keys`] that keeps a structured refusal as an
/// `Ok` verdict. Rate limiting (429), server errors, proxy interceptions and
/// transport failures stay `Err`, exactly as before, so a caller can never
/// mistake an outage for a refusal.
pub async fn submit_signed_event_verdict_at_with_keys(
    event: &nostr::Event,
    state: &AppState,
    api_base_url: &str,
    keys: &nostr::Keys,
) -> Result<SubmitVerdict, String> {
    if event.pubkey != keys.public_key() {
        return Err("signed event does not match the publishing identity".to_string());
    }
    crate::relay_admission::wait_for_rate_limit().await;
    let url = format!("{}/events", api_base_url.trim_end_matches('/'));
    let body_bytes = event.as_json().into_bytes();
    crate::egress_guard::assert_no_key_backup_bytes(&body_bytes, "relay event submit")?;
    let auth_header = build_nip98_auth_header_for_keys(keys, &Method::POST, &url, &body_bytes)?;

    let response = state
        .http_client
        .post(&url)
        .header("Authorization", auth_header)
        .header("Content-Type", "application/json")
        .body(body_bytes)
        .send()
        .await
        .map_err(|e| classify_request_error(&e))?;

    let status = response.status();
    if !status.is_success() {
        // `relay_error_details` populates `refusal` only for a 4xx (never a
        // 429) whose body parsed as the relay's own JSON reason. Everything
        // else (outage, quota, intercepted page, unparseable body) stays an
        // `Err` no caller can mistake for the relay's answer.
        let details = relay_error_details(response).await;
        if let Some(refusal) = details.refusal {
            return Ok(SubmitVerdict::Refused {
                error: details.error,
                refusal,
            });
        }
        return Err(details.error);
    }

    let result: SubmitEventResponse = parse_json_response(response).await?;
    if !result.accepted {
        // The SECOND refusal door, and the one Buzz's own relay actually
        // uses: `api/bridge.rs` answers `POST /events` with HTTP 200 and
        // `{event_id, accepted, message}`, so an ordinary non-rejection
        // refusal never reaches `relay_error_details` and never touched the
        // bound that lives there. `RelayRefusal::new` applies it here too,
        // and it is the only constructor, so this cannot drift again.
        //
        // The relay's `message` is a human sentence, not a machine code, so
        // it goes in the `detail` half. `code_is` compares the machine-code
        // half exactly; a sentence in it would be a category error.
        let refusal = RelayRefusal::new(None, Some(result.message.clone()));
        return Ok(SubmitVerdict::Refused {
            error: format!("relay rejected event: {}", refusal.message()),
            refusal,
        });
    }

    Ok(SubmitVerdict::Accepted(result))
}

/// Sign with an explicit identity and POST the event to an explicit relay.
///
/// The caller owns the signer lifetime. This is important for deferred work:
/// an in-process identity swap cannot retarget the event or its NIP-98 auth
/// after the caller has validated which identity the operation belongs to.
pub async fn submit_event_at_with_keys(
    builder: nostr::EventBuilder,
    state: &AppState,
    api_base_url: &str,
    keys: &nostr::Keys,
) -> Result<SubmitEventResponse, String> {
    let event = builder
        .sign_with_keys(keys)
        .map_err(|e| format!("failed to sign event: {e}"))?;
    submit_signed_event_at_with_keys(&event, state, api_base_url, keys).await
}

/// Build and submit an event to the currently active workspace relay.
pub async fn submit_event(
    builder: nostr::EventBuilder,
    state: &AppState,
) -> Result<SubmitEventResponse, String> {
    let api_base_url = relay_api_base_url_with_override(state);
    let keys = state.signing_keys()?;
    submit_event_at_with_keys(builder, state, &api_base_url, &keys).await
}

/// Sign with an explicit identity, submit to an explicit HTTP API base URL,
/// and also return the signed event's `created_at`.
///
/// Callers that persist a timestamp as an event cursor (e.g. the Projects
/// conversation opener) need the signed event's own second — a
/// post-publication clock read can land a second later and permanently
/// exclude other events stamped in the event's real second.
///
/// The explicit base (rather than a re-read of the workspace override at
/// submit time) matters for the same callers: they validated a tenant scope
/// against the resolved base earlier in the same command, and re-resolving
/// here would reopen the window where a workspace switch retargets the event
/// after the check passed. The explicit `keys` close the sibling window: the
/// relay URL and the signing keys mutate under separate locks during a
/// workspace switch, so re-reading the keys here could sign — and NIP-98
/// authenticate — the event as the *new* tenant's identity after the caller
/// validated the old one. The caller passes the exact snapshot it asserted.
pub async fn submit_event_at_created_at(
    builder: nostr::EventBuilder,
    state: &AppState,
    api_base_url: &str,
    keys: &nostr::Keys,
) -> Result<(SubmitEventResponse, i64), String> {
    let event = builder
        .sign_with_keys(keys)
        .map_err(|e| format!("failed to sign event: {e}"))?;
    let created_at = event.created_at.as_secs() as i64;
    let result = submit_signed_event_at_with_keys(&event, state, api_base_url, keys).await?;
    Ok((result, created_at))
}

/// Like `submit_event_with_keys`, but also returns the signed event's
/// `created_at` — same cursor rationale as [`submit_event_at_created_at`].
pub async fn submit_event_with_keys_created_at(
    builder: nostr::EventBuilder,
    state: &AppState,
    keys: &nostr::Keys,
    auth_tag: Option<&str>,
) -> Result<(SubmitEventResponse, i64), String> {
    let event = builder
        .sign_with_keys(keys)
        .map_err(|e| format!("failed to sign event: {e}"))?;
    let created_at = event.created_at.as_secs() as i64;
    let result = super::submit_signed_event_with_keys(&event, state, keys, auth_tag).await?;
    Ok((result, created_at))
}
