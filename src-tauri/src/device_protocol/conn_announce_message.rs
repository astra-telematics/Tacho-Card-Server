use crate::device_protocol::common::{ProtocolError, Result};

#[derive(Debug)]
pub struct ConnAnnounceMessage {
    pub imei: String,
    /// The device's own firmware version — not for telemetry (a firmware
    /// version can narrow down a specific client/deployment, which would
    /// defeat the point of the anonymized telemetry in `telemetry.rs`), but
    /// so the server can adjust how it talks to a given device if a future
    /// firmware version ever needs different handling.
    pub firmware_version: String,
}


impl TryFrom<Vec<u8>> for ConnAnnounceMessage {
    type Error = ProtocolError;

    fn try_from(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() < 9 {
            return Err(ProtocolError { message: "ConnAnnounce too short".to_string() });
        }

        // IMEI is a 64bit int on the wire, but we will store it as a string for easier handling
        let imei = u64::from_be_bytes(bytes[0..8].try_into().unwrap()).to_string();

        // Validate IMEI using Luhn algorithm as a basic sanity check
        if !luhn::valid(&imei) {
            return Err(ProtocolError { message: "Invalid IMEI".to_string() });
        }

        let fw_len = bytes[8] as usize;
        if bytes.len() != 9 + fw_len {
            return Err(ProtocolError { message: "ConnAnnounce firmware_version length mismatch".to_string() });
        }
        let firmware_version = String::from_utf8(bytes[9..9 + fw_len].to_vec())
            .map_err(|_| ProtocolError { message: "Invalid UTF-8 in firmware_version".to_string() })?;

        Ok(ConnAnnounceMessage {
            imei,
            firmware_version,
        })
    }
}

impl Into<Vec<u8>> for ConnAnnounceMessage {
    fn into(self) -> Vec<u8> {
        let imei_int: u64 = self.imei.parse().unwrap_or(0);
        let fw_bytes = self.firmware_version.into_bytes();
        let mut bytes = Vec::with_capacity(9 + fw_bytes.len());
        bytes.extend_from_slice(&imei_int.to_be_bytes());
        bytes.push(fw_bytes.len() as u8);
        bytes.extend_from_slice(&fw_bytes);
        bytes
    }
}
