use crate::device_protocol::common::{ProtocolError, Result};

#[derive(Debug)]
pub struct ReadyMessage {
    // No fields for ReadyMessage, it's just a signal
}


impl TryFrom<Vec<u8>> for ReadyMessage {
    type Error = ProtocolError;

    fn try_from(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() != 0 {
            return Err(ProtocolError { message: "Ready message should have no payload".to_string() });
        }
        Ok(ReadyMessage {})
    }
}

impl Into<Vec<u8>> for ReadyMessage {
    fn into(self) -> Vec<u8> {
        Vec::new() // ReadyMessage has no payload
    }
}