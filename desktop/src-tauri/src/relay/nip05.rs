//! NIP-05 handle derivation for managed agents.
//!
//! The relay only reads a handle from the kind:0 `nip05` field, requires it
//! to be `local@<relay host>` (host without port, see the relay's
//! `canonicalize_nip05` / `extract_domain`), and keeps it unique per
//! community with a UNIQUE column. A contested handle is silently dropped by
//! the relay, so the desktop must pick a free one BEFORE publishing.
//!
//! Derivation is deterministic from the agent's display name and pubkey:
//! `slug`, then `slug-<4 hex of pubkey>`, then `slug-<8 hex of pubkey>`. A
//! handle already on the agent's kind:0 is *preferred* as long as it is one
//! of those candidates, so reconciles never flip-flop between candidates and
//! a rename is the only thing that changes a handle. It is preferred, not
//! trusted: the kind:0 is the agent's own copy and the relay drops a
//! contested handle silently on its UNIQUE conflict, so the handle is
//! confirmed against what the relay actually attributes before it is kept.

use crate::app_state::AppState;

/// Longest SLUG the desktop derives from a display name. NIP-05 has no hard
/// cap but short handles keep `@mentions` and the well-known lookup readable.
///
/// This is not the cap on a local part: [`nip05_candidates`] appends
/// `-<4 or 8 hex>` after the slug is capped, so a derived local part reaches
/// 41 characters. Capping the candidate instead would truncate the pubkey
/// suffix that makes it unique, which is the one thing about it that must
/// survive.
const NIP05_LOCAL_MAX_LEN: usize = 32;

/// Fallback local part when the display name slugifies to nothing.
const NIP05_LOCAL_FALLBACK: &str = "agent";

/// Per-request bound for the public well-known lookup.
const NIP05_LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Domain part of a handle on `relay_url`: the host, lowercased, without the
/// port. Mirrors the relay's `extract_domain`, which strips the port from the
/// bound tenant host before comparing.
///
/// Empty when no handle can work on this relay, which the caller turns into
/// "publish no handle". A bracketed IPv6 literal is one such case: the
/// relay's `extract_domain` (`crates/buzz-relay/src/api/nip05.rs`) splits the
/// authority on `:` before anything else, so it derives `[` from
/// `[::1]:3000` and would never match a handle the desktop could write.
pub fn nip05_domain(relay_url: &str) -> String {
    let trimmed = relay_url.trim();
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let authority = without_scheme.split('/').next().unwrap_or("");
    let host = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    if host.starts_with('[') {
        return String::new();
    }
    host.split(':').next().unwrap_or("").to_ascii_lowercase()
}

/// Latin letters NFKD does not decompose, and the ASCII they are conventionally
/// transliterated to. These are ligatures and letters with a stroke or a bar
/// rather than a combining accent, so NFKD leaves them whole and the slugifier
/// drops them: without this map `Ægir` becomes `gir`, `Łukasz` becomes
/// `ukasz`, `Søren` becomes `s-ren` and `Straße` becomes `stra-e`. Applied
/// before the fold, so a decomposition never has to see them.
///
/// Deliberately small: it covers the European names the fold was added for,
/// not every script. A name outside it still slugifies to the `agent`
/// fallback, which the pubkey suffix disambiguates.
const ASCII_TRANSLITERATIONS: &[(char, &str)] = &[
    ('Æ', "AE"),
    ('æ', "ae"),
    ('Ø', "O"),
    ('ø', "o"),
    ('Ł', "L"),
    ('ł', "l"),
    ('Đ', "D"),
    ('đ', "d"),
    ('Ð', "D"),
    ('ð', "d"),
    ('ß', "ss"),
    ('Þ', "TH"),
    ('þ', "th"),
    ('Œ', "OE"),
    ('œ', "oe"),
];

/// Fold a display name onto its ASCII skeleton before slugification.
///
/// [`crate::util::slugify`] maps every non-ASCII character to the separator,
/// which loses letters rather than punctuation: `Zoë` becomes `zo`. Two steps
/// fix that. First the undecomposable Latin letters above are transliterated,
/// because NFKD has nothing to split them into. Then NFKD splits a precomposed
/// letter into its base plus a combining mark, so dropping the combining marks
/// keeps the base letter and `Zoë` becomes `zoe`. Characters outside both
/// (`東`) are unchanged and still slugify to separators, leaving the `agent`
/// fallback for a name with no ASCII letters at all. The pubkey suffix
/// disambiguates those.
fn fold_to_ascii_skeleton(display_name: &str) -> String {
    use unicode_normalization::char::is_combining_mark;
    use unicode_normalization::UnicodeNormalization;

    let transliterated: String = display_name
        .chars()
        .map(|character| {
            ASCII_TRANSLITERATIONS
                .iter()
                .find(|(from, _)| *from == character)
                .map_or_else(|| character.to_string(), |(_, to)| (*to).to_string())
        })
        .collect();

    transliterated
        .nfkd()
        .filter(|character| !is_combining_mark(*character))
        .collect()
}

/// Slugified local part for `display_name`.
pub fn nip05_local_part(display_name: &str) -> String {
    crate::util::slugify(
        &fold_to_ascii_skeleton(display_name),
        NIP05_LOCAL_FALLBACK,
        NIP05_LOCAL_MAX_LEN,
    )
}

/// Candidate local parts in preference order: the bare slug, then the slug
/// with a short pubkey-derived suffix, then a longer one. The suffix comes
/// from the agent's own pubkey so it is stable across reconciles without
/// persisting anything.
pub fn nip05_candidates(display_name: &str, agent_pubkey_hex: &str) -> Vec<String> {
    let slug = nip05_local_part(display_name);
    let pubkey = agent_pubkey_hex.to_ascii_lowercase();
    let mut candidates = vec![slug.clone()];
    for suffix_len in [4usize, 8] {
        let suffix: String = pubkey.chars().take(suffix_len).collect();
        if suffix.is_empty() {
            continue;
        }
        candidates.push(format!("{slug}-{suffix}"));
    }
    candidates
}

/// Split `local@domain` into its parts, lowercased.
fn split_handle(handle: &str) -> Option<(String, String)> {
    let (local, domain) = handle.trim().split_once('@')?;
    if local.is_empty() || domain.is_empty() {
        return None;
    }
    Some((local.to_ascii_lowercase(), domain.to_ascii_lowercase()))
}

/// The local part of a handle already on the agent's kind:0, when it is one
/// of the candidates derived from the CURRENT name on the SAME domain. This
/// is the stability rule: a reconcile never swaps `bob-a1f3` for `bob` just
/// because `bob` freed up, and never touches a handle on another relay.
///
/// The answer is a *preference*, not a decision: the caller still confirms
/// the handle against the relay's own attribution before publishing it.
pub fn stable_existing_local(
    existing: Option<&str>,
    candidates: &[String],
    domain: &str,
) -> Option<String> {
    let (local, existing_domain) = split_handle(existing?)?;
    if existing_domain != domain {
        return None;
    }
    candidates.contains(&local).then_some(local)
}

/// Candidate local parts in the order they should be probed: the handle the
/// agent already carries first (stability), then the derived candidates in
/// preference order, each exactly once.
pub fn nip05_probe_order(
    existing: Option<&str>,
    candidates: &[String],
    domain: &str,
) -> Vec<String> {
    let stable = stable_existing_local(existing, candidates, domain);
    stable
        .iter()
        .cloned()
        .chain(
            candidates
                .iter()
                .filter(|candidate| Some(*candidate) != stable.as_ref())
                .cloned(),
        )
        .collect()
}

/// Who owns a local part on the relay, per its public NIP-05 endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Nip05Lookup {
    /// No user holds this local part.
    Free,
    /// Held by the given hex pubkey (possibly this agent).
    OwnedBy(String),
    /// The relay does not serve NIP-05 (404), so handles are meaningless there.
    Unsupported,
}

/// Public `GET /.well-known/nostr.json?name=<local>` on the relay. No auth:
/// the endpoint is the NIP-05 discovery door itself.
///
/// Sent on the no-redirect [`AppState::relay_meta_client`], and any 3xx is an
/// error rather than an answer. This lookup is the *authority* on who holds a
/// handle, so a redirect to another origin must not be able to supply it: a
/// third origin claiming the name would push the agent off a handle it owns,
/// and one answering an empty `names` map would make the desktop publish a
/// handle the relay attributes to somebody else, which the relay silently
/// drops on its UNIQUE index and which is exactly the stranded state this
/// confirmation exists to prevent.
pub async fn lookup_nip05_owner(
    state: &AppState,
    http_base_url: &str,
    local: &str,
) -> Result<Nip05Lookup, String> {
    let url = format!(
        "{}/.well-known/nostr.json?name={}",
        http_base_url.trim_end_matches('/'),
        local
    );
    let response = state
        .relay_meta_client
        .get(&url)
        .header("Accept", "application/json")
        .timeout(NIP05_LOOKUP_TIMEOUT)
        .send()
        .await
        .map_err(|error| super::classify_request_error(&error))?;
    if response.status().is_redirection() {
        return Err(format!(
            "NIP-05 lookup for {local} was redirected off the relay ({}), so the relay did not answer it",
            response.status()
        ));
    }
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Nip05Lookup::Unsupported);
    }
    if !response.status().is_success() {
        return Err(format!(
            "NIP-05 lookup for {local} failed: {}",
            super::relay_error_message(response).await
        ));
    }
    let body: serde_json::Value = super::parse_json_response(response).await?;
    let owner = body
        .get("names")
        .and_then(|names| names.get(local))
        .and_then(|value| value.as_str())
        .map(|hex| hex.to_ascii_lowercase());
    Ok(match owner {
        Some(hex) if !hex.is_empty() => Nip05Lookup::OwnedBy(hex),
        _ => Nip05Lookup::Free,
    })
}

/// The handle to publish when the relay's well-known could not answer: the
/// one the agent already carries, and only when it is on this relay's own
/// domain.
///
/// Republishing the existing handle changes nothing about relay state, which
/// is exactly what an unanswerable confirmation justifies. The domain check
/// is what keeps it from being a change: a handle for another relay's domain
/// is not one this relay could ever attribute to the agent, so it is dropped
/// rather than published here.
fn unconfirmable_handle(existing_handle: Option<&str>, domain: &str) -> Option<String> {
    let (local, existing_domain) = split_handle(existing_handle?)?;
    (existing_domain == domain).then(|| format!("{local}@{domain}"))
}

/// Resolve the handle a managed agent should publish on `relay_url`.
///
/// Every candidate, including one the agent's kind:0 already carries, is
/// confirmed against the relay's public well-known endpoint before it is
/// returned. That confirmation is the whole point: the relay accepts a kind:0
/// whose handle collides and simply syncs the profile without the handle
/// (`handlers/side_effects.rs`, the `23505` retry), so the loser of a slug
/// race keeps a handle in its own kind:0 that resolves to nobody. Trusting
/// that copy would leave it stranded until someone renamed the agent.
///
/// Returns `Ok(None)` when every candidate is taken by someone else.
///
/// ADVISORY, never a gate. When the well-known cannot answer at all (a 502
/// or a 404 from an ingress in front of the relay, a rewrite, a same-origin
/// 3xx, a timeout), this keeps whatever handle the agent already carries on
/// this domain and publishes it unchanged, and publishes no handle when there
/// is none. Both halves matter and they used to disagree: an error propagated
/// out of `sync_managed_agent_profile` and abandoned the whole kind:0, so an
/// ordinary proxy hiccup cost the agent its name, avatar and about to protect
/// a handle that, on a fresh agent, did not exist; while a 404 went the other
/// way and published a kind:0 with no handle at all, stripping the one the
/// relay holds (kind:0 is absolute state there). Buzz's own relay always
/// answers 200 with a names map (`crates/buzz-relay/src/api/nip05.rs`), so
/// neither shape is the relay saying anything about handles: it is the relay
/// not answering, and the right response to that is to change nothing.
///
/// Keeping the existing handle is safe: it is the handle the relay itself
/// last attributed to this agent, republished verbatim, and the relay drops a
/// contested one silently on its UNIQUE index. The next reconcile whose
/// lookup succeeds re-confirms it.
pub async fn resolve_managed_agent_nip05(
    state: &AppState,
    relay_url: &str,
    agent_pubkey_hex: &str,
    display_name: &str,
    existing_handle: Option<&str>,
) -> Result<Option<String>, String> {
    // Egress boundary 9: the slug of the display name leaves the device as a
    // query parameter below, and slugification keeps the first bytes of a
    // pasted key backup intact. Fail closed before any lookup.
    crate::egress_guard::assert_no_key_backup_bytes(
        display_name.as_bytes(),
        "agent NIP-05 lookup",
    )?;
    let domain = nip05_domain(relay_url);
    if domain.is_empty() {
        return Ok(None);
    }
    let candidates = nip05_candidates(display_name, agent_pubkey_hex);
    let http_base = super::relay_http_base_url(relay_url);
    let agent_pubkey = agent_pubkey_hex.to_ascii_lowercase();
    for local in nip05_probe_order(existing_handle, &candidates, &domain) {
        let lookup = match lookup_nip05_owner(state, &http_base, &local).await {
            Ok(lookup) => lookup,
            // The endpoint could not answer. Every remaining candidate would
            // ask the same endpoint, so stop probing and change nothing.
            Err(error) => {
                eprintln!(
                    "buzz-desktop: NIP-05 confirmation for {agent_pubkey} on {domain} could not \
                     be completed, keeping the handle it already carries: {error}"
                );
                return Ok(unconfirmable_handle(existing_handle, &domain));
            }
        };
        match lookup {
            // A 404 is the same class: the relay serves this route
            // unconditionally, so a 404 is an ingress or a rewrite answering
            // in its place, not the relay saying handles are meaningless.
            Nip05Lookup::Unsupported => {
                return Ok(unconfirmable_handle(existing_handle, &domain));
            }
            // Free: nobody holds it, including a handle the relay dropped on
            // a collision that has since been released. Republishing reclaims
            // it.
            Nip05Lookup::Free => return Ok(Some(format!("{local}@{domain}"))),
            Nip05Lookup::OwnedBy(owner) if owner == agent_pubkey => {
                return Ok(Some(format!("{local}@{domain}")));
            }
            // Held by somebody else. Even when this is the handle the agent's
            // own kind:0 claims, move on: the relay is the authority.
            Nip05Lookup::OwnedBy(_) => continue,
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT: &str = "a1f3c0ffee00000000000000000000000000000000000000000000000000beef";

    #[test]
    fn domain_is_host_without_scheme_port_or_path() {
        assert_eq!(nip05_domain("ws://localhost:3000"), "localhost");
        assert_eq!(
            nip05_domain("wss://Relay.Example.com/"),
            "relay.example.com"
        );
        assert_eq!(
            nip05_domain("wss://chiefs-mac-studio.tailfa450b.ts.net"),
            "chiefs-mac-studio.tailfa450b.ts.net"
        );
        assert_eq!(nip05_domain("http://127.0.0.1:8080/path"), "127.0.0.1");
        assert_eq!(nip05_domain(""), "");
    }

    /// A bracketed IPv6 literal yields no domain at all, so no handle is
    /// published. The relay's `extract_domain` splits the authority on `:`
    /// and derives `[` from `[::1]:3000`, so any handle the desktop wrote
    /// there would be dropped without a word.
    #[test]
    fn ipv6_literal_relays_get_no_handle() {
        assert_eq!(nip05_domain("ws://[::1]:3000"), "");
        assert_eq!(nip05_domain("wss://[2001:db8::1]"), "");
    }

    #[test]
    fn local_part_is_a_bounded_lowercase_slug() {
        assert_eq!(nip05_local_part("Bob"), "bob");
        assert_eq!(nip05_local_part("  Sherlock Holmes! "), "sherlock-holmes");
        assert_eq!(nip05_local_part("!!!"), "agent");
        assert_eq!(nip05_local_part(""), "agent");
        let long = "a".repeat(80);
        assert_eq!(nip05_local_part(&long).len(), NIP05_LOCAL_MAX_LEN);
    }

    /// Accented Latin names keep their letters. Scripts with no ASCII
    /// decomposition still fall back, which the pubkey suffix disambiguates.
    #[test]
    fn unicode_names_fold_to_letters_not_separators() {
        let cases = [
            ("Zoë", "zoe"),
            ("Ünïcödé", "unicode"),
            ("José García", "jose-garcia"),
            ("Renée-Ann", "renee-ann"),
            ("Björn", "bjorn"),
            ("東京", "agent"),
        ];
        for (name, expected) in cases {
            assert_eq!(nip05_local_part(name), expected, "name={name}");
        }
    }

    /// The letters NFKD will not decompose. Without the transliteration map
    /// these lose their first letter or split a name in half, which is worse
    /// than an accent dropped: `Ægir` became `gir`, `Łukasz` became `ukasz`,
    /// `Søren` became `s-ren` and `Straße` became `stra-e`.
    #[test]
    fn undecomposable_latin_letters_are_transliterated_not_dropped() {
        let cases = [
            ("Ægir", "aegir"),
            ("Łukasz", "lukasz"),
            ("Søren", "soren"),
            ("Đoković", "dokovic"),
            ("Straße", "strasse"),
            ("Þor", "thor"),
            ("Œuvre", "oeuvre"),
        ];
        for (name, expected) in cases {
            assert_eq!(nip05_local_part(name), expected, "name={name}");
        }
    }

    /// The cap is on the SLUG, not the candidate: the pubkey suffix is
    /// appended after it and must survive, or the suffixed candidates stop
    /// being unique and the collision fallback stops working.
    #[test]
    fn the_length_cap_applies_to_the_slug_and_never_truncates_the_suffix() {
        let long = "a".repeat(80);
        let candidates = nip05_candidates(&long, AGENT);
        assert_eq!(candidates[0].len(), NIP05_LOCAL_MAX_LEN);
        assert_eq!(
            candidates[1],
            format!("{}-a1f3", "a".repeat(NIP05_LOCAL_MAX_LEN))
        );
        assert_eq!(
            candidates[2],
            format!("{}-a1f3c0ff", "a".repeat(NIP05_LOCAL_MAX_LEN))
        );
    }

    #[test]
    fn candidates_are_slug_then_pubkey_suffixed() {
        assert_eq!(
            nip05_candidates("Bob", AGENT),
            vec!["bob", "bob-a1f3", "bob-a1f3c0ff"]
        );
    }

    /// Table over the stability rule: an existing handle is preferred only
    /// when it is a candidate of the current name on the same domain.
    #[test]
    fn existing_handle_is_preferred_only_when_still_derived_from_the_name() {
        let candidates = nip05_candidates("Bob", AGENT);
        let cases: [(Option<&str>, Option<&str>); 7] = [
            (None, None),
            (Some("bob@relay.example"), Some("bob")),
            (Some("Bob-A1F3@Relay.Example"), Some("bob-a1f3")),
            (Some("bob-a1f3c0ff@relay.example"), Some("bob-a1f3c0ff")),
            (Some("bobby@relay.example"), None),
            (Some("bob@other.example"), None),
            (Some("not-a-handle"), None),
        ];
        for (existing, expected) in cases {
            assert_eq!(
                stable_existing_local(existing, &candidates, "relay.example").as_deref(),
                expected,
                "existing={existing:?}"
            );
        }
    }

    /// The probe order is a permutation of the candidates with the kept
    /// handle first: stability decides which is asked about first, never
    /// which is published without asking.
    #[test]
    fn probe_order_puts_the_existing_handle_first_and_repeats_nothing() {
        let candidates = nip05_candidates("Bob", AGENT);
        assert_eq!(
            nip05_probe_order(None, &candidates, "relay.example"),
            candidates
        );
        assert_eq!(
            nip05_probe_order(Some("bob-a1f3@relay.example"), &candidates, "relay.example"),
            vec!["bob-a1f3", "bob", "bob-a1f3c0ff"]
        );
        assert_eq!(
            nip05_probe_order(Some("nope@relay.example"), &candidates, "relay.example"),
            candidates
        );
    }

    // Gated off Windows like the other stub-relay tests: `build_app_state()`
    // pulls native DLLs unavailable in the Windows CI runner.
    #[cfg(not(target_os = "windows"))]
    mod stub_relay {
        use super::*;
        use crate::app_state::build_app_state;
        use std::sync::{Arc, Mutex};

        /// Stub `/.well-known/nostr.json` whose `names` map is the given
        /// table. Records every looked-up name. `None` table means 404.
        async fn spawn_well_known(
            names: Option<Vec<(&'static str, &'static str)>>,
        ) -> (String, Arc<Mutex<Vec<String>>>) {
            use axum::{extract::Query, http::StatusCode, routing::get, Router};

            let looked_up = Arc::new(Mutex::new(Vec::new()));
            let seen = looked_up.clone();
            let app = Router::new().route(
                "/.well-known/nostr.json",
                get(
                    move |Query(query): Query<std::collections::HashMap<String, String>>| {
                        let names = names.clone();
                        let seen = seen.clone();
                        async move {
                            let name = query.get("name").cloned().unwrap_or_default();
                            seen.lock().unwrap().push(name.clone());
                            let Some(names) = names else {
                                return (StatusCode::NOT_FOUND, String::new());
                            };
                            let mut map = serde_json::Map::new();
                            if let Some((_, hex)) = names.iter().find(|(n, _)| *n == name) {
                                map.insert(name, serde_json::Value::String((*hex).to_string()));
                            }
                            (
                                StatusCode::OK,
                                serde_json::json!({ "names": map, "relays": {} }).to_string(),
                            )
                        }
                    },
                ),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind stub relay");
            let addr = listener.local_addr().expect("stub relay addr");
            tokio::spawn(async move {
                axum::serve(listener, app).await.ok();
            });
            (format!("ws://{addr}"), looked_up)
        }

        const OTHER: &str = "0000000000000000000000000000000000000000000000000000000000000001";

        #[tokio::test]
        async fn free_slug_is_taken_as_is() {
            let (relay, looked_up) = spawn_well_known(Some(vec![])).await;
            let state = build_app_state();
            let handle = resolve_managed_agent_nip05(&state, &relay, AGENT, "Bob", None)
                .await
                .unwrap();
            assert_eq!(handle.as_deref(), Some("bob@127.0.0.1"));
            assert_eq!(*looked_up.lock().unwrap(), vec!["bob"]);
        }

        #[tokio::test]
        async fn collision_appends_pubkey_suffix() {
            let (relay, looked_up) = spawn_well_known(Some(vec![("bob", OTHER)])).await;
            let state = build_app_state();
            let handle = resolve_managed_agent_nip05(&state, &relay, AGENT, "Bob", None)
                .await
                .unwrap();
            assert_eq!(handle.as_deref(), Some("bob-a1f3@127.0.0.1"));
            assert_eq!(*looked_up.lock().unwrap(), vec!["bob", "bob-a1f3"]);
        }

        #[tokio::test]
        async fn handle_already_owned_by_this_agent_is_reused() {
            let (relay, _) = spawn_well_known(Some(vec![("bob", AGENT)])).await;
            let state = build_app_state();
            let handle = resolve_managed_agent_nip05(&state, &relay, AGENT, "Bob", None)
                .await
                .unwrap();
            assert_eq!(handle.as_deref(), Some("bob@127.0.0.1"));
        }

        #[tokio::test]
        async fn existing_suffixed_handle_is_confirmed_then_kept() {
            let (relay, looked_up) = spawn_well_known(Some(vec![("bob-a1f3", AGENT)])).await;
            let state = build_app_state();
            let handle = resolve_managed_agent_nip05(
                &state,
                &relay,
                AGENT,
                "Bob",
                Some("bob-a1f3@127.0.0.1"),
            )
            .await
            .unwrap();
            // Kept even though the bare slug is free now: stability wins,
            // but only after the relay confirmed the agent still holds it.
            assert_eq!(handle.as_deref(), Some("bob-a1f3@127.0.0.1"));
            assert_eq!(*looked_up.lock().unwrap(), vec!["bob-a1f3"]);
        }

        /// Regression for the slug race: two desktops publish `bob`, the
        /// relay's UNIQUE index gives it to one of them and syncs the loser's
        /// profile without the handle, but the loser's own kind:0 still says
        /// `bob@host`. Trusting that copy left the agent with a handle nobody
        /// could resolve, forever, because the reconcile short-circuited
        /// before any lookup. The relay's attribution decides.
        #[tokio::test]
        async fn contested_existing_handle_moves_to_the_suffixed_candidate() {
            let (relay, looked_up) = spawn_well_known(Some(vec![("bob", OTHER)])).await;
            let state = build_app_state();
            let handle =
                resolve_managed_agent_nip05(&state, &relay, AGENT, "Bob", Some("bob@127.0.0.1"))
                    .await
                    .unwrap();
            assert_eq!(handle.as_deref(), Some("bob-a1f3@127.0.0.1"));
            assert_eq!(*looked_up.lock().unwrap(), vec!["bob", "bob-a1f3"]);
        }

        /// The relay dropped the handle and nobody else took it: republishing
        /// the same kind:0 reclaims it, so the agent keeps the handle it has
        /// rather than churning onto a suffixed one.
        #[tokio::test]
        async fn existing_handle_the_relay_no_longer_holds_is_reclaimed() {
            let (relay, looked_up) = spawn_well_known(Some(vec![])).await;
            let state = build_app_state();
            let handle =
                resolve_managed_agent_nip05(&state, &relay, AGENT, "Bob", Some("bob@127.0.0.1"))
                    .await
                    .unwrap();
            assert_eq!(handle.as_deref(), Some("bob@127.0.0.1"));
            assert_eq!(*looked_up.lock().unwrap(), vec!["bob"]);
        }

        /// A lookup outage while confirming an existing handle keeps that
        /// handle. It must never fall through to "publish the kind:0 without
        /// a handle", which would strip the one the relay holds, and it must
        /// not become an error either, which would abandon the whole profile
        /// over a question the profile does not depend on.
        #[tokio::test]
        async fn outage_while_confirming_an_existing_handle_keeps_it() {
            let state = build_app_state();
            let handle = resolve_managed_agent_nip05(
                &state,
                "ws://127.0.0.1:9",
                AGENT,
                "Bob",
                Some("bob@127.0.0.1"),
            )
            .await
            .expect("an unanswerable lookup is not a gate");
            assert_eq!(
                handle.as_deref(),
                Some("bob@127.0.0.1"),
                "an outage must republish the handle unchanged, never strip it"
            );
        }

        #[tokio::test]
        async fn every_candidate_taken_publishes_no_handle() {
            let (relay, _) = spawn_well_known(Some(vec![
                ("bob", OTHER),
                ("bob-a1f3", OTHER),
                ("bob-a1f3c0ff", OTHER),
            ]))
            .await;
            let state = build_app_state();
            let handle = resolve_managed_agent_nip05(&state, &relay, AGENT, "Bob", None)
                .await
                .unwrap();
            assert_eq!(handle, None);
        }

        /// A 404 answers nothing (Buzz's relay always answers 200 there), so
        /// with no handle to keep, none is published, and none is invented.
        #[tokio::test]
        async fn a_404_with_nothing_to_keep_publishes_no_handle() {
            let (relay, _) = spawn_well_known(None).await;
            let state = build_app_state();
            let handle = resolve_managed_agent_nip05(&state, &relay, AGENT, "Bob", None)
                .await
                .unwrap();
            assert_eq!(handle, None);
        }

        /// ...and with a handle to keep, the same 404 keeps it. This is the
        /// arm that used to differ: `Unsupported` was mapped to `Ok(None)`,
        /// so an ingress 404 in front of the relay published a kind:0 with no
        /// `nip05` and deleted a handle the relay was still holding.
        #[tokio::test]
        async fn a_404_never_strips_the_handle_the_agent_carries() {
            let (relay, _) = spawn_well_known(None).await;
            let state = build_app_state();
            let handle =
                resolve_managed_agent_nip05(&state, &relay, AGENT, "Bob", Some("bob@127.0.0.1"))
                    .await
                    .unwrap();
            assert_eq!(handle.as_deref(), Some("bob@127.0.0.1"));
        }

        /// A cross-origin redirect must not be able to answer the handle
        /// question. The lookup is the AUTHORITY on who holds a local part,
        /// so an answer from a third origin is not an answer at all: one
        /// claiming the name pushes the agent off a handle it owns, and one
        /// answering an empty `names` map makes the desktop publish a handle
        /// the relay attributes to somebody else, which the relay drops on
        /// its UNIQUE index and which strands the agent exactly as the
        /// contested-handle bug did.
        #[tokio::test]
        async fn a_redirected_well_known_is_an_error_not_an_answer() {
            use axum::{http::StatusCode, response::IntoResponse, routing::get, Router};

            // Origin B would happily answer, and its answer must never count.
            let (elsewhere, _) = spawn_well_known(Some(vec![("bob", OTHER)])).await;
            let target = format!(
                "{}/.well-known/nostr.json",
                crate::relay::relay_http_base_url(&elsewhere)
            );

            let app = Router::new().route(
                "/.well-known/nostr.json",
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

            let state = build_app_state();
            let relay = format!("ws://{addr}");

            // The lookup itself refuses the redirect rather than reading the
            // other origin's answer.
            let error =
                lookup_nip05_owner(&state, &crate::relay::relay_http_base_url(&relay), "bob")
                    .await
                    .expect_err("a 3xx is not the relay's answer");
            assert!(
                error.contains("redirected"),
                "the error must name the redirect: {error}"
            );

            // And the resolver degrades rather than adopting it: origin B
            // claims `bob` for OTHER and would leave every other candidate
            // free, so a client that followed the redirect would come back
            // with `bob-a1f3`. Nothing was confirmed, so nothing is
            // published.
            let handle = resolve_managed_agent_nip05(&state, &relay, AGENT, "Bob", None)
                .await
                .expect("an unanswerable lookup is not a gate");
            assert_eq!(
                handle, None,
                "a third origin's answer must never become the agent's handle"
            );
        }

        /// An unreachable relay publishes no handle when there is none to
        /// keep. It must not invent one, and it must not fail the publish.
        #[tokio::test]
        async fn unreachable_relay_publishes_no_handle_and_invents_none() {
            let state = build_app_state();
            let handle =
                resolve_managed_agent_nip05(&state, "ws://127.0.0.1:9", AGENT, "Bob", None)
                    .await
                    .expect("an unreachable lookup is not a gate");
            assert_eq!(handle, None);
        }
    }
}
