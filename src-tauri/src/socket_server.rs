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
use tokio::net::tcp::WriteHalf;
use tokio::net::{TcpListener, TcpStream};

use crate::device_protocol::{AbortMessage, AbortReason};
use crate::device_protocol::Message;
use crate::global_app_handle::server_emit_event;

/// Suggested wait before a device retries (reconnects and re-announces) after being
/// turned away because the auth queue (see `card_pool`) was already full.
const AUTH_QUEUE_FULL_RETRY_SECONDS: u16 = 5;

/// How long a freshly-accepted connection has to send `ConnAnnounce` before it's
/// dropped.
const CONN_ANNOUNCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// How long an announced-and-accepted device may go without a card (queued, or
/// waiting for a replacement after one was pulled) before it's dropped.
const AWAITING_CARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
/// How long a device holding a card has, after `Ready` (or after any message), to
/// send its next message before it's dropped — keeps a device that goes silent
/// mid-authentication from sitting on a card forever.
const AUTHENTICATING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The read timeout for the connection's *current* phase. Any fully-received
/// message (Ping, Pong, or anything else) implicitly resets the clock: the caller
/// reconstructs this timeout fresh on every loop iteration, so receiving a message —
/// or a background card event like being granted/re-granted a card — always starts a
/// brand new window rather than counting down from when the connection first opened.
fn current_read_timeout(announced: bool, has_card: bool) -> std::time::Duration {
    if !announced {
        CONN_ANNOUNCE_TIMEOUT
    } else if has_card {
        AUTHENTICATING_TIMEOUT
    } else {
        AWAITING_CARD_TIMEOUT
    }
}

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
    println!(
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
            println!(
                "[TCP] phase=accept status=nodelay_failed peer={} err={:?}",
                peer_addr,
                e
            );
        }

        let count = active_connections.fetch_add(1, Ordering::Relaxed) + 1;
        let over_soft_limit = count > connection_limit as usize;
        println!(
            "[TCP] phase=connect status=accepted peer={} active_connections={} over_soft_limit={}",
            peer_addr,
            count,
            over_soft_limit
        );

        let active_connections = Arc::clone(&active_connections);
        tokio::spawn(async move {
            handle_connection(stream, peer_addr, over_soft_limit).await;

            let remaining = active_connections.fetch_sub(1, Ordering::Relaxed) - 1;
            println!(
                "[TCP] phase=disconnect status=closed peer={} active_connections={}",
                peer_addr,
                remaining
            );
        });
    }
    updater.abort();
}

async fn send_abort_message(stream: &mut WriteHalf<'_>, reason: AbortReason) {
    let abort_message = AbortMessage { reason };
    let abort_bytes: Vec<u8> = Message::Abort(abort_message).into();
    if let Err(e) = stream.write_all(&abort_bytes).await {
        println!(
            "[TCP] peer={} action=send_abort err={:?}",
            stream.peer_addr().unwrap(),
            e
        );
    }
}

/// Outcome of whichever card-related background wait is currently pending — see
/// `handle_connection`'s `pending` variable.
enum CardEvent {
    /// The device's first `try_reserve` wait resolved with a card.
    Acquired(crate::card_pool::CardGuard),
    /// A priority replacement for a revoked card arrived.
    Replaced(crate::smart_card::ManagedCard),
    /// The currently-held card was pulled.
    Revoked,
}

/// A boxed, pinned card-related wait, re-armed only at actual state transitions (not
/// every loop tick) — see the comment at its construction site in `handle_connection`
/// for why that matters (recreating a semaphore-wait future on every poll would
/// silently reset the device's place in the queue).
type PendingCardEvent = std::pin::Pin<Box<dyn std::future::Future<Output = CardEvent> + Send>>;

/// Polls `pending` if it's armed, or never resolves if it's `None`. A plain
/// `pending.as_mut().unwrap()` guarded by `if pending.is_some()` looks equivalent but
/// isn't: `tokio::select!`'s `if` precondition only skips *polling* that branch — the
/// branch expression itself is still evaluated every iteration regardless, which
/// would unwrap a `None` and panic. Routing through `std::future::pending()` in the
/// `None` case sidesteps that entirely.
async fn poll_pending(pending: &mut Option<PendingCardEvent>) -> CardEvent {
    match pending {
        Some(fut) => fut.as_mut().await,
        None => std::future::pending().await,
    }
}

/// Per-connection handler.
///
/// `Ready`/`Abort(CardRemoved)` are server-initiated: the device announces itself
/// once (`ConnAnnounce`) and then just waits — indefinitely — for the server to send
/// `Ready` when a card is actually available. It never asks for one itself. So this
/// handler does the admission decision (queue-full or not) synchronously right when
/// `ConnAnnounce` arrives, then separately waits for an actual card concurrently with
/// continuing to read whatever else the device sends meanwhile (Ping, Abort, ...).
async fn handle_connection(mut stream: TcpStream, peer_addr: SocketAddr, over_soft_limit: bool) {
    if over_soft_limit {
        println!(
            "[TCP] phase=connect status=over_soft_limit peer={} action=reject",
            peer_addr
        );
        // TODO: once the device protocol is defined, write a "try again shortly" /
        // busy response here before closing instead of dropping silently, e.g.:
        //   let _ = stream.write_all(BUSY_RESPONSE).await;
        let _ = stream; // dropping closes the socket
        return;
    }

    let (read_half, mut write_half) = stream.split();
    let mut message_reader = crate::device_protocol::message::MessageReader::new(read_half);

    let mut announced = false;
    // Set once ConnAnnounce is admitted (ahead of actually holding a card) and stays
    // set until the first card is acquired. Distinct from `card` because there's a
    // real waiting period between admission and having a card in hand.
    let mut awaiting_first_card = false;
    // Dropping this — on any exit from this function, including disconnect, Abort,
    // timeout, or the task being cancelled — returns the card to `CARD_POOL`
    // automatically, which is what lets a waiting device in the queue immediately
    // take its place.
    let mut card: Option<crate::card_pool::CardGuard> = None;
    let mut pending: Option<PendingCardEvent> = None;

    loop {
        // (Re)arm `pending` if nothing is currently in flight for the connection's
        // present state. This only runs right after a transition (ConnAnnounce
        // admitted, card acquired, card replaced, card revoked) since `pending` is
        // only ever reset to `None` at those points — NOT every loop tick. Polling
        // the exact same future across iterations (rather than constructing a fresh
        // one each time) matters here: `wait_after_reserving`'s semaphore wait would
        // otherwise silently lose the device's place in line on every single poll.
        let has_card = card.as_ref().map(|g| g.has_card()).unwrap_or(false);

        if pending.is_none() {
            pending = if awaiting_first_card {
                Some(Box::pin(async {
                    CardEvent::Acquired(crate::card_pool::CARD_POOL.wait_after_reserving().await)
                }))
            } else if let Some(guard) = &card {
                if has_card {
                    let (revoked, notify) = guard.revoked_signal();
                    Some(Box::pin(async move {
                        crate::card_pool::wait_for_revocation_signal(revoked, notify).await;
                        CardEvent::Revoked
                    }))
                } else {
                    Some(Box::pin(async {
                        CardEvent::Replaced(crate::card_pool::CARD_POOL.wait_for_replacement_card().await)
                    }))
                }
            } else {
                None // Not announced yet — nothing to wait for.
            };
        }

        // Reconstructed fresh every iteration with whichever duration applies to the
        // connection's current phase — this is what makes "any message resets the
        // timeout" work for free: a new window starts as soon as the previous read
        // (or card event) completes, using this loop pass's phase, not the phase the
        // connection was in when it first opened.
        let read_fut = tokio::time::timeout(
            current_read_timeout(announced, has_card),
            message_reader.read_message(),
        );

        tokio::select! {
            result = read_fut => match result {
            Ok(Some(Ok(message))) => {
                println!("[TCP] peer={} received message", peer_addr);
                match message {
                    Message::Ping(_) => {
                        let reply = Message::Pong(crate::device_protocol::PongMessage {});
                        let reply_bytes: Vec<u8> = reply.into();
                        println!("[TCP] peer={} message=Ping action=reply Pong, {:?}", peer_addr, reply_bytes);
                        write_half.write_all(&reply_bytes).await.unwrap();
                    },
                    Message::Pong(_) => {
                        println!("[TCP] peer={} message=Pong", peer_addr);
                    },
                    Message::ConnAnnounce(conn_announce_message) => {
                        if announced {
                            // Duplicate announce — just re-ack, don't reserve a second queue slot.
                            let reply = Message::ConnResponse(crate::device_protocol::ConnResponseMessage {
                                status: crate::device_protocol::ConnResponseStatus::Accepted,
                                retry_after_seconds: None,
                            });
                            let reply_bytes: Vec<u8> = reply.into();
                            let _ = write_half.write_all(&reply_bytes).await;
                            println!("[TCP] peer={} IMEI={} message=ConnAnnounce action=reply status=Accepted note=duplicate", peer_addr, conn_announce_message.imei);
                        } else {
                            match crate::card_pool::CARD_POOL.try_reserve() {
                                Ok(()) => {
                                    announced = true;
                                    awaiting_first_card = true;
                                    pending = None; // arm as "Acquired" wait next loop
                                    let reply = Message::ConnResponse(crate::device_protocol::ConnResponseMessage {
                                        status: crate::device_protocol::ConnResponseStatus::Accepted,
                                        retry_after_seconds: None,
                                    });
                                    let reply_bytes: Vec<u8> = reply.into();
                                    write_half.write_all(&reply_bytes).await.unwrap();
                                    println!("[TCP] peer={} IMEI={} message=ConnAnnounce action=reply status=Accepted", peer_addr, conn_announce_message.imei);
                                }
                                Err(crate::card_pool::QueueFull) => {
                                    let reply = Message::ConnResponse(crate::device_protocol::ConnResponseMessage {
                                        status: crate::device_protocol::ConnResponseStatus::TryAgainLater,
                                        retry_after_seconds: Some(AUTH_QUEUE_FULL_RETRY_SECONDS),
                                    });
                                    let reply_bytes: Vec<u8> = reply.into();
                                    let _ = write_half.write_all(&reply_bytes).await;
                                    println!("[TCP] peer={} IMEI={} message=ConnAnnounce action=reject reason=queue_full", peer_addr, conn_announce_message.imei);
                                    return; // Close the connection; device is expected to reconnect later.
                                }
                            }
                        }
                    },
                    Message::ConnResponse(conn_response_message) => {
                        self::send_abort_message(&mut write_half, AbortReason::ProtocolViolation).await;
                        println!("[TCP] peer={} message=ConnResponse action=unexpected status={:?}", peer_addr, conn_response_message.status);
                    },
                    Message::Ready(_) => {
                        // Ready is server -> device only; a device sending it is a protocol error.
                        self::send_abort_message(&mut write_half, AbortReason::ProtocolViolation).await;
                        println!("[TCP] peer={} message=Ready action=abort reason=unexpected_direction", peer_addr);
                    },
                    Message::TachoToCard(tacho_to_card_message) => {
                        let has_card = card.as_ref().map(|g| g.has_card()).unwrap_or(false);
                        if !has_card {
                            // Device must have received Ready before relaying APDUs.
                            self::send_abort_message(&mut write_half, AbortReason::ProtocolViolation).await;
                            println!("[TCP] peer={} message=TachoToCard action=abort reason=no_card_assigned", peer_addr);
                        } else {
                            let guard = card.as_ref().unwrap();
                            let apdu_hex = hex::encode(&tacho_to_card_message.data);
                            match guard.apdu_transmit(&apdu_hex).await {
                                Ok(response_hex) => {
                                    // Passive sniffer: extract useful plaintext EF data as it
                                    // flows through (see apdu_sniffer.rs). We stay transparent —
                                    // the device drives its own authentication against the card
                                    // and we only relay + observe, never inject APDUs of our own.
                                    // Reading the ICCID via `guard.iccid.get()` (sync, cache-only)
                                    // rather than `get_iccid().await` (which can itself talk to
                                    // the card if uncached) makes that a structural guarantee
                                    // here, not just an invariant we're trusting holds elsewhere.
                                    if let Some(iccid) = guard.iccid.get() {
                                        let card_number = crate::config::get_from_cache(crate::config::CacheSection::Cards, iccid);
                                        crate::apdu_sniffer::sniff(&card_number, &apdu_hex, &response_hex);
                                    }

                                    match hex::decode(&response_hex) {
                                        Ok(response_bytes) => {
                                            let reply = Message::CardToTacho(crate::device_protocol::CardToTachoMessage {
                                                data: response_bytes,
                                            });
                                            let reply_bytes: Vec<u8> = reply.into();
                                            let _ = write_half.write_all(&reply_bytes).await;
                                        }
                                        Err(e) => {
                                            log::error!(
                                                "[TCP] peer={} action=tacho_to_card status=bad_response_hex err={:?}",
                                                peer_addr, e
                                            );
                                        }
                                    }
                                }
                                Err(e) => {
                                    // Not necessarily a removed card (could be a bad APDU) —
                                    // card removal is handled separately via the pending
                                    // Revoked wait. Just log for now; there's no defined
                                    // "transmit failed" signal in the protocol yet.
                                    log::error!("[TCP] peer={} action=tacho_to_card status=transmit_failed err={}", peer_addr, e);
                                }
                            }
                        }
                    },
                    Message::CardToTacho(_card_to_tacho_message) => {
                        self::send_abort_message(&mut write_half, AbortReason::ProtocolViolation).await;
                    },
                    Message::Abort(abort_message) => {
                        println!("[TCP] peer={} message=Abort action=close reason={}", peer_addr, abort_message.reason.to_string());
                        break;
                    },
                }
            }
            Ok(Some(Err(e))) => {
                println!("[TCP] peer={} read error: {:?}", peer_addr, e.message);
                break;
            }
            Ok(None) => {
                println!("[TCP] peer={} closed the connection", peer_addr);
                break;
            }
            Err(_) => {
                println!(
                    "[TCP] peer={} read timeout after {:?} (announced={} has_card={})",
                    peer_addr, current_read_timeout(announced, has_card), announced, has_card
                );
                break;
            }
            },
            event = poll_pending(&mut pending) => {
                pending = None; // consumed; re-armed for the new state at the top of the next iteration
                match event {
                    CardEvent::Acquired(guard) => {
                        card = Some(guard);
                        awaiting_first_card = false;
                        let reply_bytes: Vec<u8> = Message::Ready(crate::device_protocol::ReadyMessage {}).into();
                        if write_half.write_all(&reply_bytes).await.is_ok() {
                            println!("[TCP] peer={} action=card_assigned status=ready_sent", peer_addr);
                        }
                    }
                    CardEvent::Replaced(new_card) => {
                        if let Some(guard) = card.as_mut() {
                            guard.install_replacement(new_card);
                        }
                        let reply_bytes: Vec<u8> = Message::Ready(crate::device_protocol::ReadyMessage {}).into();
                        if write_half.write_all(&reply_bytes).await.is_ok() {
                            println!("[TCP] peer={} action=card_reassigned status=ready_sent", peer_addr);
                        }
                    }
                    CardEvent::Revoked => {
                        // Tell the device (it resets to "connected" and waits for the
                        // next Ready) and drop the dead card — but keep this
                        // session's slot reserved: `pending` re-arms as "Replaced"
                        // above, which bypasses the M-capped queue entirely.
                        if let Some(guard) = card.as_mut() {
                            guard.discard_dead_card();
                        }
                        self::send_abort_message(&mut write_half, AbortReason::CardRemoved).await;
                        println!("[TCP] peer={} action=card_removed status=notified", peer_addr);
                    }
                }
            }
        }
    }
}
