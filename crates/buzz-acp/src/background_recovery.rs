//! Bounded attempts to restore a provider session for a pending worker result.
//!
//! Failure never acknowledges or deletes the durable result. A missing or
//! incompatible session is paused after three attempts in this harness run;
//! restarting the harness after repair permits another bounded set of attempts.
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time::Instant;

#[derive(Default)]
pub(crate) struct RecoveryRetries {
    failures: HashMap<String, Failure>,
}

struct Failure {
    attempts: u8,
    retry_at: Option<Instant>,
}

impl RecoveryRetries {
    pub(crate) fn ready(&self, origin: &str, now: &Instant) -> bool {
        self.failures
            .get(origin)
            .is_none_or(|failure| failure.retry_at.is_some_and(|retry_at| *now >= retry_at))
    }

    pub(crate) fn fail(&mut self, origin: &str, now: Instant) -> String {
        let failure = self.failures.entry(origin.to_owned()).or_insert(Failure {
            attempts: 0,
            retry_at: None,
        });
        failure.attempts = failure.attempts.saturating_add(1).min(3);
        let delay = match failure.attempts {
            1 => Some(60),
            2 => Some(120),
            _ => None,
        };
        failure.retry_at = delay.map(|seconds| now + Duration::from_secs(seconds));
        match delay {
            Some(seconds) => format!(
                "Background result recovery attempt {}/3 failed for session {origin}; retry in {seconds}s. The result remains pending.",
                failure.attempts
            ),
            None => format!(
                "Background result recovery paused after 3 failed attempts for session {origin}. The result remains pending; repair the session and restart the agent to retry."
            ),
        }
    }

    pub(crate) fn clear(&mut self, origin: &str) {
        self.failures.remove(origin);
    }

    /// Keep retry state only for the durable pending origins advertised now.
    pub(crate) fn retain_pending(&mut self, pending: &HashSet<String>) {
        self.failures.retain(|origin, _| pending.contains(origin));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_backs_off_then_pauses_without_reopening_at_later_time() {
        let mut retries = RecoveryRetries::default();
        let start = Instant::now();
        assert!(retries.ready("origin", &start));
        assert!(retries.fail("origin", start).contains("retry in 60s"));
        assert!(!retries.ready("origin", &(start + Duration::from_secs(59))));
        let second = start + Duration::from_secs(60);
        assert!(retries.ready("origin", &second));
        assert!(retries.fail("origin", second).contains("retry in 120s"));
        assert!(!retries.ready("origin", &(second + Duration::from_secs(119))));
        let third = second + Duration::from_secs(120);
        assert!(retries.ready("origin", &third));
        let diagnostic = retries.fail("origin", third);
        assert!(diagnostic.contains("paused after 3 failed attempts"));
        assert!(diagnostic.contains("result remains pending"));
        assert!(!retries.ready("origin", &(third + Duration::from_secs(86400))));
        assert!(retries.ready("unrelated", &third));
    }

    #[test]
    fn success_and_completed_origins_release_retry_state() {
        let mut retries = RecoveryRetries::default();
        let now = Instant::now();
        retries.fail("successful", now);
        retries.clear("successful");
        assert!(retries.ready("successful", &now));
        retries.fail("pending", now);
        retries.fail("completed", now);
        retries.retain_pending(&HashSet::from(["pending".into()]));
        assert_eq!(retries.failures.len(), 1);
        assert!(!retries.ready("pending", &now));
        assert!(retries.ready("completed", &now));
        assert!(RecoveryRetries::default().ready("pending", &now));
    }
}
