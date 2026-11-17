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
    /// The card in the VU's driver slot, as the tachograph broadcasts it in FMS
    /// Driver Identification (PGN 0xFE6B), e.g. "UK 1234567890123456". A length byte
    /// and that many ASCII bytes after `confirmed_bytes` (which is then always
    /// present); absent before firmware that sends it, and zero length when the
    /// slot is empty or the vehicle doesn't broadcast it. Lets the server allow a
    /// download because the card is due even when the VU isn't — the device
    /// announces as soon as this changes, whatever it was last told to wait.
    pub driver_card: Option<String>,
    /// The card in the co-driver slot, the same way (FMS's second driver ID): a
    /// length byte and ASCII after `driver_card`, absent from firmware that only
    /// sends the driver slot.
    pub co_driver_card: Option<String>,
}

/// Reads a length-prefixed string at `at`: the string and where the next field starts.
fn length_prefixed(bytes: &[u8], at: usize) -> Option<(String, usize)> {
    let len = *bytes.get(at)? as usize;
    let text = bytes.get(at + 1..at + 1 + len)?;
    // Not UTF-8-strict: it's only ever compared against card numbers, so anything
    // unexpected just fails to match rather than rejecting the whole announce.
    Some((String::from_utf8_lossy(text).into_owned(), at + 1 + len))
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
        let length_mismatch = || ProtocolError { message: "ConnAnnounce firmware_version length mismatch".to_string() };
        // After the firmware version: nothing, the resume flag, flag + confirmed_bytes,
        // then optionally a length-prefixed driver card and after it a co-driver card,
        // with nothing left over.
        let cards_at = 14 + fw_len;
        let mut driver_card = None;
        let mut co_driver_card = None;
        if bytes.len() > cards_at {
            let (card, next) = length_prefixed(&bytes, cards_at).ok_or_else(length_mismatch)?;
            driver_card = Some(card);
            if bytes.len() > next {
                let (card, next) = length_prefixed(&bytes, next).ok_or_else(length_mismatch)?;
                co_driver_card = Some(card);
                if bytes.len() != next {
                    return Err(length_mismatch());
                }
            }
        } else if bytes.len() < 9 + fw_len || ![0, 1, 5].contains(&(bytes.len() - 9 - fw_len)) {
            return Err(length_mismatch());
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
            driver_card,
            co_driver_card,
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
        let driver_card = self.driver_card.or_else(|| self.co_driver_card.as_ref().map(|_| String::new()));
        if self.confirmed_bytes.is_some() || driver_card.is_some() {
            bytes.extend_from_slice(&self.confirmed_bytes.unwrap_or(0).to_be_bytes());
        }
        for card in [driver_card, self.co_driver_card].into_iter().flatten() {
            let card = card.as_bytes();
            let len = card.len().min(u8::MAX as usize);
            bytes.push(len as u8);
            bytes.extend_from_slice(&card[..len]);
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

    #[test]
    fn the_driver_card_follows_the_confirmed_bytes() {
        assert_eq!(parse(announce(&[0, 0, 0, 0, 0])).driver_card, None);

        let empty_slot = parse(announce(&[0, 0, 0, 0, 0, 0]));
        assert_eq!(empty_slot.driver_card.as_deref(), Some(""));
        assert!(!empty_slot.resuming_download);

        let mut trailing = vec![0, 0, 0, 0, 0, 19];
        trailing.extend_from_slice(b"UK 1234567890123456");
        let with_card = parse(announce(&trailing));
        assert_eq!(with_card.driver_card.as_deref(), Some("UK 1234567890123456"));
        assert_eq!(with_card.confirmed_bytes, Some(0));

        assert_eq!(with_card.co_driver_card, None);

        // Length byte disagreeing with what follows.
        assert!(ConnAnnounceMessage::try_from(announce(&[0, 0, 0, 0, 0, 3, b'A'])).is_err());
        assert!(ConnAnnounceMessage::try_from(announce(&[0, 0, 0, 0, 0, 1, b'A', b'B'])).is_err());
        // Partial confirmed_bytes is still wrong.
        assert!(ConnAnnounceMessage::try_from(announce(&[0, 0, 0])).is_err());
    }

    #[test]
    fn the_co_driver_card_follows_the_driver_card() {
        let mut trailing = vec![0, 0, 0, 0, 0, 0, 16];
        trailing.extend_from_slice(b"DB12135162163822");
        let co_driver_only = parse(announce(&trailing));
        assert_eq!(co_driver_only.driver_card.as_deref(), Some(""));
        assert_eq!(co_driver_only.co_driver_card.as_deref(), Some("DB12135162163822"));

        let both = parse(announce(&[0, 0, 0, 0, 0, 2, b'A', b'B', 1, b'C']));
        assert_eq!((both.driver_card.as_deref(), both.co_driver_card.as_deref()), (Some("AB"), Some("C")));

        // Anything after the co-driver card, or a co-driver length running past the end.
        assert!(ConnAnnounceMessage::try_from(announce(&[0, 0, 0, 0, 0, 0, 0, 9])).is_err());
        assert!(ConnAnnounceMessage::try_from(announce(&[0, 0, 0, 0, 0, 0, 2, b'A'])).is_err());
    }

    #[test]
    fn driver_cards_round_trip() {
        let message = |driver_card: Option<&str>, co_driver_card: Option<&str>| ConnAnnounceMessage {
            imei: "356938035643809".to_string(),
            firmware_version: "1.2".to_string(),
            resuming_download: false,
            confirmed_bytes: None,
            driver_card: driver_card.map(str::to_string),
            co_driver_card: co_driver_card.map(str::to_string),
        };
        let round_trip = |m: ConnAnnounceMessage| -> ConnAnnounceMessage { parse(m.into()) };

        let parsed = round_trip(message(Some("UK 1234567890123456"), None));
        assert_eq!(parsed.driver_card.as_deref(), Some("UK 1234567890123456"));
        assert_eq!(parsed.co_driver_card, None);
        assert_eq!(parsed.confirmed_bytes, Some(0));

        let parsed = round_trip(message(None, Some("DB12135162163822")));
        assert_eq!(parsed.driver_card.as_deref(), Some(""));
        assert_eq!(parsed.co_driver_card.as_deref(), Some("DB12135162163822"));
    }
}
