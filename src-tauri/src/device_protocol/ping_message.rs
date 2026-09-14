use crate::device_protocol::common::{ProtocolError, Result};

#[derive(Debug)]
pub struct PingMessage {
    // No fields for PingMessage, it's just a signal
}


impl TryFrom<Vec<u8>> for PingMessage {
    type Error = ProtocolError;

    fn try_from(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() != 0 {
            return Err(ProtocolError { message: "Ping message should have no payload".to_string() });
        }
        Ok(PingMessage {})
    }
}

impl Into<Vec<u8>> for PingMessage {
    fn into(self) -> Vec<u8> {
        Vec::new() // PingMessage has no payload
    }
}