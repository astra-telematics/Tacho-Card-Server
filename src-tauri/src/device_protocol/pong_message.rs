use crate::device_protocol::common::{ProtocolError, Result};

#[derive(Debug)]
pub struct PongMessage {
    // No fields for PongMessage, it's just a signal
}


impl TryFrom<Vec<u8>> for PongMessage {
    type Error = ProtocolError;

    fn try_from(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() != 0 {
            return Err(ProtocolError { message: "Pong message should have no payload".to_string() });
        }
        Ok(PongMessage {})
    }
}

impl Into<Vec<u8>> for PongMessage {
    fn into(self) -> Vec<u8> {
        Vec::new() // PongMessage has no payload
    }
}