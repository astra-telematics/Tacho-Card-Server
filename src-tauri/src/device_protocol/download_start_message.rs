use crate::device_protocol::common::{ProtocolError, Result};

/// Which VU data types are concatenated into this one download file. Per
/// Annex 1C Appendix 7 (DDP_034), a whole download session — even one that
/// pulls several of these — is stored as a single physical file, so one
/// `DownloadStart` covers everything in it and this is a bitmask rather than
/// a single value.
pub mod data_types {
    pub const OVERVIEW: u8 = 0x01;
    pub const ACTIVITIES: u8 = 0x02;
    pub const EVENTS_AND_FAULTS: u8 = 0x04;
    pub const DETAILED_SPEED: u8 = 0x08;
    pub const TECHNICAL_DATA: u8 = 0x10;
}

/// Sent by the device to begin handing over a completed VU download. The
/// server never talks to the VU itself — `RequestUpload`/`TransferData`/
/// `RequestTransferExit` happen entirely on the device's own CAN/UDS link —
/// so this is purely a "here comes a file" announcement. Sent once per whole
/// download session, not once per data type (see `data_types` module doc).
pub struct DownloadStartMessage {
    /// 1 or 2 — which tachograph generation's `Overview` block layout this
    /// download uses. Told to us by the device rather than detected, since it
    /// already knows which UDS dialect it used against the VU.
    pub generation: u8,
    /// Bitmask of `data_types::*` flags — which data types are concatenated
    /// into this file. Not interpreted server-side beyond storage/logging;
    /// kept as a bitmask so a mixed download (e.g. Overview + Events & Faults
    /// but not Activities) can actually be described.
    pub data_types: u8,
}

impl TryFrom<Vec<u8>> for DownloadStartMessage {
    type Error = ProtocolError;

    fn try_from(value: Vec<u8>) -> Result<Self> {
        if value.len() != 2 {
            return Err(ProtocolError { message: "DownloadStart must be exactly 2 bytes".to_string() });
        }
        let generation = value[0];
        if generation != 1 && generation != 2 {
            return Err(ProtocolError { message: format!("Invalid generation {} in DownloadStart", generation) });
        }
        Ok(DownloadStartMessage { generation, data_types: value[1] })
    }
}

impl Into<Vec<u8>> for DownloadStartMessage {
    fn into(self) -> Vec<u8> {
        vec![self.generation, self.data_types]
    }
}
