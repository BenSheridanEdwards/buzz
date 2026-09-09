use crate::managed_agents::{
    default_hermes_home_dir, list_hermes_profiles_for_home, HermesProfile,
};

/// List the Hermes Agent profiles installed under `~/.hermes`.
///
/// Feeds the "Hermes profile" picker on the agent forms. A missing profiles
/// directory is not an error: the picker simply has no entries to offer. A
/// profiles root that exists but cannot be read *is* an error, so the picker
/// can say the scan failed rather than claim the machine has no profiles.
#[tauri::command]
pub async fn list_hermes_profiles() -> Result<Vec<HermesProfile>, String> {
    tokio::task::spawn_blocking(|| {
        let Some(home) = default_hermes_home_dir() else {
            return Ok(Vec::new());
        };
        list_hermes_profiles_for_home(&home)
            .map_err(|error| format!("could not read {}: {error}", home.display()))
    })
    .await
    .map_err(|error| format!("spawn_blocking failed: {error}"))?
}
