use crate::device_protocol::common::{ProtocolError, Result};

#[derive(Debug)]
pub struct TachoToCardMessage {
    pub data: Vec<u8>,
}


impl TryFrom<Vec<u8>> for TachoToCardMessage {
    type Error = ProtocolError;

    fn try_from(bytes: Vec<u8>) -> Result<Self> {
        Ok(TachoToCardMessage { data: bytes })
    }
}

impl Into<Vec<u8>> for TachoToCardMessage {
    fn into(self) -> Vec<u8> {
        self.data
    }
}
