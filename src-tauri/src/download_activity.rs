//! Downloads in progress, for the UI: which devices are sending one, and how far along.
//!
//! `socket_server` reports each connection's download as it starts, grows and ends. The
//! frontend gets the whole list as the `downloads-active` event whenever it changes
//! (progress at most once a second per download) and can ask for it with
//! `get_active_downloads` when a page loads.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use serde::Serialize;

use crate::global_app_handle::emit;

const EVENT: &str = "downloads-active";
/// Progress updates for one download are sent at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Serialize, Clone, Debug)]
pub struct ActiveDownload {
    pub imei: String,
    pub company: String,
    /// Unix seconds (UTC).
    pub started_at: i64,
    /// Bytes received so far. The total isn't known until it ends.
    pub bytes: usize,
    pub generation: u8,
}

struct Entry {
    download: ActiveDownload,
    last_emitted: Instant,
}

/// By connection (peer address): one download at a time per connection.
static ACTIVE: Lazy<Mutex<HashMap<String, Entry>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn active() -> std::sync::MutexGuard<'static, HashMap<String, Entry>> {
    ACTIVE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn list(active: &HashMap<String, Entry>) -> Vec<ActiveDownload> {
    let mut downloads: Vec<ActiveDownload> = active.values().map(|e| e.download.clone()).collect();
    downloads.sort_by_key(|d| d.started_at);
    downloads
}

/// A connection has started a download (`DownloadStart`), or resumed one with `bytes`
/// already received (a resumed one, see `partial_downloads`). Replaces any it had.
pub fn started(connection: &str, imei: &str, company: &str, generation: u8, bytes: usize) {
    let downloads = {
        let mut active = active();
        active.insert(
            connection.to_string(),
            Entry {
                download: ActiveDownload {
                    imei: imei.to_string(),
                    company: company.to_string(),
                    started_at: chrono::Utc::now().timestamp(),
                    bytes,
                    generation,
                },
                last_emitted: Instant::now(),
            },
        );
        list(&active)
    };
    emit(EVENT, downloads);
}

/// More of a connection's download has arrived; `bytes` is the total so far.
pub fn progress(connection: &str, bytes: usize) {
    let downloads = {
        let mut active = active();
        let Some(entry) = active.get_mut(connection) else { return };
        entry.download.bytes = bytes;
        if entry.last_emitted.elapsed() < PROGRESS_INTERVAL {
            return;
        }
        entry.last_emitted = Instant::now();
        list(&active)
    };
    emit(EVENT, downloads);
}

/// A connection's download has ended: completed, failed, or the connection closed.
/// Does nothing if it had none.
pub fn finished(connection: &str) {
    let downloads = {
        let mut active = active();
        if active.remove(connection).is_none() {
            return;
        }
        list(&active)
    };
    emit(EVENT, downloads);
}

/// Downloads in progress, oldest first.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_active_downloads() -> Vec<ActiveDownload> {
    list(&active())
}

/// Re-sends the list, for a page that has just (re)loaded.
pub fn emit_active_downloads() {
    emit(EVENT, get_active_downloads());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn for_connection(connection: &str) -> Option<ActiveDownload> {
        active().get(connection).map(|e| e.download.clone())
    }

    #[test]
    fn a_download_is_listed_from_start_to_finish() {
        let connection = "192.0.2.10:5000";
        started(connection, "356938035643809", "acme", 2, 0);
        assert_eq!(for_connection(connection).map(|d| (d.bytes, d.company)), Some((0, "acme".to_string())));
        assert!(get_active_downloads().iter().any(|d| d.imei == "356938035643809"));

        progress(connection, 4096);
        progress(connection, 8192); // within the throttle interval: stored, not emitted
        assert_eq!(for_connection(connection).map(|d| d.bytes), Some(8192));

        finished(connection);
        assert!(for_connection(connection).is_none());
        finished(connection); // again (connection end after DownloadComplete): no-op
    }

    #[test]
    fn progress_for_an_unknown_connection_is_ignored() {
        progress("192.0.2.11:5000", 100);
        assert!(for_connection("192.0.2.11:5000").is_none());
    }
}
