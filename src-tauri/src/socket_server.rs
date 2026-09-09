//! TCP listen server.
//!
//! Foundation for replacing the outbound MQTT client (`app_connect`/`mqtt`) with a
//! server that devices connect *into* instead of this app dialing out to a broker.
//! Every accepted connection is handled on its own lightweight tokio task rather than
//! an OS thread, so concurrency scales with available memory/file descriptors instead
//! of thread count — tokio's reactor (epoll/IOCP) is what actually lets one process
//! serve tens of thousands of sockets at once.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::device_protocol::abort_message::{AbortMessage, AbortReason};
use crate::device_protocol::message::Message;
use crate::global_app_handle::server_emit_event;

pub async fn server_status_updater(active_connections: Arc<AtomicUsize>) {
    // Update the connection count on the frontend every 5 seconds.
    // This avoids spamming the frontend with every single connection change, which would be
    // a lot of events if many devices are connecting/disconnecting rapidly.
    loop {
        server_emit_event(active_connections.load(Ordering::Relaxed) as u16);
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// Binds `bind_addr` (e.g. "0.0.0.0:7878") and accepts connections until the listener
/// itself errors out. Each connection is handed to its own task immediately, so a slow
/// or idle client can never block others from being accepted.
///
/// `soft_connection_limit` is not a hard cap: connections are always accepted (so the
/// TCP handshake never fails and the client isn't left hanging at the OS level), but
/// once `active_connections` exceeds the limit, newly accepted connections are flagged
/// as over the soft limit. `handle_connection` currently just logs and closes those —
/// once the device protocol is defined, that's where a "try again shortly" / busy
/// response should be written before closing.
pub async fn run(listen_port: u16, connection_limit: u16) -> io::Result<()> {
    let active_connections = Arc::new(AtomicUsize::new(0));
    let updater = tokio::spawn(server_status_updater(Arc::clone(&active_connections)));
    let listener = TcpListener::bind(("0.0.0.0", listen_port)).await?;
    log::info!(
        "[TCP] phase=listen status=bound addr=0.0.0.0:{} connection_limit={}",
        listen_port,
        connection_limit
    );

    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                // A single failed accept (e.g. a transient resource error) shouldn't take
                // the whole listener down.
                log::warn!("[TCP] phase=accept status=failed err={:?}", e);
                continue;
            }
        };

        if let Err(e) = stream.set_nodelay(true) {
            log::warn!(
                "[TCP] phase=accept status=nodelay_failed peer={} err={:?}",
                peer_addr,
                e
            );
        }

        let count = active_connections.fetch_add(1, Ordering::Relaxed) + 1;
        let over_soft_limit = count > connection_limit as usize;
        log::info!(
            "[TCP] phase=connect status=accepted peer={} active_connections={} over_soft_limit={}",
            peer_addr,
            count,
            over_soft_limit
        );

        let active_connections = Arc::clone(&active_connections);
        tokio::spawn(async move {
            handle_connection(stream, peer_addr, over_soft_limit).await;

            let remaining = active_connections.fetch_sub(1, Ordering::Relaxed) - 1;
            log::info!(
                "[TCP] phase=disconnect status=closed peer={} active_connections={}",
                peer_addr,
                remaining
            );
        });
    }
    updater.abort();
}

/// Per-connection handler. Placeholder: reads and logs incoming bytes until the peer
/// disconnects or errors. Replace the body with real protocol parsing/dispatch once
/// the device protocol is defined.
async fn handle_connection(mut stream: TcpStream, peer_addr: SocketAddr, over_soft_limit: bool) {
    if over_soft_limit {
        log::warn!(
            "[TCP] phase=connect status=over_soft_limit peer={} action=reject",
            peer_addr
        );
        // TODO: once the device protocol is defined, write a "try again shortly" /
        // busy response here before closing instead of dropping silently, e.g.:
        //   let _ = stream.write_all(BUSY_RESPONSE).await;
        let _ = stream; // dropping closes the socket
        return;
    }

    let mut buf = [0u8; 4096];
    let (mut read_half, mut write_half) = stream.split();
    let mut message_reader = crate::device_protocol::message::MessageReader::new(read_half);

    loop {

        match message_reader.read_message().await {
            Some(Ok(message)) => {
                log::debug!("[TCP] peer={} received message", peer_addr);
                match message {
                    Message::Ping(_) => {
                        log::debug!("[TCP] peer={} message=Ping", peer_addr);
                    }
                    Message::Pong(_) => {
                        log::debug!("[TCP] peer={} message=Pong", peer_addr);
                    },
                    Message::ConnAnnounce(conn_announce_message) => {
                        // conn_announce_message.handle(&mut write_half, peer_addr).await;
                    },
                    Message::ConnResponse(conn_response_message) => {
                        let reply = Message::Abort(AbortMessage{reason: AbortReason::ProtocolViolation});
                        let reply_bytes: Vec<u8> = reply.into();
                        
                        write_half.write_all(&reply_bytes).await.unwrap();
                    },
                    Message::Ready(ready_message) => {
                        
                    },
                        Message::TachoToCard(tacho_to_card_message) => {
                        
                    },
                    Message::CardToTacho(card_to_tacho_message) => {
                        
                    },
                    Message::Abort(abort_message) => {
                        log::info!("[TCP] peer={} message=Abort action=close reason={}", peer_addr, abort_message.reason.to_string());
                    },
                }
            }
            Some(Err(e)) => {
                log::warn!("[TCP] peer={} read error: {:?}", peer_addr, e.message);
                break;
            }
            None => {
                log::debug!("[TCP] peer={} closed the connection", peer_addr);
                break;
            }
        }
    }
}
