use super::{ack_publication, invalid, load, save};
use crate::acp::AcpError;
use std::path::Path;

pub(crate) struct Publication<'a> {
    pub dir: &'a Path,
    pub sid: &'a str,
    pub key: &'a str,
}

impl Publication<'_> {
    pub fn retain(&self, event: nostr::Event) -> Result<nostr::Event, AcpError> {
        let mut state = load(self.dir, self.sid)?;
        if let Some(existing) = state.publications.get(self.key) {
            return Ok(existing.clone());
        }
        state.publications.insert(self.key.into(), event.clone());
        save(self.dir, self.sid, &state)?;
        Ok(event)
    }
    pub async fn submit(
        &self,
        rest: &crate::relay::RestClient,
        event: &nostr::Event,
    ) -> Result<(), AcpError> {
        let response =
            tokio::time::timeout(std::time::Duration::from_secs(20), rest.submit_event(event))
                .await
                .map_err(|_| invalid("publication timed out; event retained"))?
                .map_err(|e| invalid(&format!("publication failed: {e}")))?;
        if response["accepted"] != true {
            return Err(invalid(
                "relay did not acknowledge acceptance; event retained",
            ));
        }
        if let Some(id) = response["event_id"].as_str() {
            if id != event.id.to_hex() {
                return Err(invalid("relay acknowledgement identity mismatch"));
            }
        }
        ack_publication(self.dir, self.sid, self.key)
    }
}
