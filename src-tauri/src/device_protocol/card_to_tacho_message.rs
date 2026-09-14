use crate::device_protocol::common::{ProtocolError, Result};

#[derive(Debug)]
pub struct CardToTachoMessage {
    // No fields for CardToTachoMessage, it's just a signal
}


impl TryFrom<Vec<u8>> for CardToTachoMessage {
    type Error = ProtocolError;

    fn try_from(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() != 0 {
            return Err(ProtocolError { message: "CardToTachoMessage should have no payload".to_string() });
        }
        Ok(CardToTachoMessage {})
    }
}

impl Into<Vec<u8>> for CardToTachoMessage {
    fn into(self) -> Vec<u8> {
        Vec::new() // CardToTachoMessage has no payload
    }
}
