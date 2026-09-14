use crate::device_protocol::common::{ProtocolError, Result};

#[derive(Debug)]
pub struct ConnAnnounceMessage {
    pub imei: String,
    pub vin: Option<String>,
}


impl TryFrom<Vec<u8>> for ConnAnnounceMessage {
    type Error = ProtocolError;

    fn try_from(bytes: Vec<u8>) -> Result<Self> {

        // IMEI is a 64bit int on the wire, but we will store it as a string for easier handling
        let imei = u64::from_be_bytes(bytes[0..8].try_into().unwrap()).to_string();

        // Validate IMEI using Luhn algorithm as a basic sanity check
        if !luhn::valid(&imei) {
            return Err(ProtocolError { message: "Invalid IMEI".to_string() });
        }

        let vin = if bytes.len() > 8 {
            Some(String::from_utf8(bytes[8..].to_vec()).map_err(|_| ProtocolError { message: "Invalid UTF-8 in VIN".to_string() })?)
        } else {
            None
        };


        Ok(ConnAnnounceMessage {
            imei,
            vin,
        })
    }
}

impl Into<Vec<u8>> for ConnAnnounceMessage {
    fn into(self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let imei_int: u64 = self.imei.parse().unwrap_or(0);
        bytes.extend_from_slice(&imei_int.to_be_bytes());
        if let Some(vin) = self.vin {
            bytes.extend_from_slice(vin.as_bytes());
        }
        bytes
    }
}
