use crate::device_protocol::common::{ProtocolError, Result};

/// One piece of a VU download's raw bytes. `message.rs`'s own 2-byte length
/// already bounds each chunk, so no inner framing is needed here — same shape
/// as `TachoToCardMessage`.
#[derive(Debug)]
pub struct DownloadChunkMessage {
    pub data: Vec<u8>,
}

impl TryFrom<Vec<u8>> for DownloadChunkMessage {
    type Error = ProtocolError;

    fn try_from(bytes: Vec<u8>) -> Result<Self> {
        Ok(DownloadChunkMessage { data: bytes })
    }
}

impl Into<Vec<u8>> for DownloadChunkMessage {
    fn into(self) -> Vec<u8> {
        self.data
    }
}
