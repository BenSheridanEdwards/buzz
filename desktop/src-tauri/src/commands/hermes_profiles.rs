use crate::managed_agents::{default_hermes_profiles_dir, scan_hermes_profiles, HermesProfile};

/// List the Hermes Agent profiles installed under `~/.hermes/profiles`.
///
/// Feeds the "Hermes profile" picker on the agent forms. A missing profiles
/// directory is not an error: the picker simply has no entries to offer.
#[tauri::command]
pub async fn list_hermes_profiles() -> Result<Vec<HermesProfile>, String> {
    tokio::task::spawn_blocking(|| {
        default_hermes_profiles_dir()
            .map(|root| scan_hermes_profiles(&root))
            .unwrap_or_default()
    })
    .await
    .map_err(|error| format!("spawn_blocking failed: {error}"))
}
