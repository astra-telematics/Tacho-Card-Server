use crate::device_protocol::common::{ProtocolError, Result};

#[derive(Debug, PartialEq)]
pub enum ConnResponseStatus {
    Rejected,
    Accepted,
    TryAgainLater,
}

#[derive(Debug)]
pub struct ConnResponseMessage {
    pub status: ConnResponseStatus,
    pub retry_after_seconds: Option<u16>,
}

impl TryFrom<u8> for ConnResponseStatus {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x00 => Ok(ConnResponseStatus::Rejected),
            0x01 => Ok(ConnResponseStatus::Accepted),
            0x02 => Ok(ConnResponseStatus::TryAgainLater),
            _ => Err(ProtocolError { message: "Invalid status".to_string() }),
        }
    }
}

impl Into<u8> for ConnResponseStatus {
    fn into(self) -> u8 {
        match self {
            ConnResponseStatus::Rejected => 0x00,
            ConnResponseStatus::Accepted => 0x01,
            ConnResponseStatus::TryAgainLater => 0x02,
        }
    }
}

impl TryFrom<Vec<u8>> for ConnResponseMessage {
    type Error = ProtocolError;

    fn try_from(bytes: Vec<u8>) -> Result<Self> {
        let status = ConnResponseStatus::try_from(bytes[0])?;

        let retry_after_seconds = if status == ConnResponseStatus::TryAgainLater && bytes.len() >= 3 {
            Some(u16::from_be_bytes([bytes[1], bytes[2]]))
        } else {
            None
        };

        if status == ConnResponseStatus::TryAgainLater && retry_after_seconds.is_none() {
            return Err(ProtocolError { message: "Missing retry_after_seconds for TryAgainLater status".to_string() });
        }

        Ok(ConnResponseMessage {
            status,
            retry_after_seconds,
        })
    }
}


impl Into<Vec<u8>> for ConnResponseMessage {
    fn into(self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let status_byte: u8 = self.status.into();
        bytes.push(status_byte);
        if let Some(retry_after) = self.retry_after_seconds {
            bytes.extend_from_slice(&retry_after.to_be_bytes());
        }
        bytes
    }
}