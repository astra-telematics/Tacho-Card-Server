//! Downloads whose connection dropped part-way, kept so the device can resume them.
//!
//! The device streams a download from the VU as it reads it. When its connection drops
//! it keeps its VU session, reconnects and sends `ConnAnnounce` with its resume flag
//! set. Accepted, it skips `Ready` and authentication and carries on sending
//! `DownloadChunk`s, which are added to what arrived before. The `DownloadComplete` CRC
//! over the whole file then shows whether anything went missing across the reconnect.
//! Kept in memory only, one per device: a server restart loses them, and the device
//! is told there's nothing to resume and starts again.
//!
//! A partial download that isn't resumed within `RESUME_WINDOW`, or that the device
//! replaces with a new `DownloadStart`, is recorded as incomplete (see
//! `socket_server::record_incomplete_download`), as it was before resuming existed.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;

use crate::vu_download::DownloadData;

/// How long a partial download is kept for the device to resume. Longer than the
/// device could plausibly keep its VU session open while reconnecting.
pub const RESUME_WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Debug)]
pub struct PartialDownload {
    pub company: String,
    pub imei: String,
    pub generation: u8,
    pub data_types: u8,
    pub data: DownloadData,
    kept_at: Instant,
}

impl PartialDownload {
    pub fn new(company: String, imei: String, generation: u8, data_types: u8, data: DownloadData) -> Self {
        Self { company, imei, generation, data_types, data, kept_at: Instant::now() }
    }

    fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.kept_at) >= RESUME_WINDOW
    }
}

/// By IMEI.
static PARTIALS: Lazy<Mutex<HashMap<String, PartialDownload>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn partials() -> std::sync::MutexGuard<'static, HashMap<String, PartialDownload>> {
    PARTIALS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Keeps a download whose connection dropped. Returns the one it replaces, if the
/// device already had one (it should have resumed or restarted that one instead).
pub fn keep(partial: PartialDownload) -> Option<PartialDownload> {
    partials().insert(partial.imei.clone(), partial)
}

/// Takes the device's partial download, whether or not it's the one being resumed or
/// still within the window: the caller decides, and records it as incomplete if not.
pub fn take(imei: &str) -> Option<PartialDownload> {
    partials().remove(imei)
}

/// Whether `partial` is still within the window to be resumed.
pub fn resumable(partial: &PartialDownload) -> bool {
    !partial.expired(Instant::now())
}

/// Removes and returns the partial downloads kept longer than `RESUME_WINDOW`.
pub fn take_expired() -> Vec<PartialDownload> {
    take_expired_at(Instant::now())
}

fn take_expired_at(now: Instant) -> Vec<PartialDownload> {
    let mut partials = partials();
    let expired: Vec<String> = partials.iter().filter(|(_, p)| p.expired(now)).map(|(imei, _)| imei.clone()).collect();
    expired.iter().filter_map(|imei| partials.remove(imei)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vu_download::Section;

    fn partial_with(imei: &str, bytes: &[u8]) -> PartialDownload {
        let mut data = DownloadData::default();
        data.add(Section::Vu, bytes);
        PartialDownload::new("acme".to_string(), imei.to_string(), 2, 0x03, data)
    }

    fn partial(imei: &str) -> PartialDownload {
        partial_with(imei, &[1, 2, 3])
    }

    #[test]
    fn a_kept_download_is_resumable_once() {
        let imei = "356938035643817";
        assert!(keep(partial(imei)).is_none());
        let taken = take(imei).expect("kept");
        assert!(resumable(&taken));
        assert_eq!(taken.data.vu, vec![1, 2, 3]);
        assert!(take(imei).is_none());
    }

    #[test]
    fn keeping_a_second_download_returns_the_first() {
        let imei = "356938035643825";
        keep(partial(imei));
        let replaced = keep(partial_with(imei, &[9]));
        assert_eq!(replaced.map(|p| p.data.vu), Some(vec![1, 2, 3]));
        assert_eq!(take(imei).map(|p| p.data.vu), Some(vec![9]));
    }

    #[test]
    fn only_expired_downloads_are_swept() {
        let imei = "356938035643833";
        keep(partial(imei));
        assert!(!take_expired_at(Instant::now()).iter().any(|p| p.imei == imei));
        let later = Instant::now() + RESUME_WINDOW;
        assert!(take_expired_at(later).iter().any(|p| p.imei == imei));
        assert!(take(imei).is_none());
    }
}
