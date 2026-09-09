// Messages start with a $, have a 2 byte message type, a 2 byte payload length, followed by the payload itself
// This module serves to provide a struct with methods for serializing and deserializing messages to and from bytes, as well as a method for reading a message from a TcpStream.

use std::time::Duration;

use tokio::{io::AsyncReadExt, time::timeout};

use crate::device_protocol::{abort_message::AbortMessage, common::{ProtocolError, Result}, conn_announce_message::ConnAnnounceMessage, conn_response_message::ConnResponseMessage};



pub struct PingMessage {}

pub struct PongMessage {}

pub struct ReadyMessage {

}

pub struct TachoToCardMessage {

}

pub struct CardToTachoMessage {

}

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
            // Try to parse a message from the buffer
            if let Ok(message) = Message::try_from(self.buffer.clone()) {
                // Remove the parsed message from the buffer
                let payload_length = u16::from_be_bytes([self.buffer[3], self.buffer[4]]) as usize;
                self.buffer.drain(0..5 + payload_length);
                return Some(Ok(message));
            }

            let mut temp_buffer = [0u8; 1024];
            match self.stream.read(&mut temp_buffer).await {
                Ok(0) => return None, // Connection closed
                Ok(n) => self.buffer.extend_from_slice(&temp_buffer[..n]),
                Err(_) => return None, // Read error
            }
        }
    }
}

impl Into<Vec<u8>> for Message {
    fn into(self) -> Vec<u8> {
        let mut bytes = vec![b'$'];
        match self {
            Message::Ping(_) => {
                bytes.extend_from_slice(&0x0000u16.to_be_bytes());  // Message Type
                bytes.extend_from_slice(&0u16.to_be_bytes());       // Payload Length
            },
            Message::Pong(_) => {
                bytes.extend_from_slice(&0x0001u16.to_be_bytes());  // Message Type
                bytes.extend_from_slice(&0u16.to_be_bytes());       // Payload Length
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
            Message::Ready(_) => {
                bytes.extend_from_slice(&0x0004u16.to_be_bytes());  // Message Type
                bytes.extend_from_slice(&0u16.to_be_bytes());       // Payload Length
            },
            Message::TachoToCard(_) => {
                bytes.extend_from_slice(&0x0005u16.to_be_bytes());  // Message Type
                bytes.extend_from_slice(&0u16.to_be_bytes());       // Payload Length
            },
            Message::CardToTacho(_) => {
                bytes.extend_from_slice(&0x0006u16.to_be_bytes());  // Message Type
                bytes.extend_from_slice(&0u16.to_be_bytes());       // Payload Length
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
                if payload_length != 0 {
                    return Err(ProtocolError { message: "Ping message should have no payload".to_string() });
                }
                Ok(Message::Ping(PingMessage {}))
            },
            0x0001 => {
                if payload_length != 0 {
                    return Err(ProtocolError { message: "Pong message should have no payload".to_string() });
                }
                Ok(Message::Pong(PongMessage {}))
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
            0x0004 => Ok(Message::Ready(ReadyMessage {})),
            0x0005 => Ok(Message::TachoToCard(TachoToCardMessage {})),
            0x0006 => Ok(Message::CardToTacho(CardToTachoMessage {})),
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