//! Transcribe an inbound voice note and publish the words as their own event.
//!
//! A voice note only shows a transcript when its author put one in the imeta
//! `alt` field. An agent can do that for its own replies, because it writes
//! the tag as it publishes. A person cannot: the composer has no speech-to-text
//! and the credential that would do it lives with the agents, so a human's
//! voice note reaches the relay with nothing readable attached.
//!
//! Nothing can add a tag to that message afterwards either. The author signed
//! it, so amending it would break the signature. The transcript therefore
//! travels as a separate `KIND_VOICE_NOTE_TRANSCRIPT` event tagged `e` with the
//! note's id, which clients render on that message.
//!
//! Transcription is delegated to Hermes over loopback rather than called
//! directly. Hermes owns the subscription credential and refreshes it, and its
//! endpoint filters provider hallucinations, so this holds no credential and
//! reaches nothing beyond `127.0.0.1`.
//!
//! Every failure is silent and total: no transcript event is published and the
//! turn proceeds exactly as it did before.

use std::time::Duration;

/// Largest clip sent for transcription.
///
/// A long recording would hold the turn open on someone else's latency, and
/// the transcript is a convenience rather than part of the answer.
const MAX_CLIP_BYTES: u64 = 25 * 1024 * 1024;

/// How long to wait on Hermes before giving up on the transcript.
/// Ceiling on one transcription call. Detached from the reply, so it can be
/// generous: the Hermes endpoint took 36s on a 4.5s clip when its provider
/// token was being refreshed, and a 30s ceiling threw the words away.
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(120);

/// Whether this attachment should be transcribed at all.
///
/// Split from the request so each refusal is observable in a test without a
/// network call: folded into the request path, a deleted guard and a failed
/// request are indistinguishable, both yielding `None`.
pub fn should_transcribe(endpoint: &str, is_audio: bool, size: u64) -> bool {
    if endpoint.trim().is_empty() {
        return false;
    }
    if !is_audio {
        return false;
    }
    if size == 0 || size > MAX_CLIP_BYTES {
        return false;
    }
    true
}

/// Scope the call to a Hermes profile, whose subscription pays for it.
///
/// Built by hand so an endpoint that already carries a query string is
/// extended rather than broken.
pub fn endpoint_url(endpoint: &str, profile: &str) -> String {
    let profile = profile.trim();
    if profile.is_empty() {
        return endpoint.to_string();
    }
    let separator = if endpoint.contains('?') { '&' } else { '?' };
    format!("{endpoint}{separator}profile={profile}")
}

/// Ask Hermes for the words in `path`.
///
/// Returns `None` on every failure path, including a clip Hermes heard as
/// silence: an empty transcript would render a blank row rather than no row.
pub async fn transcribe_clip(
    endpoint: &str,
    profile: &str,
    token: &str,
    path: &std::path::Path,
    mime_type: &str,
) -> Option<String> {
    use base64::Engine as _;

    let bytes = tokio::fs::read(path).await.ok()?;
    let data_url = format!(
        "data:{};base64,{}",
        mime_type,
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    );

    let client = reqwest::Client::builder()
        .timeout(TRANSCRIBE_TIMEOUT)
        .build()
        .ok()?;

    let mut request = client.post(endpoint_url(endpoint, profile));
    // Hermes gates every /api/ route on this token, loopback included.
    if !token.trim().is_empty() {
        request = request.bearer_auth(token.trim());
    }
    let response = request
        .json(&serde_json::json!({ "data_url": data_url, "mime_type": mime_type }))
        .send()
        .await
        .map_err(|error| {
            // Warn: a timeout or a refused call is the difference between a
            // note with its words and a note without, and it has to be
            // findable in the log.
            tracing::warn!(target: "buzz_acp::media", %error, "inbound transcript: request failed")
        })
        .ok()?;

    if !response.status().is_success() {
        tracing::warn!(
            target: "buzz_acp::media",
            status = %response.status(),
            "inbound transcript: endpoint declined the clip"
        );
        return None;
    }

    let body: serde_json::Value = response.json().await.ok()?;
    let text = body.get("transcript").and_then(|v| v.as_str())?;
    normalize(text)
}

/// Flatten to one line and bound it; whitespace-only becomes `None`.
///
/// The transcript rides in event content that clients render inline, so a
/// provider's line breaks are folded and a runaway result is cut rather than
/// pushed at the relay's event size limit.
fn normalize(text: &str) -> Option<String> {
    const MAX_TRANSCRIPT_BYTES: usize = 1000;
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    if flat.len() <= MAX_TRANSCRIPT_BYTES {
        return Some(flat);
    }
    let mut end = MAX_TRANSCRIPT_BYTES;
    while end > 0 && !flat.is_char_boundary(end) {
        end -= 1;
    }
    Some(flat[..end].trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_endpoint_refuses() {
        assert!(!should_transcribe("   ", true, 1024));
    }

    #[test]
    fn a_configured_endpoint_accepts_audio() {
        assert!(should_transcribe("http://127.0.0.1:9119/x", true, 1024));
    }

    #[test]
    fn a_non_audio_attachment_is_never_transcribed() {
        assert!(!should_transcribe("http://127.0.0.1:9119/x", false, 1024));
    }

    #[test]
    fn an_empty_or_oversized_clip_is_refused_but_one_at_the_cap_is_not() {
        let e = "http://127.0.0.1:9119/x";
        assert!(!should_transcribe(e, true, 0));
        assert!(!should_transcribe(e, true, MAX_CLIP_BYTES + 1));
        assert!(should_transcribe(e, true, MAX_CLIP_BYTES));
    }

    #[test]
    fn profile_scoping_appends_and_respects_an_existing_query() {
        assert_eq!(endpoint_url("http://h/api", ""), "http://h/api");
        assert_eq!(
            endpoint_url("http://h/api", " sky "),
            "http://h/api?profile=sky"
        );
        assert_eq!(
            endpoint_url("http://h/api?x=1", "echo"),
            "http://h/api?x=1&profile=echo"
        );
    }

    #[test]
    fn normalize_folds_newlines_into_one_line() {
        assert_eq!(
            normalize("hello\nthere  chief\n").as_deref(),
            Some("hello there chief")
        );
    }

    #[test]
    fn silence_produces_no_transcript_rather_than_a_blank_one() {
        assert!(normalize("   \n\t ").is_none());
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let text = "é".repeat(600);
        let cut = normalize(&text).unwrap();
        assert!(cut.len() <= 1000);
        assert!(cut.chars().all(|c| c == 'é'));
    }
}

/// Live end-to-end checks against a running Hermes transcribe endpoint.
///
/// Ignored by default: they need a Hermes web server on loopback and a real
/// clip, so they are opt-in rather than part of the unit run. Drive them with
/// `BUZZ_ACP_E2E_ENDPOINT`, `BUZZ_ACP_E2E_TOKEN`, `BUZZ_ACP_E2E_PROFILE` and
/// `BUZZ_ACP_E2E_CLIP`, then `cargo test -p buzz-acp e2e_ -- --ignored`.
#[cfg(test)]
mod e2e {
    use super::*;

    fn env(name: &str) -> String {
        std::env::var(name).unwrap_or_default()
    }

    #[tokio::test]
    #[ignore = "needs a running Hermes transcribe endpoint"]
    async fn e2e_transcribes_a_real_clip_through_hermes() {
        let endpoint = env("BUZZ_ACP_E2E_ENDPOINT");
        let clip = env("BUZZ_ACP_E2E_CLIP");
        assert!(!endpoint.is_empty() && !clip.is_empty(), "e2e env not set");

        let transcript = transcribe_clip(
            &endpoint,
            &env("BUZZ_ACP_E2E_PROFILE"),
            &env("BUZZ_ACP_E2E_TOKEN"),
            std::path::Path::new(&clip),
            "audio/mpeg",
        )
        .await
        .expect("a real clip must produce a transcript");

        let lowered = transcript.to_lowercase();
        assert!(
            lowered.contains("voice note") && lowered.contains("came through"),
            "transcript did not carry the spoken words: {transcript}"
        );
        // The publisher's contract: one line, bounded.
        assert!(!transcript.contains('\n'), "transcript was not flattened");
        assert!(transcript.len() <= 1000, "transcript exceeded the cap");
    }

    #[tokio::test]
    #[ignore = "needs a running Hermes transcribe endpoint"]
    async fn e2e_a_missing_token_is_refused_rather_than_transcribed() {
        let endpoint = env("BUZZ_ACP_E2E_ENDPOINT");
        let clip = env("BUZZ_ACP_E2E_CLIP");
        assert!(!endpoint.is_empty() && !clip.is_empty(), "e2e env not set");

        // Hermes gates /api/ even on loopback; without the token the call must
        // yield no transcript rather than silently succeeding.
        let out = transcribe_clip(
            &endpoint,
            &env("BUZZ_ACP_E2E_PROFILE"),
            "",
            std::path::Path::new(&clip),
            "audio/mpeg",
        )
        .await;
        assert!(out.is_none(), "an unauthenticated call must not transcribe");
    }

    /// The relay half of the pipeline, against a real relay: a transcript for
    /// a note is stored and comes back with the channel window's aux closure,
    /// it never counts as a reply, and one dressed up as a reply is refused.
    ///
    /// Needs `BUZZ_ACP_E2E_RELAY_URL` (http(s) base), `BUZZ_ACP_E2E_PRIVATE_KEY`
    /// (a relay member) and `BUZZ_ACP_E2E_CHANNEL_ID` (a channel that member
    /// can post in). Point it at the test relay, never production.
    #[tokio::test]
    #[ignore = "needs a running relay and a member key"]
    async fn e2e_relay_stores_a_transcript_as_an_annotation_not_a_reply() {
        let relay = env("BUZZ_ACP_E2E_RELAY_URL");
        let key = env("BUZZ_ACP_E2E_PRIVATE_KEY");
        let channel = env("BUZZ_ACP_E2E_CHANNEL_ID");
        assert!(
            !relay.is_empty() && !key.is_empty() && !channel.is_empty(),
            "e2e env not set"
        );
        let channel_id = uuid::Uuid::parse_str(&channel).expect("channel id");
        let rest = crate::relay::RestClient {
            http: reqwest::Client::new(),
            base_url: relay.trim_end_matches('/').to_string(),
            keys: nostr::Keys::parse(&key).expect("member key"),
            auth_tag_json: None,
        };

        let note = buzz_sdk::build_message(
            channel_id,
            "e2e stand-in for a voice note",
            None,
            &[],
            false,
            &[],
            &[],
        )
        .expect("note builder")
        .sign_with_keys(&rest.keys)
        .expect("sign note");
        rest.submit_event(&note)
            .await
            .expect("the note is accepted");

        let transcript = buzz_sdk::build_voice_note_transcript(
            channel_id,
            note.id,
            "hello from the relay round trip",
        )
        .expect("transcript builder")
        .sign_with_keys(&rest.keys)
        .expect("sign transcript");
        rest.submit_event(&transcript)
            .await
            .expect("the transcript is accepted");

        let disguised = nostr::EventBuilder::new(
            nostr::Kind::Custom(buzz_core::kind::KIND_VOICE_NOTE_TRANSCRIPT as u16),
            "not a reply",
        )
        .tags([
            nostr::Tag::parse(["h", &channel]).unwrap(),
            nostr::Tag::parse(["e", &note.id.to_hex(), "", "reply"]).unwrap(),
        ])
        .sign_with_keys(&rest.keys)
        .expect("sign disguised");
        let refused = rest.submit_event(&disguised).await;
        assert!(
            matches!(&refused, Err(e) if e.to_string().contains("400")),
            "a transcript with a reply marker must be refused: {refused:?}"
        );

        let window = rest
            .query_raw(&[serde_json::json!({
                "#h": [channel],
                "kinds": [buzz_core::kind::KIND_STREAM_MESSAGE],
                "limit": 5,
                "top_level": true,
                "include_aux": true,
                "include_summaries": true,
            })])
            .await
            .expect("channel window");
        let events = window.as_array().expect("window is an array");
        let transcript_hex = transcript.id.to_hex();
        assert!(
            events.iter().any(|e| e["id"] == transcript_hex),
            "the aux closure must carry the transcript"
        );
        let note_hex = note.id.to_hex();
        let counted_as_thread = events.iter().any(|e| {
            e["kind"] == buzz_core::kind::KIND_THREAD_SUMMARY
                && e["tags"]
                    .as_array()
                    .is_some_and(|tags| tags.iter().any(|t| t[0] == "e" && t[1] == note_hex))
        });
        assert!(
            !counted_as_thread,
            "a transcript must not give the note a thread summary"
        );
    }
}
