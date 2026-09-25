use crate::device_protocol::common::{ProtocolError, Result};

/// Server's reply to `DownloadComplete`: whether the CRC32 matched and the
/// file was saved (`true`), or the device should redownload (`false`).
pub struct DownloadAckMessage {
    pub success: bool,
}

impl TryFrom<Vec<u8>> for DownloadAckMessage {
    type Error = ProtocolError;

    fn try_from(value: Vec<u8>) -> Result<Self> {
        let byte = value.get(0).copied().ok_or(ProtocolError { message: "Missing download ack".to_string() })?;
        Ok(DownloadAckMessage { success: byte != 0 })
    }
}

impl Into<Vec<u8>> for DownloadAckMessage {
    fn into(self) -> Vec<u8> {
        vec![if self.success { 1 } else { 0 }]
    }
}
