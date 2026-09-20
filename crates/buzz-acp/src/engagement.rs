//! Thread engagement that survives a restart.
//!
//! The main loop's engaged-thread set is in memory: a thread the agent was
//! answering before a restart is forgotten, and the next reply in it — which
//! carries no fresh `p` mention because the human is mid-conversation — never
//! reaches the agent. On startup the harness asks the relay for its own recent
//! stream messages and re-engages the threads they belong to, per subscribed
//! channel, newest first, bounded exactly like the live set.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use buzz_core::kind::{KIND_STREAM_MESSAGE, KIND_STREAM_MESSAGE_V2};
use serde_json::Value;
use uuid::Uuid;

use crate::relay::RestClient;

/// How far back the agent's own posts are read on startup. Threads older than
/// this are not re-engaged; a fresh mention engages them again as usual.
pub const LOOKBACK: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// Derive the per-channel thread roots the agent is engaged in from its own
/// posts, newest first, at most `max_per_channel` per channel and only for
/// channels in `subscribed`. Each channel's deque is ordered oldest-first, the
/// same shape the live set keeps (most recent last).
///
/// The root is resolved the way admission resolves it: NIP-10 markers via
/// `buzz_core::nip10`, falling back to the post's own id for a top-level post.
pub fn engaged_roots_from_history(
    events: &[Value],
    subscribed: &HashSet<Uuid>,
    max_per_channel: usize,
) -> HashMap<Uuid, VecDeque<String>> {
    let mut posts: Vec<(u64, Uuid, String)> = events
        .iter()
        .filter_map(thread_of)
        .filter(|(_, channel, _)| subscribed.contains(channel))
        .collect();
    // Newest first, so the per-channel cap keeps the threads that matter.
    posts.sort_by_key(|post| std::cmp::Reverse(post.0));
    let mut out: HashMap<Uuid, VecDeque<String>> = HashMap::new();
    for (_, channel, root) in posts {
        let set = out.entry(channel).or_default();
        if set.len() >= max_per_channel || set.iter().any(|r| *r == root) {
            continue;
        }
        set.push_front(root);
    }
    out
}

/// `(created_at, channel, thread root)` of one raw stream-message event, or
/// `None` when the event is not a well-formed channel post.
fn thread_of(event: &Value) -> Option<(u64, Uuid, String)> {
    let id = event.get("id")?.as_str()?;
    if id.len() != 64 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let created_at = event.get("created_at")?.as_u64()?;
    let tags: Vec<Vec<String>> = serde_json::from_value(event.get("tags")?.clone()).ok()?;
    let channel = tags
        .iter()
        .find(|tag| tag.len() >= 2 && tag[0] == "h")
        .and_then(|tag| Uuid::parse_str(&tag[1]).ok())?;
    let markers = buzz_core::nip10::parse_thread_markers_from_parts(tags.iter().map(Vec::as_slice));
    let root = markers
        .resolve()
        .map(|(root, _parent)| root)
        .unwrap_or_else(|| id.to_string())
        .to_ascii_lowercase();
    Some((created_at, channel, root))
}

/// Read the agent's own recent stream messages from the relay and derive the
/// engaged-thread set. A relay error is returned, not swallowed, so the caller
/// can say the seed was skipped; the agent then behaves as before the seed
/// existed.
pub async fn seed_engaged_threads(
    rest: &RestClient,
    agent_pubkey_hex: &str,
    subscribed: &HashSet<Uuid>,
    max_per_channel: usize,
) -> Result<HashMap<Uuid, VecDeque<String>>, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let since = now.saturating_sub(LOOKBACK.as_secs());
    let filter = serde_json::json!({
        "kinds": [KIND_STREAM_MESSAGE, KIND_STREAM_MESSAGE_V2],
        "authors": [agent_pubkey_hex],
        "since": since,
    });
    let events = rest
        .query_raw_all(filter)
        .await
        .map_err(|e| e.to_string())?;
    Ok(engaged_roots_from_history(
        &events,
        subscribed,
        max_per_channel,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "075640027bff555cd91768830f1d0eb4def3593685d248060a6c9fac308b3113";
    const OTHER: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    fn post(id_byte: u8, created_at: u64, channel: &str, e_tags: &[(&str, &str)]) -> Value {
        let id = format!("{:02x}", id_byte).repeat(32);
        let mut tags = vec![serde_json::json!(["h", channel])];
        for (target, marker) in e_tags {
            tags.push(serde_json::json!(["e", target, "", marker]));
        }
        serde_json::json!({"id": id, "created_at": created_at, "kind": 9, "tags": tags, "content": "x"})
    }

    #[test]
    fn replies_engage_their_root_and_top_level_posts_engage_themselves() {
        let channel = Uuid::new_v4();
        let events = vec![
            post(0xaa, 100, &channel.to_string(), &[(ROOT, "reply")]),
            post(0xbb, 200, &channel.to_string(), &[]),
            post(
                0xcc,
                300,
                &channel.to_string(),
                &[(OTHER, "root"), (ROOT, "reply")],
            ),
        ];
        let roots = engaged_roots_from_history(&events, &HashSet::from([channel]), 16);
        let set: Vec<String> = roots[&channel].iter().cloned().collect();
        // Oldest first, most recent last, like the live set; the lone `reply`
        // marker resolves to that id, and the root marker wins when present.
        assert_eq!(
            set,
            vec![ROOT.to_string(), "bb".repeat(32), OTHER.to_string()]
        );
    }

    #[test]
    fn newest_threads_win_the_per_channel_cap_and_unsubscribed_channels_are_dropped() {
        let channel = Uuid::new_v4();
        let elsewhere = Uuid::new_v4();
        let mut events = Vec::new();
        for i in 0..5u8 {
            events.push(post(i + 1, 100 + u64::from(i), &channel.to_string(), &[]));
        }
        events.push(post(0xee, 999, &elsewhere.to_string(), &[]));
        let roots = engaged_roots_from_history(&events, &HashSet::from([channel]), 2);
        assert_eq!(roots.len(), 1);
        let set: Vec<String> = roots[&channel].iter().cloned().collect();
        assert_eq!(set, vec!["04".repeat(32), "05".repeat(32)]);
    }

    #[test]
    fn malformed_events_are_skipped() {
        let channel = Uuid::new_v4();
        let events = vec![
            serde_json::json!({"id": "nope", "created_at": 1, "tags": [["h", channel.to_string()]]}),
            serde_json::json!({"id": "aa".repeat(32), "created_at": 1, "tags": [["h", "not-a-uuid"]]}),
            serde_json::json!({"id": "aa".repeat(32), "tags": [["h", channel.to_string()]]}),
        ];
        assert!(engaged_roots_from_history(&events, &HashSet::from([channel]), 16).is_empty());
    }
}
