use crate::device_protocol::common::{ProtocolError, Result};

#[derive(Debug)]
pub struct ReadyMessage {
    /// The card's raw ATR (Answer To Reset), exactly as read from the physical
    /// reader. The device never touches the card directly — this is the only way
    /// it ever sees the ATR. Per ISO/IEC 7816-3 this is 2-33 bytes.
    pub atr: Vec<u8>,
}

impl TryFrom<Vec<u8>> for ReadyMessage {
    type Error = ProtocolError;

    fn try_from(bytes: Vec<u8>) -> Result<Self> {
        Ok(ReadyMessage { atr: bytes })
    }
}

impl Into<Vec<u8>> for ReadyMessage {
    fn into(self) -> Vec<u8> {
        self.atr
    }
}