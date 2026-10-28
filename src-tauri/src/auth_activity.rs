//! Devices authenticating with a company card, for the UI: from the server sending
//! `Ready` until the device reports `AuthOutcome`, loses the card or disconnects.
//!
//! `socket_server` reports each connection's authentication as it starts and ends. The
//! frontend gets the whole list as the `authenticating-devices` event whenever it
//! changes and can ask for it with `get_authenticating_devices` when a page loads.

use std::collections::HashMap;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use serde::Serialize;

use crate::global_app_handle::emit;

const EVENT: &str = "authenticating-devices";

#[derive(Serialize, Clone, Debug)]
pub struct AuthenticatingDevice {
    pub imei: String,
    pub company: String,
    /// Unix seconds (UTC).
    pub started_at: i64,
}

/// By connection (peer address).
static ACTIVE: Lazy<Mutex<HashMap<String, AuthenticatingDevice>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn active() -> std::sync::MutexGuard<'static, HashMap<String, AuthenticatingDevice>> {
    ACTIVE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn list(active: &HashMap<String, AuthenticatingDevice>) -> Vec<AuthenticatingDevice> {
    let mut devices: Vec<AuthenticatingDevice> = active.values().cloned().collect();
    devices.sort_by_key(|d| d.started_at);
    devices
}

/// A connection's device has been sent `Ready` with a card. Restarts it if it already
/// was (a replacement card).
pub fn started(connection: &str, imei: &str, company: &str) {
    let devices = {
        let mut active = active();
        active.insert(
            connection.to_string(),
            AuthenticatingDevice {
                imei: imei.to_string(),
                company: company.to_string(),
                started_at: chrono::Utc::now().timestamp(),
            },
        );
        list(&active)
    };
    emit(EVENT, devices);
}

/// A connection's authentication has ended, however it did. Does nothing if it wasn't
/// authenticating.
pub fn finished(connection: &str) {
    let devices = {
        let mut active = active();
        if active.remove(connection).is_none() {
            return;
        }
        list(&active)
    };
    emit(EVENT, devices);
}

/// Devices authenticating, longest first.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_authenticating_devices() -> Vec<AuthenticatingDevice> {
    list(&active())
}

/// Re-sends the list, for a page that has just (re)loaded.
pub fn emit_authenticating_devices() {
    emit(EVENT, get_authenticating_devices());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_is_listed_until_its_authentication_ends() {
        let connection = "192.0.2.20:5000";
        started(connection, "356938035643841", "acme");
        assert!(get_authenticating_devices().iter().any(|d| d.imei == "356938035643841"));

        finished(connection);
        assert!(!get_authenticating_devices().iter().any(|d| d.imei == "356938035643841"));
        finished(connection); // again (connection end after AuthOutcome): no-op
    }
}
