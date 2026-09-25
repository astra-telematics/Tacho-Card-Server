use crate::device_protocol::common::{ProtocolError, Result};

/// Sent by the device once its own VU-side authentication attempt has
/// concluded (success or failure). The server has no other way to know this —
/// it only ever relays opaque APDU bytes between the device and the physical
/// card, so it can't infer the VU's final verdict on its own.
pub struct AuthOutcomeMessage {
    pub success: bool,
}

impl TryFrom<Vec<u8>> for AuthOutcomeMessage {
    type Error = ProtocolError;

    fn try_from(value: Vec<u8>) -> Result<Self> {
        let byte = value.get(0).copied().ok_or(ProtocolError { message: "Missing auth outcome".to_string() })?;
        Ok(AuthOutcomeMessage { success: byte != 0 })
    }
}

impl Into<Vec<u8>> for AuthOutcomeMessage {
    fn into(self) -> Vec<u8> {
        vec![if self.success { 1 } else { 0 }]
    }
}
