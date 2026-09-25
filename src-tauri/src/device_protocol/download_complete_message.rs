use crate::device_protocol::common::{ProtocolError, Result};

/// Marks the end of a VU download transfer. `crc32` is computed by the device
/// over the complete reassembled file — lets the server detect a corrupted/
/// incomplete transfer and tell the device to redownload via `DownloadAck`
/// rather than silently persisting bad data.
pub struct DownloadCompleteMessage {
    pub crc32: u32,
}

impl TryFrom<Vec<u8>> for DownloadCompleteMessage {
    type Error = ProtocolError;

    fn try_from(value: Vec<u8>) -> Result<Self> {
        if value.len() != 4 {
            return Err(ProtocolError { message: "DownloadComplete must be exactly 4 bytes".to_string() });
        }
        let crc32 = u32::from_be_bytes(value.try_into().unwrap());
        Ok(DownloadCompleteMessage { crc32 })
    }
}

impl Into<Vec<u8>> for DownloadCompleteMessage {
    fn into(self) -> Vec<u8> {
        self.crc32.to_be_bytes().to_vec()
    }
}
