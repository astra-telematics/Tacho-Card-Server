//! Updating the server build's container from the web UI.
//!
//! A container can't replace itself, so docker-compose.yml runs Watchtower beside it,
//! which only updates when asked through its HTTP API. Updates are only offered in the
//! Docker image (`TCS_DOCKER`, set by the Dockerfile), with `TCS_UPDATER_TOKEN` set
//! and Watchtower answering its health check, at `TCS_UPDATER_URL` or by default
//! `DEFAULT_UPDATER_URL`. Otherwise (the desktop app, a server run another way, or a
//! compose file without Watchtower) the UI only shows the version.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use serde::Serialize;

/// GitHub allows 60 unauthenticated requests an hour, so the latest release is
/// looked up at most this often.
const LATEST_RELEASE_CACHE: Duration = Duration::from_secs(10 * 60);

/// The updater service in docker-compose.yml.
const DEFAULT_UPDATER_URL: &str = "http://updater:8080";
/// How long the health check waits for Watchtower before taking it as not there.
const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Serialize, Clone, Debug)]
pub struct UpdateStatus {
    pub current_version: String,
    /// The latest release (`v1.2.3`), or `None` if GitHub couldn't be asked.
    pub latest_version: Option<String>,
    pub update_available: bool,
    /// Whether this server can be updated from the UI: it's in Docker and Watchtower
    /// is set up and connected to Docker.
    pub can_update: bool,
}

struct Updater {
    url: String,
    token: String,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// The updater's configuration, if this is the Docker image and it has a token.
fn updater() -> Option<Updater> {
    env("TCS_DOCKER")?;
    let token = env("TCS_UPDATER_TOKEN")?;
    let url = env("TCS_UPDATER_URL").unwrap_or_else(|| DEFAULT_UPDATER_URL.to_string());
    Some(Updater { url: url.trim_end_matches('/').to_string(), token })
}

/// Whether Watchtower is answering and connected to Docker (its `/readyz`, which needs
/// `health` in `WATCHTOWER_HTTP_API_ENDPOINTS`).
async fn updater_ready(updater: &Updater) -> bool {
    let result = reqwest::Client::new()
        .get(format!("{}/readyz", updater.url))
        .timeout(HEALTH_CHECK_TIMEOUT)
        .send()
        .await;
    match result {
        Ok(response) if response.status().is_success() => true,
        Ok(response) => {
            log::warn!("[Update] action=check_updater url={} status={}", updater.url, response.status());
            false
        }
        Err(e) => {
            log::warn!("[Update] action=check_updater url={} err={}", updater.url, e);
            false
        }
    }
}

static LATEST: Lazy<Mutex<Option<(Instant, String)>>> = Lazy::new(|| Mutex::new(None));

async fn latest_version() -> Option<String> {
    if let Some((fetched_at, version)) = LATEST.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        if fetched_at.elapsed() < LATEST_RELEASE_CACHE {
            return Some(version.clone());
        }
    }
    match crate::logger::latest_release_tag().await {
        Ok(version) => {
            *LATEST.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), version.clone()));
            Some(version)
        }
        Err(e) => {
            log::warn!("[Update] action=check_latest_version err={}", e);
            None
        }
    }
}

fn is_newer(latest: &str, current: &str) -> bool {
    crate::logger::version_to_number(latest) > crate::logger::version_to_number(current)
}

pub async fn get_update_status() -> UpdateStatus {
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let latest_version = latest_version().await;
    let update_available = latest_version.as_deref().is_some_and(|latest| is_newer(latest, &current_version));
    let can_update = match updater() {
        Some(updater) => updater_ready(&updater).await,
        None => false,
    };
    UpdateStatus { current_version, latest_version, update_available, can_update }
}

/// Asks Watchtower to pull the latest image and restart the server with it. Returns
/// once it has started; the server then stops, so connections and in-progress
/// downloads are cut off.
pub async fn start_update(username: &str) -> Result<(), String> {
    let Some(updater) = updater() else {
        return Err("This server isn't set up to be updated from here.".to_string());
    };
    let response = reqwest::Client::new()
        .post(format!("{}/v1/update?async=true", updater.url))
        .bearer_auth(&updater.token)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| {
            log::error!("[Update] action=start_update err={}", e);
            format!("Couldn't reach the updater: {}", e)
        })?;
    match response.status().as_u16() {
        200 | 202 => {
            log::info!("[Update] action=start_update user={} status=started", username);
            Ok(())
        }
        401 => Err("The updater refused the token: TCS_UPDATER_TOKEN must match WATCHTOWER_HTTP_API_TOKEN.".to_string()),
        429 => Err("An update is already in progress.".to_string()),
        status => {
            log::error!("[Update] action=start_update status={}", status);
            Err(format!("The updater answered {}.", status))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_later_release_is_an_update() {
        assert!(is_newer("v0.0.12", "0.0.11"));
        assert!(!is_newer("v0.0.12", "0.0.12"));
        assert!(!is_newer("v0.0.11", "0.0.12"));
    }
}
