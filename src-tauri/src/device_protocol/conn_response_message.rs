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
    pub retry_after_seconds: Option<u32>, // Only present if status is TryAgainLater
    /// Only with Accepted: which `download_start_message::data_types` the server wants
    /// in this download, one byte after the status. Sent only to devices that announced
    /// which cards are in the slots (see `ConnAnnounceMessage::driver_card`), so it can
    /// leave out whatever isn't due - the VU's data, either card - and save the
    /// transfer. Absent means the device downloads what it always has.
    pub data_types: Option<u8>,
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

        let retry_after_seconds = if status == ConnResponseStatus::TryAgainLater && bytes.len() >= 5 {
            Some(u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]))
        } else {
            None
        };

        if status == ConnResponseStatus::TryAgainLater && retry_after_seconds.is_none() {
            return Err(ProtocolError { message: "Missing retry_after_seconds for TryAgainLater status".to_string() });
        }

        let data_types = if status == ConnResponseStatus::Accepted { bytes.get(1).copied() } else { None };

        Ok(ConnResponseMessage {
            status,
            retry_after_seconds,
            data_types,
        })
    }
}


impl Into<Vec<u8>> for ConnResponseMessage {
    fn into(self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let accepted = self.status == ConnResponseStatus::Accepted;
        let status_byte: u8 = self.status.into();
        bytes.push(status_byte);
        if let Some(retry_after) = self.retry_after_seconds {
            bytes.extend_from_slice(&retry_after.to_be_bytes());
        }
        if let Some(data_types) = self.data_types.filter(|_| accepted) {
            bytes.push(data_types);
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(message: ConnResponseMessage) -> (Vec<u8>, ConnResponseMessage) {
        let bytes: Vec<u8> = message.into();
        let parsed = ConnResponseMessage::try_from(bytes.clone()).unwrap_or_else(|e| panic!("{}", e.message));
        (bytes, parsed)
    }

    #[test]
    fn accepted_can_say_which_data_types_to_download() {
        let (bytes, parsed) = round_trip(ConnResponseMessage {
            status: ConnResponseStatus::Accepted,
            retry_after_seconds: None,
            data_types: Some(0x20),
        });
        assert_eq!(bytes, vec![0x01, 0x20]);
        assert_eq!(parsed.data_types, Some(0x20));

        // Without it, just the status byte - what older firmware has always had.
        let (bytes, parsed) = round_trip(ConnResponseMessage {
            status: ConnResponseStatus::Accepted,
            retry_after_seconds: None,
            data_types: None,
        });
        assert_eq!(bytes, vec![0x01]);
        assert_eq!(parsed.data_types, None);
    }

    #[test]
    fn data_types_only_go_with_accepted() {
        let (bytes, parsed) = round_trip(ConnResponseMessage {
            status: ConnResponseStatus::TryAgainLater,
            retry_after_seconds: Some(60),
            data_types: Some(0x20),
        });
        assert_eq!(bytes, vec![0x02, 0, 0, 0, 60]);
        assert_eq!(parsed.data_types, None);
    }
}