use crate::device_protocol::common::{ProtocolError, Result};

#[derive(Debug)]
pub struct CardToTachoMessage {
    pub data: Vec<u8>,
}


impl TryFrom<Vec<u8>> for CardToTachoMessage {
    type Error = ProtocolError;

    fn try_from(bytes: Vec<u8>) -> Result<Self> {
        Ok(CardToTachoMessage { data: bytes })
    }
}

impl Into<Vec<u8>> for CardToTachoMessage {
    fn into(self) -> Vec<u8> {
        self.data
    }
}
