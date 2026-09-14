use crate::device_protocol::common::{ProtocolError, Result};

#[derive(Debug)]
pub struct TachoToCardMessage {
    // No fields for TachoToCardMessage, it's just a signal
}


impl TryFrom<Vec<u8>> for TachoToCardMessage {
    type Error = ProtocolError;

    fn try_from(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() != 0 {
            return Err(ProtocolError { message: "TachoToCardMessage should have no payload".to_string() });
        }
        Ok(TachoToCardMessage {})
    }
}

impl Into<Vec<u8>> for TachoToCardMessage {
    fn into(self) -> Vec<u8> {
        Vec::new() // TachoToCardMessage has no payload
    }
}
