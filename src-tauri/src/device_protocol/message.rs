// Messages start with a $, have a 2 byte message type, a 2 byte payload length, followed by the payload itself
// This module serves to provide a struct with methods for serializing and deserializing messages to and from bytes, as well as a method for reading a message from a TcpStream.

use std::time::Duration;

use tokio::{io::AsyncReadExt, time::timeout};

use crate::device_protocol::{
    AbortMessage, CardToTachoMessage, ConnAnnounceMessage, ConnResponseMessage, PingMessage, PongMessage, ReadyMessage, TachoToCardMessage, ready_message};
use crate::device_protocol::{ProtocolError, Result};

pub enum Message {
    Ping(PingMessage),
    Pong(PongMessage),
    ConnAnnounce(ConnAnnounceMessage),
    ConnResponse(ConnResponseMessage),
    Ready(ReadyMessage),
    TachoToCard(TachoToCardMessage),
    CardToTacho(CardToTachoMessage),
    Abort(AbortMessage),
}


pub struct MessageReader<'a> {
    stream: tokio::net::tcp::ReadHalf<'a>,
    buffer: Vec<u8>,
}

impl MessageReader<'_> {
    pub fn new(stream: tokio::net::tcp::ReadHalf<'_>) -> MessageReader<'_> {
        MessageReader {
            stream,
            buffer: Vec::new(),
        }
    }

    pub async fn read_message(&mut self) -> Option<Result<Message>> {
        loop {
            // Find the start byte `$` and discard any leading garbage
            if let Some(pos) = self.buffer.iter().position(|&b| b == b'$') {
                if pos > 0 {
                    self.buffer.drain(0..pos);
                }
            }

            // Need at least 5 bytes for header ('$' + 2 type + 2 length)
            if self.buffer.len() < 5 {
                let mut temp_buffer = [0u8; 1024];
                match self.stream.read(&mut temp_buffer).await {
                    Ok(0) => return None, // Connection closed
                    Ok(n) => {
                        self.buffer.extend_from_slice(&temp_buffer[..n]);
                        continue;
                    }
                    Err(_) => return None, // Read error
                }
            }
            println!("[TCP] peer={} buffer_len={} buffer={:?}", self.stream.as_ref().peer_addr().unwrap(), self.buffer.len(), self.buffer);

            // We have header; read payload length and ensure full message present
            let payload_length = u16::from_be_bytes([self.buffer[3], self.buffer[4]]) as usize;
            let full_len = 5 + payload_length;
            while self.buffer.len() < full_len {
                let mut temp_buffer = [0u8; 1024];
                match self.stream.read(&mut temp_buffer).await {
                    Ok(0) => return None, // Connection closed
                    Ok(n) => {
                        self.buffer.extend_from_slice(&temp_buffer[..n]);
                        continue;
                    }
                    Err(_) => return None, // Read error
                }
            }

            // We have a full message slice; parse only that slice
            let msg_bytes = self.buffer[..full_len].to_vec();
            match Message::try_from(msg_bytes) {
                Ok(message) => {
                    // Remove the parsed message from the buffer and return it
                    self.buffer.drain(0..full_len);
                    return Some(Ok(message));
                }
                Err(e) => {
                    // If parsing failed despite having a full-looking message, drop
                    // the leading byte to try to resynchronize and surface the error.
                    self.buffer.drain(0..1);
                    return Some(Err(e));
                }
            }
        }
    }
}

impl Into<Vec<u8>> for Message {
    fn into(self) -> Vec<u8> {
        let mut bytes = vec![b'$'];
        match self {
            Message::Ping(ping_message) => {
                let payload: Vec<u8> = ping_message.into();
                bytes.extend_from_slice(&0x0000u16.to_be_bytes());  // Message Type
                bytes.extend_from_slice(&(payload.len() as u16).to_be_bytes());       // Payload Length
                bytes.extend_from_slice(&payload);                  // Payload
            },
            Message::Pong(pong_message) => {
                let payload: Vec<u8> = pong_message.into();
                bytes.extend_from_slice(&0x0001u16.to_be_bytes());  // Message Type
                bytes.extend_from_slice(&(payload.len() as u16).to_be_bytes());       // Payload Length
                bytes.extend_from_slice(&payload);                  // Payload
            },
            Message::ConnAnnounce(conn_announce) => {
                let payload: Vec<u8> = conn_announce.into();
                bytes.extend_from_slice(&0x0002u16.to_be_bytes());  // Message Type
                bytes.extend_from_slice(&(payload.len() as u16).to_be_bytes()); // Payload Length
                bytes.extend_from_slice(&payload);                  // Payload
            },
            Message::ConnResponse(conn_response) => {
                let payload: Vec<u8> = conn_response.into();
                bytes.extend_from_slice(&0x0003u16.to_be_bytes());  // Message Type
                bytes.extend_from_slice(&(payload.len() as u16).to_be_bytes()); // Payload Length
                bytes.extend_from_slice(&payload);                  // Payload
            },
            Message::Ready(ready_message) => {
                let payload: Vec<u8> = ready_message.into();
                bytes.extend_from_slice(&0x0004u16.to_be_bytes());  // Message Type
                bytes.extend_from_slice(&(payload.len() as u16).to_be_bytes()); // Payload Length
                bytes.extend_from_slice(&payload);                  // Payload
            },
            Message::TachoToCard(tacho_to_card_message) => {
                let payload: Vec<u8> = tacho_to_card_message.into();
                bytes.extend_from_slice(&0x0005u16.to_be_bytes());  // Message Type
                bytes.extend_from_slice(&(payload.len() as u16).to_be_bytes());       // Payload Length
                bytes.extend_from_slice(&payload);                  // Payload
            },
            Message::CardToTacho(card_to_tacho_message) => {
                let payload: Vec<u8> = card_to_tacho_message.into();
                bytes.extend_from_slice(&0x0006u16.to_be_bytes());  // Message Type
                bytes.extend_from_slice(&(payload.len() as u16).to_be_bytes());       // Payload Length
                bytes.extend_from_slice(&payload);                  // Payload
            },
            Message::Abort(abort_message) => {
                let payload: Vec<u8> = abort_message.into();
                bytes.extend_from_slice(&0x0007u16.to_be_bytes());  // Message Type
                bytes.extend_from_slice(&(payload.len() as u16).to_be_bytes()); // Payload Length
                bytes.extend_from_slice(&payload);                  // Payload
            },
        }
        bytes
    }
}

impl TryFrom<Vec<u8>> for Message {
    type Error = ProtocolError;

    fn try_from(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() < 5 || bytes[0] != b'$' {
            return Err(ProtocolError { message: "Invalid message format".to_string() });
        }
        let message_type = u16::from_be_bytes([bytes[1], bytes[2]]);
        let payload_length = u16::from_be_bytes([bytes[3], bytes[4]]) as usize;
        if bytes.len() < 5 + payload_length {
            return Err(ProtocolError { message: "Incomplete message".to_string() });
        }
        let payload = &bytes[5..5 + payload_length];
        match message_type {
            0x0000 => {
                match PingMessage::try_from(payload.to_vec()) {
                    Ok(ping_message) => Ok(Message::Ping(ping_message)),
                    Err(e) => Err(e),
                }
            },

            0x0001 => {
                match PongMessage::try_from(payload.to_vec()) {
                    Ok(pong_message) => Ok(Message::Pong(pong_message)),
                    Err(e) => Err(e),
                }
            },

            0x0002 => {
                match ConnAnnounceMessage::try_from(payload.to_vec()) {
                    Ok(conn_announce) => Ok(Message::ConnAnnounce(conn_announce)),
                    Err(e) => Err(e),
                }
            },

            0x0003 => {
                match ConnResponseMessage::try_from(payload.to_vec()) {
                    Ok(conn_response) => Ok(Message::ConnResponse(conn_response)),
                    Err(e) => Err(e),
                }
            },

            0x0004 => {
                match ReadyMessage::try_from(payload.to_vec()) {
                    Ok(ready_message) => Ok(Message::Ready(ready_message)),
                    Err(e) => Err(e),
                }
            },

            0x0005 => {
                match TachoToCardMessage::try_from(payload.to_vec()) {
                    Ok(tacho_to_card_message) => Ok(Message::TachoToCard(tacho_to_card_message)),
                    Err(e) => Err(e),
                }
            },

            0x0006 => {
                match CardToTachoMessage::try_from(payload.to_vec()) {
                    Ok(card_to_tacho_message) => Ok(Message::CardToTacho(card_to_tacho_message)),
                    Err(e) => Err(e),
                }
            },
            
            0x0007 => {
                match AbortMessage::try_from(payload.to_vec()) {
                    Ok(abort_message) => Ok(Message::Abort(abort_message)),
                    Err(e) => Err(e),
                }
            },
            _ => Err(ProtocolError { message: "Unknown message type".to_string() }),
        }
    }
}