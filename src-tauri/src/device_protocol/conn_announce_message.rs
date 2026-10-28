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
    /// Set by a device whose connection dropped while it was handing over a download:
    /// it's still reading the download from the VU and wants to carry on from where
    /// the server got to, without a card (see `partial_downloads.rs`). One trailing
    /// byte after the firmware version, nonzero = set; absent = not set.
    pub resuming_download: bool,
    /// When resuming, how many bytes of the download the device knows it sent up to
    /// the end of its last complete section: the server keeps that many and the device
    /// carries on from there. 4 bytes (u32 BE) after the resume flag, from firmware
    /// D7.0.83; absent before. Meaningless when not resuming.
    pub confirmed_bytes: Option<u32>,
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
        if ![9, 10, 14].contains(&(bytes.len().wrapping_sub(fw_len))) {
            return Err(ProtocolError { message: "ConnAnnounce firmware_version length mismatch".to_string() });
        }
        let firmware_version = String::from_utf8(bytes[9..9 + fw_len].to_vec())
            .map_err(|_| ProtocolError { message: "Invalid UTF-8 in firmware_version".to_string() })?;

        let resuming_download = bytes.get(9 + fw_len).is_some_and(|&flag| flag != 0);
        let confirmed_bytes = bytes
            .get(10 + fw_len..14 + fw_len)
            .map(|field| u32::from_be_bytes(field.try_into().unwrap()));

        Ok(ConnAnnounceMessage {
            imei,
            firmware_version,
            resuming_download,
            confirmed_bytes,
        })
    }
}

impl Into<Vec<u8>> for ConnAnnounceMessage {
    fn into(self) -> Vec<u8> {
        let imei_int: u64 = self.imei.parse().unwrap_or(0);
        let fw_bytes = self.firmware_version.into_bytes();
        let mut bytes = Vec::with_capacity(14 + fw_bytes.len());
        bytes.extend_from_slice(&imei_int.to_be_bytes());
        bytes.push(fw_bytes.len() as u8);
        bytes.extend_from_slice(&fw_bytes);
        bytes.push(self.resuming_download as u8);
        if let Some(confirmed_bytes) = self.confirmed_bytes {
            bytes.extend_from_slice(&confirmed_bytes.to_be_bytes());
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announce(trailing: &[u8]) -> Vec<u8> {
        let mut bytes = 356938035643809u64.to_be_bytes().to_vec();
        bytes.push(3);
        bytes.extend_from_slice(b"1.2");
        bytes.extend_from_slice(trailing);
        bytes
    }

    fn parse(bytes: Vec<u8>) -> ConnAnnounceMessage {
        ConnAnnounceMessage::try_from(bytes).unwrap_or_else(|e| panic!("{}", e.message))
    }

    #[test]
    fn the_resume_flag_is_the_trailing_byte() {
        assert!(!parse(announce(&[])).resuming_download);
        assert!(!parse(announce(&[0])).resuming_download);
        let resuming = parse(announce(&[1]));
        assert!(resuming.resuming_download);
        assert_eq!(resuming.firmware_version, "1.2");
        assert!(ConnAnnounceMessage::try_from(announce(&[1, 0])).is_err());
    }

    #[test]
    fn the_confirmed_bytes_follow_the_resume_flag() {
        assert_eq!(parse(announce(&[1])).confirmed_bytes, None);
        let resuming = parse(announce(&[1, 0x00, 0x01, 0x02, 0x03]));
        assert!(resuming.resuming_download);
        assert_eq!(resuming.confirmed_bytes, Some(0x0001_0203));
        assert!(ConnAnnounceMessage::try_from(announce(&[1, 0, 0])).is_err());
    }
}
