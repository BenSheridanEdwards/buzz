//! The relay's own words about a refusal, bounded once at construction.
//!
//! This lives in its own module so the bound is structural rather than a
//! convention. [`RelayRefusal`]'s fields are private, and a private field is
//! visible only inside the module that declares it and that module's
//! descendants, so neither `relay.rs` (this module's parent) nor
//! `relay/submit.rs` (its sibling) can build one field-by-field. Every door
//! has to go through [`RelayRefusal::new`], which is where the cap is applied.
//!
//! There were two doors, and the bound was on one of them. `relay_error_details`
//! (HTTP 4xx with a JSON body) had it; `submit.rs`'s HTTP 200 with
//! `accepted: false` did not, and that is the door Buzz's own relay uses for
//! an ordinary refusal (`api/bridge.rs` answers `POST /events` 200 with
//! `{event_id, accepted, message}`). Over two million characters of relay text
//! reached `relay-membership.json` through it. A third door added later
//! inherits the bound by construction instead of by review.

/// Longest run of relay-authored text kept from a refusal, in characters.
///
/// The relay writes `error` and `message` and nothing upstream bounds the
/// body: `relay_error_details` reads it with `text()` and `parse_json_response`
/// deserializes it whole, and this type is the first thing to *persist* it.
/// `membership_record_for_outcome` writes the refusal into
/// `relay-membership.json` (one row per agent/relay pair, on a fleet that
/// multiplies) and the card renders it on every summary pass. A relay
/// answering megabytes would put megabytes there.
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

/// The relay's own reason for refusing a request, parsed from the JSON body
/// it answered with.
///
/// The relay writes two different fields: `error` is the machine-readable
/// reason a caller can branch on (`relay_membership_required` from the HTTP
/// bridge's membership gate, `invalid: actor not authorized: ...` from
/// `handlers/relay_admin.rs`), and `message` is the human sentence it
/// sometimes adds. Callers that must classify a refusal read
/// [`Self::code_is`] or [`Self::haystack`]; callers that show it to a user read
/// [`Self::message`]. Neither re-parses a rendered string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayRefusal {
    /// The body's `error` field, bounded.
    code: Option<String>,
    /// The body's `message` field, bounded.
    detail: Option<String>,
}

impl RelayRefusal {
    /// The only way to build one. Both halves are bounded here, so
    /// [`Self::message`], [`Self::haystack`], the rendered
    /// `relay returned <status>: <text>` string every caller composes from
    /// `message()`, and the `*_detail` copy the membership sidecar persists
    /// all inherit the cap without repeating it.
    pub fn new(code: Option<String>, detail: Option<String>) -> Self {
        Self {
            code: code.as_deref().map(bound_relay_text),
            detail: detail.as_deref().map(bound_relay_text),
        }
    }

    /// Did the relay say anything at all? A body with neither field is not
    /// the relay's answer about this request and must not become a refusal.
    pub fn is_silent(&self) -> bool {
        self.code.is_none() && self.detail.is_none()
    }

    /// Is the relay's machine code exactly `machine_code`?
    ///
    /// Exact, and only against `error`. A machine code is a token the relay
    /// emits in that field and nowhere else, so a proxy error page or a
    /// human sentence that merely quotes the token is not the relay
    /// answering with it. Trimmed and case-folded because that is free;
    /// nothing else is allowed to differ.
    pub fn code_is(&self, machine_code: &str) -> bool {
        self.code
            .as_deref()
            .is_some_and(|code| code.trim().eq_ignore_ascii_case(machine_code))
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The cap is on the constructor, so it applies to whichever field the
    /// door happened to fill. Both doors are exercised through the real code
    /// paths elsewhere; this pins the unit.
    #[test]
    fn both_halves_are_bounded_by_the_only_constructor() {
        let refusal = RelayRefusal::new(
            Some("e".repeat(MAX_RELAY_TEXT_CHARS * 8)),
            Some("m".repeat(MAX_RELAY_TEXT_CHARS * 8)),
        );
        for text in [refusal.message(), refusal.haystack()] {
            assert!(
                text.chars().count() <= MAX_RELAY_TEXT_CHARS * 2 + 32,
                "the relay's words must not survive unbounded: {} chars",
                text.chars().count()
            );
        }
        assert!(refusal.message().ends_with("... (truncated)"));
    }

    /// Exactly at the cap is not truncated; one over is.
    #[test]
    fn the_cap_edges_are_where_they_are_documented() {
        let at = RelayRefusal::new(None, Some("x".repeat(MAX_RELAY_TEXT_CHARS)));
        assert!(!at.message().contains("(truncated)"));
        let over = RelayRefusal::new(None, Some("x".repeat(MAX_RELAY_TEXT_CHARS + 1)));
        assert!(over.message().ends_with("... (truncated)"));
    }

    /// A multi-byte body must be cut on a char boundary, never mid code point.
    #[test]
    fn a_multi_byte_refusal_is_cut_on_a_char_boundary() {
        for wide in ['\u{65E5}', '\u{1F600}'] {
            let refusal = RelayRefusal::new(
                None,
                Some(std::iter::repeat_n(wide, MAX_RELAY_TEXT_CHARS + 100).collect::<String>()),
            );
            let message = refusal.message();
            assert_eq!(
                message.chars().filter(|c| *c == wide).count(),
                MAX_RELAY_TEXT_CHARS
            );
        }
    }

    /// A machine code is matched exactly, so a page that merely quotes it is
    /// not the relay answering with it.
    #[test]
    fn a_machine_code_is_matched_exactly_not_as_a_substring() {
        assert!(
            RelayRefusal::new(Some("relay_membership_required".into()), None)
                .code_is("relay_membership_required")
        );
        assert!(
            RelayRefusal::new(Some("  Relay_Membership_Required \n".into()), None)
                .code_is("relay_membership_required")
        );
        assert!(
            !RelayRefusal::new(
                Some("Bad Gateway: upstream said relay_membership_required".into()),
                None
            )
            .code_is("relay_membership_required"),
            "a proxy page that quotes the code is not the relay emitting it"
        );
        assert!(
            !RelayRefusal::new(None, Some("relay_membership_required".into()))
                .code_is("relay_membership_required"),
            "the human sentence field is not the machine-code field"
        );
    }
}
