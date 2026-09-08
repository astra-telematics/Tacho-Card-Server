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

use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

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
    let listener = TcpListener::bind(("0.0.0.0", listen_port)).await?;
    log::info!(
        "[TCP] phase=listen status=bound addr=0.0.0.0:{} connection_limit={}",
        listen_port,
        connection_limit
    );

    let active_connections = Arc::new(AtomicUsize::new(0));

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

    loop {
        match stream.read(&mut buf).await {
            Ok(0) => {
                log::debug!("[TCP] peer={} closed the connection", peer_addr);
                break;
            }
            Ok(n) => {
                log::debug!("[TCP] peer={} received {} bytes", peer_addr, n);
                // TODO: parse/dispatch the device protocol here.
            }
            Err(e) => {
                log::warn!("[TCP] peer={} read error: {:?}", peer_addr, e);
                break;
            }
        }
    }
}
