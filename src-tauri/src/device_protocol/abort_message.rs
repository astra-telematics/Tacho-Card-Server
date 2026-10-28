use crate::device_protocol::common::{ProtocolError, Result};

#[derive(Clone)]
pub enum AbortReason {
    Unknown,
    ProtocolViolation,
    CardRemoved,
    CardProtocolMismatch,
    DownloadTooLarge,
}

pub struct AbortMessage {
    pub reason: AbortReason,
    /// From the device only: which VU exchange failed, and how. The server's own
    /// aborts carry just the reason.
    pub vu_failure: Option<VuFailure>,
}

/// The VU exchange a device was in when it aborted, the UDS status it got, and (from
/// firmware H7.0.82) the VU's raw negative response code. Kept as the raw bytes so
/// a value newer firmware adds still reaches the log.
pub struct VuFailure {
    pub stage: u8,
    pub uds_status: u8,
    pub nrc: Option<u8>,
}

impl VuFailure {
    /// The firmware's `abort_stage_t`.
    pub fn stage_name(&self) -> &'static str {
        match self.stage {
            0 => "None",
            1 => "SessionOpen",
            2 => "CardReady",
            3 => "CardAuth",
            4 => "DownloadRequest",
            5 => "UploadRequest",
            6 => "TransferData",
            7 => "Activities",
            8 => "TransferExit",
            9 => "CardDownload",
            _ => "Unrecognised",
        }
    }

    /// The firmware's `tacho_uds_status_t`.
    pub fn uds_status_name(&self) -> &'static str {
        match self.uds_status {
            0 => "Ok",
            1 => "Timeout",
            2 => "UnexpectedResponse",
            3 => "Busy",
            4 => "Pending",
            5 => "SessionLost",
            6 => "Rejected",
            _ => "Unrecognised",
        }
    }

    /// The ISO 14229 negative response code's name.
    pub fn nrc_name(nrc: u8) -> &'static str {
        match nrc {
            0x00 => "None",
            0x10 => "generalReject",
            0x11 => "serviceNotSupported",
            0x12 => "subFunctionNotSupported",
            0x13 => "incorrectMessageLengthOrInvalidFormat",
            0x14 => "responseTooLong",
            0x21 => "busyRepeatRequest",
            0x22 => "conditionsNotCorrect",
            0x24 => "requestSequenceError",
            0x31 => "requestOutOfRange",
            0x33 => "securityAccessDenied",
            0x35 => "invalidKey",
            0x36 => "exceedNumberOfAttempts",
            0x37 => "requiredTimeDelayNotExpired",
            0x70 => "uploadDownloadNotAccepted",
            0x71 => "transferDataSuspended",
            0x72 => "generalProgrammingFailure",
            0x73 => "wrongBlockSequenceCounter",
            0x78 => "responsePending",
            0x7E => "subFunctionNotSupportedInActiveSession",
            0x7F => "serviceNotSupportedInActiveSession",
            _ => "Unrecognised",
        }
    }
}

impl ToString for AbortReason {
    fn to_string(&self) -> String {
        match self {
            AbortReason::Unknown => "Unknown".to_string(),
            AbortReason::ProtocolViolation => "Protocol Violation".to_string(),
            AbortReason::CardRemoved => "Card Removed".to_string(),
            AbortReason::CardProtocolMismatch => "Card Protocol Mismatch".to_string(),
            AbortReason::DownloadTooLarge => "Download Too Large".to_string(),
        }
    }
}

impl TryFrom<Vec<u8>> for AbortMessage {
    type Error = ProtocolError;

    fn try_from(value: Vec<u8>) -> Result<Self> {
        let raw_reason_byte = value.get(0).copied().ok_or(ProtocolError { message: "Missing abort reason".to_string() })?;
        let reason = AbortReason::try_from(raw_reason_byte)?;
        let vu_failure = match value.len() {
            1 => None,
            3 => Some(VuFailure { stage: value[1], uds_status: value[2], nrc: None }),
            4 => Some(VuFailure { stage: value[1], uds_status: value[2], nrc: Some(value[3]) }),
            _ => return Err(ProtocolError { message: format!("Invalid abort length {}", value.len()) }),
        };
        Ok(AbortMessage { reason, vu_failure })
    }
}

impl Into<Vec<u8>> for AbortMessage {
    fn into(self) -> Vec<u8> {
        let mut bytes = vec![self.reason.into()];
        if let Some(failure) = self.vu_failure {
            bytes.extend([failure.stage, failure.uds_status]);
            bytes.extend(failure.nrc);
        }
        bytes
    }
}

// Implementation mapping u8 values to AbortReason enum variants and vice versa
impl TryFrom<u8> for AbortReason {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x00 => Ok(AbortReason::Unknown),
            0x01 => Ok(AbortReason::ProtocolViolation),
            0x02 => Ok(AbortReason::CardRemoved),
            0x03 => Ok(AbortReason::CardProtocolMismatch),
            0x04 => Ok(AbortReason::DownloadTooLarge),
            _ => Err(ProtocolError { message: "Invalid abort reason".to_string() }),
        }
    }
}

impl Into<u8> for AbortReason {
    fn into(self) -> u8 {
        match self {
            AbortReason::Unknown => 0x00,
            AbortReason::ProtocolViolation => 0x01,
            AbortReason::CardRemoved => 0x02,
            AbortReason::CardProtocolMismatch => 0x03,
            AbortReason::DownloadTooLarge => 0x04,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> AbortMessage {
        AbortMessage::try_from(bytes.to_vec()).unwrap_or_else(|e| panic!("{}", e.message))
    }

    #[test]
    fn a_device_abort_carries_the_stage_and_uds_status() {
        let abort = parse(&[0x00, 6, 1]);
        let failure = abort.vu_failure.expect("stage and status");
        assert_eq!((failure.stage_name(), failure.uds_status_name()), ("TransferData", "Timeout"));
    }

    #[test]
    fn a_fourth_byte_is_the_nrc() {
        let failure = parse(&[0x00, 6, 6, 0x24]).vu_failure.expect("stage and status");
        assert_eq!(failure.nrc, Some(0x24));
        assert_eq!(VuFailure::nrc_name(0x24), "requestSequenceError");
        assert_eq!(parse(&[0x00, 6, 6]).vu_failure.expect("stage and status").nrc, None);
    }

    #[test]
    fn a_reason_alone_has_no_vu_failure() {
        assert!(parse(&[0x01]).vu_failure.is_none());
    }

    #[test]
    fn values_newer_firmware_adds_still_parse() {
        let failure = parse(&[0x00, 42, 99]).vu_failure.expect("stage and status");
        assert_eq!((failure.stage, failure.uds_status), (42, 99));
        assert_eq!(failure.stage_name(), "Unrecognised");
    }

    #[test]
    fn a_truncated_vu_failure_is_rejected() {
        assert!(AbortMessage::try_from(vec![0x00, 6]).is_err());
    }
}
