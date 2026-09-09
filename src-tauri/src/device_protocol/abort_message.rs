use crate::device_protocol::common::{ProtocolError, Result};

#[derive(Clone)]
pub enum AbortReason {
    Unknown,
    ProtocolViolation,
}

pub struct AbortMessage {
    pub reason: AbortReason,
}

impl ToString for AbortReason {
    fn to_string(&self) -> String {
        match self {
            AbortReason::Unknown => "Unknown".to_string(),
            AbortReason::ProtocolViolation => "Protocol Violation".to_string(),
        }
    }
}

impl TryFrom<Vec<u8>> for AbortMessage {
    type Error = ProtocolError;

    fn try_from(value: Vec<u8>) -> Result<Self> {
        let raw_reason_byte = value.get(0).copied().ok_or(ProtocolError { message: "Missing abort reason".to_string() })?;
        match AbortReason::try_from(raw_reason_byte) {
            Ok(reason) => Ok(AbortMessage { reason }),
            Err(e) => Err(e),
        }
    }
}

impl Into<Vec<u8>> for AbortMessage {
    fn into(self) -> Vec<u8> {
        let reason_byte: u8 = self.reason.into();
        vec![reason_byte]
    }
}

// Implementation mapping u8 values to AbortReason enum variants and vice versa
impl TryFrom<u8> for AbortReason {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x00 => Ok(AbortReason::Unknown),
            0x01 => Ok(AbortReason::ProtocolViolation),
            _ => Err(ProtocolError { message: "Invalid abort reason".to_string() }),
        }
    }
}

impl Into<u8> for AbortReason {
    fn into(self) -> u8 {
        match self {
            AbortReason::Unknown => 0x00,
            AbortReason::ProtocolViolation => 0x01,
        }
    }
}