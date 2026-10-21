//! TCP listen server.
//!
//! Foundation for replacing the outbound MQTT client (`app_connect`/`mqtt`) with a
//! server that devices connect *into* instead of this app dialing out to a broker.
//! Every accepted connection is handled on its own lightweight tokio task rather than
//! an OS thread, so concurrency scales with available memory/file descriptors instead
//! of thread count — tokio's reactor (epoll/IOCP) is what actually lets one process
//! serve tens of thousands of sockets at once.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::AsyncWriteExt;
use tokio::net::tcp::WriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use lazy_static::lazy_static;

use crate::card_pool::CardPool;
use crate::config::{
    get_from_cache, CacheSection, DEFAULT_COMPANY_ID, DEFAULT_SERVER_CONNECTION_LIMIT,
    DEFAULT_SERVER_LISTEN_PORT,
};
use crate::device_protocol::ConnResponseStatus;
use crate::device_protocol::{AbortMessage, AbortReason};
use crate::device_protocol::Message;
use crate::global_app_handle::server_emit_event;

/// Shortest wait a device is told to retry after when turned away because the raw TCP
/// connection count (`connection_limit`) was over its soft limit — checked at accept
/// time, before the device has said anything at all. The actual delay is
/// `admission`'s estimate of when a card will be free; this floor stops a device
/// hammering the socket when that estimate is near zero. About one authentication's
/// worth (~30 s in testing): sooner than that, nothing will have changed.
const MIN_CONNECTION_LIMIT_RETRY: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a freshly-accepted connection has to send `ConnAnnounce` before it's
/// dropped.
const CONN_ANNOUNCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// How long an announced device without a card may go silent before it's dropped:
/// while queued (or waiting for a replacement card), and after authenticating while it
/// hands over a download. A silence limit, not a total: every message — `Ping`, each
/// `DownloadChunk` — restarts it, so a device can queue or download for as long as it
/// takes provided it keeps talking.
const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
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
        IDLE_TIMEOUT
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

/// Listener settings that can be changed at runtime from the UI (see
/// `config::update_server_config`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerSettings {
    pub listen_port: u16,
    pub connection_limit: u16,
}

lazy_static! {
    /// Live listener settings. `run` watches this: a new `connection_limit` applies to
    /// the next accepted connection, and a new `listen_port` rebinds the listener.
    static ref SETTINGS: watch::Sender<ServerSettings> = watch::Sender::new(ServerSettings {
        listen_port: DEFAULT_SERVER_LISTEN_PORT,
        connection_limit: DEFAULT_SERVER_CONNECTION_LIMIT,
    });

    /// Connections each company currently has admitted (see `CompanySlot`).
    static ref COMPANY_CONNECTIONS: Mutex<HashMap<String, usize>> = Mutex::new(HashMap::new());
}

/// One of a company's connection slots, held from admission at `ConnAnnounce` until the
/// connection ends (dropped with the connection's state), so one company's devices
/// can't crowd out another's.
struct CompanySlot {
    company: String,
}

impl CompanySlot {
    /// Claims a slot if `company` has fewer than `limit` connections admitted. Check and
    /// claim happen under one lock, so two devices can't both take the last slot.
    fn try_claim(company: &str, limit: u16) -> Option<Self> {
        let mut connections = COMPANY_CONNECTIONS.lock().unwrap_or_else(|p| p.into_inner());
        let count = connections.entry(company.to_string()).or_insert(0);
        if *count >= limit as usize {
            return None;
        }
        *count += 1;
        Some(Self { company: company.to_string() })
    }
}

impl Drop for CompanySlot {
    fn drop(&mut self) {
        let mut connections = COMPANY_CONNECTIONS.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(count) = connections.get_mut(&self.company) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                connections.remove(&self.company);
            }
        }
    }
}

/// Pushes new listener settings to the running server (if any). Safe to call before
/// `run` has started — `run` picks up whatever is current when it subscribes.
pub fn apply_settings(listen_port: u16, connection_limit: u16) {
    let new = ServerSettings { listen_port, connection_limit };
    SETTINGS.send_if_modified(|current| {
        if *current == new {
            return false;
        }
        *current = new;
        true
    });
}

async fn bind(listen_port: u16, connection_limit: u16) -> Option<TcpListener> {
    match TcpListener::bind(("0.0.0.0", listen_port)).await {
        Ok(listener) => {
            log::info!(
                "[TCP] phase=listen status=bound addr=0.0.0.0:{} connection_limit={}",
                listen_port,
                connection_limit
            );
            Some(listener)
        }
        Err(e) => {
            log::error!(
                "[TCP] phase=listen status=bind_failed addr=0.0.0.0:{} err={:?}",
                listen_port,
                e
            );
            None
        }
    }
}

/// Accepts on `listener`, or waits forever if there isn't one (bind failed), so the
/// `select!` in `run` just waits for the next settings change instead.
async fn accept(listener: &Option<TcpListener>) -> io::Result<(TcpStream, SocketAddr)> {
    match listener {
        Some(listener) => listener.accept().await,
        None => std::future::pending().await,
    }
}

/// Binds 0.0.0.0:`listen_port` and accepts connections forever. Each connection is
/// handed to its own task immediately, so a slow or idle client can never block others
/// from being accepted.
///
/// Settings changes pushed via `apply_settings` are applied without a restart: a new
/// `connection_limit` takes effect for the next accepted connection, and a new
/// `listen_port` binds a fresh listener and drops the old one. Connections that are
/// already established are unaffected by either — they keep running on their own
/// tasks. If binding the new port fails, the server keeps listening on the old one; if
/// there is no listener at all (initial bind failed), it waits for a settings change.
///
/// `connection_limit` is not a hard cap: connections are always accepted (so the
/// TCP handshake never fails and the client isn't left hanging at the OS level), but
/// once `active_connections` exceeds the limit, newly accepted connections are flagged
/// as over the soft limit. `handle_connection` sends a `ConnResponse::TryAgainLater`
/// before closing those, so a device is told to back off instead of just seeing its
/// connection die. This is the *only* admission gate in the system — deliberately: if
/// the server is ever genuinely overloaded, connections should be turned away here,
/// as early and cheaply as possible, not kept alive only to be rejected later once
/// they've announced (see `card_pool`, which never rejects).
pub async fn run(listen_port: u16, connection_limit: u16) {
    apply_settings(listen_port, connection_limit);
    let mut settings_rx = SETTINGS.subscribe();
    let mut current = *settings_rx.borrow_and_update();

    // Seed the average card session length from previous runs before any device is
    // admitted against it.
    if let Err(e) = tokio::task::spawn_blocking(crate::admission::load_history).await {
        log::error!("[ADMISSION] status=history_load_failed err={:?}", e);
    }

    let active_connections = Arc::new(AtomicUsize::new(0));
    let updater = tokio::spawn(server_status_updater(Arc::clone(&active_connections)));
    let mut listener = bind(current.listen_port, current.connection_limit).await;

    loop {
        let accepted = tokio::select! {
            changed = settings_rx.changed() => {
                if changed.is_err() {
                    break; // Sender is a static, so this never happens in practice.
                }
                let new = *settings_rx.borrow_and_update();
                if new.connection_limit != current.connection_limit {
                    log::info!(
                        "[TCP] phase=config status=connection_limit_changed old={} new={}",
                        current.connection_limit,
                        new.connection_limit
                    );
                    current.connection_limit = new.connection_limit;
                }
                if new.listen_port != current.listen_port || listener.is_none() {
                    log::info!(
                        "[TCP] phase=config status=listen_port_changed old={} new={}",
                        current.listen_port,
                        new.listen_port
                    );
                    // Bind the new port before dropping the old listener, so a failed
                    // bind leaves the server reachable where it was.
                    if let Some(new_listener) = bind(new.listen_port, current.connection_limit).await {
                        listener = Some(new_listener);
                        current.listen_port = new.listen_port;
                    }
                }
                continue;
            }
            accepted = accept(&listener) => accepted,
        };

        let (stream, peer_addr) = match accepted {
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
        let over_soft_limit = count > current.connection_limit as usize;
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

/// Sends a `ConnResponse`; `retry_after` is only meaningful with `TryAgainLater`.
/// Returns the retry delay as sent, in whole seconds, for logging.
async fn send_conn_response(
    stream: &mut WriteHalf<'_>,
    status: ConnResponseStatus,
    retry_after: Option<std::time::Duration>,
) -> Option<u32> {
    let retry_after_seconds = retry_after.map(|wait| u32::try_from(wait.as_secs()).unwrap_or(u32::MAX));
    let reply_bytes: Vec<u8> = Message::ConnResponse(crate::device_protocol::ConnResponseMessage {
        status,
        retry_after_seconds,
    })
    .into();
    let _ = stream.write_all(&reply_bytes).await;
    retry_after_seconds
}

async fn send_abort_message(stream: &mut WriteHalf<'_>, reason: AbortReason) {
    let abort_message = AbortMessage { reason };
    let abort_bytes: Vec<u8> = Message::Abort(abort_message).into();
    if let Err(e) = stream.write_all(&abort_bytes).await {
        log::warn!(
            "[TCP] peer={} action=send_abort err={:?}",
            stream.peer_addr().unwrap(),
            e
        );
    }
}

/// Outcome of whichever card-related background wait is currently pending — see
/// `handle_connection`'s `pending` variable.
enum CardEvent {
    /// The device's wait for a first card resolved.
    Acquired(crate::card_pool::CardGuard),
    /// A priority replacement for a revoked card arrived.
    Replaced(crate::smart_card::ManagedCard),
    /// The currently-held card was pulled.
    Revoked,
    /// The device's company has no cards left, so there's nothing to wait for.
    NoCards,
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

/// Whether downloads that failed (CRC mismatch) or never finished are still written to
/// disk, tagged, for debugging (`download.retain_failed_downloads` in config.yaml). Off
/// by default: such a download is then only logged and recorded in the database. Read
/// on every download, so a change from the UI applies to the next one.
fn retain_failed_downloads() -> bool {
    get_from_cache(CacheSection::Download, "retain_failed_downloads") == "true"
}

/// Per-connection handler.
///
/// `Ready`/`Abort(CardRemoved)` are server-initiated: the device announces itself
/// once (`ConnAnnounce`) and then just waits — indefinitely — for the server to send
/// `Ready` when a card is actually available. It never asks for one itself.
/// `ConnAnnounce` never rejects (the only admission gate is `connection_limit` at
/// accept time, above) — a device that reaches this handler always eventually gets a
/// card, however long the wait, while continuing to read whatever else it sends
/// meanwhile (Ping, Abort, ...).
async fn handle_connection(mut stream: TcpStream, peer_addr: SocketAddr, over_soft_limit: bool) {
    if over_soft_limit {
        let retry_after = crate::admission::over_connection_limit_retry_delay(MIN_CONNECTION_LIMIT_RETRY);
        let retry_after_seconds = u32::try_from(retry_after.as_secs()).unwrap_or(u32::MAX);
        let busyness = crate::admission::busyness(None);
        log::warn!(
            "[TCP] phase=connect status=over_soft_limit peer={} action=reject retry_after_seconds={} card_available_in_seconds={:?} cards={} waiting={} average_session_ms={} expected_returns={}",
            peer_addr,
            retry_after_seconds,
            busyness.card_available_in.map(|wait| wait.as_secs()),
            busyness.queue.cards,
            busyness.queue.waiting,
            busyness.queue.average_session.as_millis(),
            busyness.expected_returns
        );
        // The device hasn't sent anything yet at this point (this is a raw
        // connection-count guard, checked at accept time), but ConnResponse is a
        // self-contained status message, not strictly a reply, so sending it
        // unprompted here is fine and gives the device something to back off on
        // instead of just watching its connection die with no explanation.
        let reply = Message::ConnResponse(crate::device_protocol::ConnResponseMessage {
            status: crate::device_protocol::ConnResponseStatus::TryAgainLater,
            retry_after_seconds: Some(retry_after_seconds),
        });
        let reply_bytes: Vec<u8> = reply.into();
        let _ = stream.write_all(&reply_bytes).await;
        return;
    }

    let (read_half, mut write_half) = stream.split();
    let mut message_reader = crate::device_protocol::message::MessageReader::new(read_half);

    let mut announced = false;
    // Retained (not just logged inline) so the `AuthOutcome` handler can pass
    // it on to the telemetry log once the device reports how the session went.
    let mut imei: Option<String> = None;
    // Retained for future version-specific handling — not used for anything
    // yet beyond logging, and deliberately kept out of telemetry.rs (a
    // firmware version can narrow down a specific client, defeating that
    // data's anonymization).
    let mut device_firmware_version: Option<String> = None;
    // Set at admission: the device's company, the card pool it queues in, and the
    // connection slot it holds in that company (freed when this function returns).
    let mut company: Option<String> = None;
    let mut company_pool: Option<Arc<CardPool>> = None;
    let mut _company_slot: Option<CompanySlot> = None;
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
    // Whether we've already inspected the current card session's first TachoToCard
    // frame to detect which protocol the VU actually wants (21-byte APDU = T=0,
    // 22-byte = T=1, per the FMS-standard remote company card spec) and reconnected
    // the physical card to match if PCSC's own auto-negotiation guessed differently.
    // Reset on every fresh/replacement card, since each one gets its own Ready and
    // the VU redoes this detection against its own first frame each time.
    let mut protocol_confirmed = false;
    // VU download handoff state (see vu_download.rs). None of this touches the
    // physical card — RequestUpload/TransferData/RequestTransferExit happen
    // entirely on the device's own CAN/UDS link with the VU, so this just
    // accumulates whatever the device hands over. Reset on every DownloadStart.
    let mut download_buffer: Vec<u8> = Vec::new();
    let mut download_meta: Option<(u8, u8)> = None; // (generation, data_types bitmask)
    const MAX_DOWNLOAD_SIZE: usize = 16 * 1024 * 1024;

    loop {
        // (Re)arm `pending` if nothing is currently in flight for the connection's
        // present state. This only runs right after a transition (ConnAnnounce
        // admitted, card acquired, card replaced, card revoked) since `pending` is
        // only ever reset to `None` at those points — NOT every loop tick. Polling
        // the exact same future across iterations (rather than constructing a fresh
        // one each time) matters here: `acquire`'s semaphore wait would otherwise
        // silently lose the device's place in line on every single poll.
        let has_card = card.as_ref().map(|g| g.has_card()).unwrap_or(false);

        if pending.is_none() {
            pending = if awaiting_first_card {
                let pool = Arc::clone(company_pool.as_ref().expect("an admitted device has a company pool"));
                Some(Box::pin(async move {
                    tokio::select! {
                        guard = pool.acquire() => CardEvent::Acquired(guard),
                        _ = pool.wait_until_no_cards() => CardEvent::NoCards,
                    }
                }))
            } else if let Some(guard) = &card {
                if has_card {
                    let (revoked, notify) = guard.revoked_signal();
                    Some(Box::pin(async move {
                        crate::card_pool::wait_for_revocation_signal(revoked, notify).await;
                        CardEvent::Revoked
                    }))
                } else {
                    let pool = Arc::clone(company_pool.as_ref().expect("an admitted device has a company pool"));
                    Some(Box::pin(async move {
                        tokio::select! {
                            card = pool.wait_for_replacement_card() => CardEvent::Replaced(card),
                            _ = pool.wait_until_no_cards() => CardEvent::NoCards,
                        }
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
        let read_timeout = current_read_timeout(announced, has_card);
        let read_fut = tokio::time::timeout(read_timeout, message_reader.read_message());

        tokio::select! {
            result = read_fut => match result {
            Ok(Some(Ok(message))) => {
                log::debug!("[TCP] peer={} received message", peer_addr);
                match message {
                    Message::Ping(_) => {
                        let reply = Message::Pong(crate::device_protocol::PongMessage {});
                        let reply_bytes: Vec<u8> = reply.into();
                        log::debug!("[TCP] peer={} message=Ping action=reply Pong, {:?}", peer_addr, reply_bytes);
                        write_half.write_all(&reply_bytes).await.unwrap();
                    },
                    Message::Pong(_) => {
                        log::debug!("[TCP] peer={} message=Pong", peer_addr);
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
                            log::info!("[TCP] peer={} IMEI={} firmware_version={} message=ConnAnnounce action=reply status=Accepted note=duplicate", peer_addr, conn_announce_message.imei, conn_announce_message.firmware_version);
                        } else {
                            // Beyond the global `connection_limit` at accept time (see
                            // `over_soft_limit` above), admission is decided per company —
                            // see `multi-company design.md` §4. An admitted device queues for
                            // one of its company's cards, however long the wait.
                            let device_imei = conn_announce_message.imei.clone();
                            let firmware_version = conn_announce_message.firmware_version.clone();
                            let company_config = crate::company_directory::directory()
                                .company_for_imei(&device_imei)
                                .and_then(|id| crate::config::get_company(&id).map(|config| (id, config)));
                            let Some((company_id, company_config)) = company_config else {
                                send_conn_response(&mut write_half, ConnResponseStatus::Rejected, None).await;
                                // Listed in the UI (Devices page) so it can be assigned.
                                crate::database::record_unknown_device(&device_imei, &firmware_version).await;
                                log::warn!(
                                    "[TCP] peer={} IMEI={} firmware_version={} message=ConnAnnounce action=reply status=Rejected reason=unregistered_imei",
                                    peer_addr, device_imei, firmware_version
                                );
                                break;
                            };

                            let download_allowed_at = crate::database::next_download_allowed_at(
                                &company_id,
                                &device_imei,
                                company_config.download_interval_days,
                            ).await;
                            if let Some(retry_after) = crate::admission::download_interval_retry_delay(download_allowed_at) {
                                let retry_after_seconds = send_conn_response(&mut write_half, ConnResponseStatus::TryAgainLater, Some(retry_after)).await;
                                log::info!(
                                    "[TCP] peer={} company={} IMEI={} firmware_version={} message=ConnAnnounce action=reply status=TryAgainLater reason=download_interval_not_elapsed retry_after_seconds={:?}",
                                    peer_addr, company_id, device_imei, firmware_version, retry_after_seconds
                                );
                                break;
                            }

                            let pool = crate::card_pool::CARD_POOLS.pool(&company_id);
                            if pool.card_count() == 0 {
                                let retry_after_seconds = send_conn_response(&mut write_half, ConnResponseStatus::TryAgainLater, Some(crate::admission::NO_CARDS_RETRY)).await;
                                log::info!(
                                    "[TCP] peer={} company={} IMEI={} firmware_version={} message=ConnAnnounce action=reply status=TryAgainLater reason=no_cards retry_after_seconds={:?}",
                                    peer_addr, company_id, device_imei, firmware_version, retry_after_seconds
                                );
                                break;
                            }

                            let Some(slot) = CompanySlot::try_claim(&company_id, company_config.connection_limit) else {
                                let retry_after = crate::admission::over_company_limit_retry_delay(&company_id, MIN_CONNECTION_LIMIT_RETRY);
                                let retry_after_seconds = send_conn_response(&mut write_half, ConnResponseStatus::TryAgainLater, Some(retry_after)).await;
                                log::warn!(
                                    "[TCP] peer={} company={} IMEI={} firmware_version={} message=ConnAnnounce action=reply status=TryAgainLater reason=company_connection_limit limit={} retry_after_seconds={:?}",
                                    peer_addr, company_id, device_imei, firmware_version, company_config.connection_limit, retry_after_seconds
                                );
                                break;
                            };

                            announced = true;
                            awaiting_first_card = true;
                            imei = Some(device_imei.clone());
                            device_firmware_version = Some(firmware_version.clone());
                            company = Some(company_id.clone());
                            company_pool = Some(pool);
                            _company_slot = Some(slot);
                            pending = None; // arm as "Acquired" wait next loop
                            send_conn_response(&mut write_half, ConnResponseStatus::Accepted, None).await;
                            let snapshot = crate::admission::snapshot(Some(&company_id));
                            log::info!(
                                "[TCP] peer={} company={} IMEI={} firmware_version={} message=ConnAnnounce action=reply status=Accepted estimated_wait_seconds={:?} cards={} waiting={} average_session_ms={}",
                                peer_addr, company_id, device_imei, firmware_version,
                                snapshot.estimated_wait().map(|wait| wait.as_secs()), snapshot.cards, snapshot.waiting, snapshot.average_session.as_millis()
                            );
                        }
                    },
                    Message::ConnResponse(conn_response_message) => {
                        self::send_abort_message(&mut write_half, AbortReason::ProtocolViolation).await;
                        log::warn!("[TCP] peer={} message=ConnResponse action=unexpected status={:?}", peer_addr, conn_response_message.status);
                    },
                    Message::Ready(_) => {
                        // Ready is server -> device only; a device sending it is a protocol error.
                        self::send_abort_message(&mut write_half, AbortReason::ProtocolViolation).await;
                        log::warn!("[TCP] peer={} message=Ready action=abort reason=unexpected_direction", peer_addr);
                    },
                    Message::TachoToCard(tacho_to_card_message) => {
                        let has_card = card.as_ref().map(|g| g.has_card()).unwrap_or(false);
                        if !has_card {
                            // Device must have received Ready before relaying APDUs.
                            self::send_abort_message(&mut write_half, AbortReason::ProtocolViolation).await;
                            log::warn!("[TCP] peer={} message=TachoToCard action=abort reason=no_card_assigned", peer_addr);
                        } else {
                            let guard = card.as_ref().unwrap();

                            // Gen1-only fallback: on the older 3DES scheme, the VU's
                            // Internal Authenticate APDU length reveals its protocol choice
                            // (21 bytes = T=0, 22 = T=1) independently of the ATR. Confirmed
                            // on real hardware (see TACHOGRAPH_RDL_HANDOFF.md) that this
                            // does NOT apply to Gen2 ECC-based auth — GENERAL AUTHENTICATE
                            // never varies in length by protocol — so `wanted` will just
                            // stay `None` forever on that card generation and this block is
                            // a no-op; `ManagedCard::new`'s ATR-declared-default connect is
                            // the real defense there. Left in for the Gen1 case, where this
                            // is a genuinely faster, guess-free option. Keep sniffing every
                            // frame's length (not just frame #1 — housekeeping commands like
                            // SELECT commonly come first) until one actually matches.
                            if !protocol_confirmed {
                                let len = tacho_to_card_message.data.len();
                                let wanted = match len {
                                    21 => Some(pcsc::Protocols::T0),
                                    22 => Some(pcsc::Protocols::T1),
                                    _ => None,
                                };
                                log::debug!(
                                    "[TCP] peer={} action=protocol_check apdu_len={} current_protocol={:?} inferred_protocol={:?}",
                                    peer_addr, len, guard.protocol(), wanted
                                );
                                if let Some(wanted) = wanted {
                                    protocol_confirmed = true;
                                    if guard.protocol() != wanted {
                                        log::info!(
                                            "[TCP] peer={} action=protocol_switch from={:?} to={:?}",
                                            peer_addr, guard.protocol(), wanted
                                        );
                                        match guard.reconnect_to_protocol(wanted).await {
                                            Ok(()) => log::info!("[TCP] peer={} action=protocol_switch status=ok now={:?}", peer_addr, guard.protocol()),
                                            Err(e) => log::error!("[TCP] peer={} action=protocol_switch status=failed err={}", peer_addr, e),
                                        }
                                    }
                                }
                            }

                            let apdu_hex = hex::encode(&tacho_to_card_message.data);
                            log::debug!("[TCP] peer={} action=tacho_to_card apdu_hex={}", peer_addr, apdu_hex);
                            match guard.apdu_transmit(&apdu_hex).await {
                                Ok(response_hex) => {
                                    log::debug!("[TCP] peer={} action=card_to_tacho apdu_hex={}", peer_addr, response_hex);
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
                        log::info!("[TCP] peer={} message=Abort action=close reason={}", peer_addr, abort_message.reason.to_string());
                        break;
                    },
                    Message::AuthOutcome(auth_outcome_message) => {
                        let has_card = card.as_ref().map(|g| g.has_card()).unwrap_or(false);
                        if !has_card {
                            self::send_abort_message(&mut write_half, AbortReason::ProtocolViolation).await;
                            log::warn!("[TCP] peer={} message=AuthOutcome action=abort reason=no_card_assigned", peer_addr);
                        } else {
                            let guard = card.as_ref().unwrap();
                            log::info!("[TCP] peer={} action=auth_outcome success={}", peer_addr, auth_outcome_message.success);
                            if let Some(imei) = imei.as_deref() {
                                crate::database::record_device_auth(imei, auth_outcome_message.success).await;
                            }

                            // Cache-only, same transparency invariant as the sniffer hook
                            // above: never touch the card ourselves to find this out.
                            if let Some(iccid) = guard.iccid.get() {
                                let card_number = crate::config::get_from_cache(crate::config::CacheSection::Cards, iccid);
                                crate::config::record_auth_result_async(&card_number, auth_outcome_message.success).await;
                            }

                            // The download phase (if any) needs zero card interaction —
                            // RequestUpload/TransferData/RequestTransferExit happen
                            // entirely on the device's own CAN/UDS link with the VU — so
                            // release the card now rather than holding it for the whole
                            // download. `awaiting_first_card` stays false, so the loop's
                            // `pending`-arming logic naturally arms nothing further (no
                            // card-wait), and `has_card` reads false from here on.
                            card = None;
                        }
                    },
                    Message::DownloadStart(download_start_message) => {
                        if !announced {
                            self::send_abort_message(&mut write_half, AbortReason::ProtocolViolation).await;
                            log::warn!("[TCP] peer={} message=DownloadStart action=abort reason=not_announced", peer_addr);
                        } else {
                            download_buffer.clear();
                            download_meta = Some((download_start_message.generation, download_start_message.data_types));
                            log::info!(
                                "[TCP] peer={} action=download_start generation={} data_types={:#04x}",
                                peer_addr, download_start_message.generation, download_start_message.data_types
                            );
                            crate::download_activity::started(
                                &peer_addr.to_string(),
                                imei.as_deref().unwrap_or_default(),
                                company.as_deref().unwrap_or(DEFAULT_COMPANY_ID),
                                download_start_message.generation,
                            );
                        }
                    },
                    Message::DownloadChunk(download_chunk_message) => {
                        if !announced || download_meta.is_none() {
                            self::send_abort_message(&mut write_half, AbortReason::ProtocolViolation).await;
                            log::warn!("[TCP] peer={} message=DownloadChunk action=abort reason=no_download_started", peer_addr);
                        } else if download_buffer.len() + download_chunk_message.data.len() > MAX_DOWNLOAD_SIZE {
                            self::send_abort_message(&mut write_half, AbortReason::DownloadTooLarge).await;
                            log::warn!("[TCP] peer={} message=DownloadChunk action=abort reason=download_too_large", peer_addr);
                        } else {
                            log::debug!("[TCP] peer={} action=download_chunk len={} total={}", peer_addr, download_chunk_message.data.len(), download_buffer.len() + download_chunk_message.data.len());
                            download_buffer.extend_from_slice(&download_chunk_message.data);
                            crate::download_activity::progress(&peer_addr.to_string(), download_buffer.len());
                        }
                    },
                    Message::DownloadComplete(download_complete_message) => {
                        if !announced || download_meta.is_none() {
                            self::send_abort_message(&mut write_half, AbortReason::ProtocolViolation).await;
                            log::warn!("[TCP] peer={} message=DownloadComplete action=abort reason=no_download_started", peer_addr);
                        } else {
                            let (generation, data_types) = download_meta.unwrap();
                            crate::download_activity::finished(&peer_addr.to_string());
                            let actual_crc32 = crate::vu_download::compute_crc32(&download_buffer);
                            let matches = actual_crc32 == download_complete_message.crc32;
                            log::info!(
                                "[TCP] peer={} action=download_complete bytes={} expected_crc32={:08x} actual_crc32={:08x} matches={}",
                                peer_addr, download_buffer.len(), download_complete_message.crc32, actual_crc32, matches
                            );

                            // Structural check: every declared block must be exactly
                            // its declared size, with nothing left over and nothing
                            // overrunning. Diagnostic only — it never fails a download.
                            // The CRC is the sole pass/fail: a match proves the bytes
                            // arrived exactly as the device sent them, whereas a
                            // structure failure only means this parser doesn't fully
                            // understand the file. Only meaningful for generation 2 —
                            // Gen1's fields are fixed-offset, not self-describing, so
                            // there's nothing in the byte stream to check sizes against.
                            let structure_issue = if generation == 2 {
                                crate::vu_download::validate_gen2_structure(&download_buffer).err()
                            } else {
                                None
                            };
                            if let Some(offset) = structure_issue {
                                log::warn!("[TCP] peer={} action=download_unparsed offset={}", peer_addr, offset);
                            }

                            // A CRC-matched download is always saved. A failed one is
                            // saved, tagged in the filename for debugging, only if
                            // `retain_failed_downloads` is on; either way it's recorded in
                            // the database. Only a fully clean result (CRC match and,
                            // where checkable, a structure this parser understands) gets
                            // the plain untagged filename.
                            let vrn = crate::vu_download::extract_vrn(generation, &download_buffer);
                            let imei_value = imei.clone().unwrap_or_default();
                            let verified = matches;
                            let mut tag_parts: Vec<String> = Vec::new();
                            if !matches {
                                tag_parts.push("crc_mismatch".to_string());
                            }
                            if let Some(offset) = structure_issue {
                                tag_parts.push(format!("unparsed_at_{}", offset));
                            }
                            let tag = if tag_parts.is_empty() { None } else { Some(tag_parts.join("_")) };
                            let saved_path = if !verified && !retain_failed_downloads() {
                                log::warn!("[TCP] peer={} action=download_discarded reason=crc_mismatch bytes={} vrn={:?} note=retain_failed_downloads_off", peer_addr, download_buffer.len(), vrn);
                                None
                            } else {
                                match crate::vu_download::save_download(company.as_deref().unwrap_or(DEFAULT_COMPANY_ID), vrn.as_deref(), &imei_value, &download_buffer, tag.as_deref()) {
                                    Ok(path) => {
                                        log::info!("[TCP] peer={} action=download_saved path={:?} vrn={:?} verified={}", peer_addr, path, vrn, verified);
                                        Some(path)
                                    }
                                    Err(e) => {
                                        log::error!("[TCP] peer={} action=download_save_failed err={}", peer_addr, e);
                                        None
                                    }
                                }
                            };
                            // Only a download that's both verified and actually on disk
                            // counts towards the once-a-day limit.
                            let success = verified && saved_path.is_some();
                            crate::database::record_download(crate::database::DownloadRecord {
                                company: company.clone().unwrap_or_else(|| DEFAULT_COMPANY_ID.to_string()),
                                imei: imei_value,
                                vrn: vrn.clone(),
                                verified: success,
                                generation: Some(generation),
                                data_types: Some(data_types),
                                bytes: download_buffer.len(),
                                file_path: saved_path,
                            }).await;

                            let reply = Message::DownloadAck(crate::device_protocol::DownloadAckMessage { success });
                            let reply_bytes: Vec<u8> = reply.into();
                            let _ = write_half.write_all(&reply_bytes).await;

                            download_buffer.clear();
                            download_meta = None;
                        }
                    },
                    Message::DownloadAck(_download_ack_message) => {
                        // Server -> device only; a device sending it is a protocol error.
                        self::send_abort_message(&mut write_half, AbortReason::ProtocolViolation).await;
                        log::warn!("[TCP] peer={} message=DownloadAck action=abort reason=unexpected_direction", peer_addr);
                    },
                }
            }
            Ok(Some(Err(e))) => {
                log::warn!("[TCP] peer={} read error: {:?}", peer_addr, e.message);
                break;
            }
            Ok(None) => {
                log::info!("[TCP] peer={} closed the connection", peer_addr);
                break;
            }
            Err(_) => {
                log::warn!(
                    "[TCP] peer={} read timeout after {:?} (announced={} has_card={})",
                    peer_addr, read_timeout, announced, has_card
                );
                break;
            }
            },
            event = poll_pending(&mut pending) => {
                pending = None; // consumed; re-armed for the new state at the top of the next iteration
                match event {
                    CardEvent::Acquired(guard) => {
                        let atr = guard.atr().to_vec();
                        card = Some(guard);
                        awaiting_first_card = false;
                        protocol_confirmed = false;
                        let reply_bytes: Vec<u8> = Message::Ready(crate::device_protocol::ReadyMessage { atr }).into();
                        if write_half.write_all(&reply_bytes).await.is_ok() {
                            log::info!("[TCP] peer={} action=card_assigned status=ready_sent payload_hex={}", peer_addr, hex::encode(&reply_bytes));
                        }
                    }
                    CardEvent::Replaced(new_card) => {
                        if let Some(guard) = card.as_mut() {
                            guard.install_replacement(new_card);
                        }
                        protocol_confirmed = false;
                        let atr = card.as_ref().map(|g| g.atr().to_vec()).unwrap_or_default();
                        let reply_bytes: Vec<u8> = Message::Ready(crate::device_protocol::ReadyMessage { atr }).into();
                        if write_half.write_all(&reply_bytes).await.is_ok() {
                            log::info!("[TCP] peer={} action=card_reassigned status=ready_sent payload_hex={}", peer_addr, hex::encode(&reply_bytes));
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
                        log::warn!("[TCP] peer={} action=card_removed status=notified", peer_addr);
                    }
                    CardEvent::NoCards => {
                        // The company's last card has gone: nothing to wait for. Tell the
                        // device when to try again, as at announce.
                        let retry_after_seconds = send_conn_response(&mut write_half, ConnResponseStatus::TryAgainLater, Some(crate::admission::NO_CARDS_RETRY)).await;
                        log::warn!(
                            "[TCP] peer={} company={:?} action=reply status=TryAgainLater reason=no_cards_left retry_after_seconds={:?}",
                            peer_addr, company, retry_after_seconds
                        );
                        break;
                    }
                }
            }
        }
    }

    // No longer downloading, however the connection ended.
    crate::download_activity::finished(&peer_addr.to_string());

    // A download that was in progress but never reached DownloadComplete (e.g. the
    // device disconnected mid-transfer, as seen in testing) would otherwise just
    // vanish with the rest of this function's local state. Best-effort save it
    // anyway, clearly tagged as incomplete/unverified (no CRC was ever provided to
    // check it against) — for debugging visibility into what actually arrived
    // rather than losing it silently.
    if !download_buffer.is_empty() {
        let generation = download_meta.map(|(generation, _)| generation).unwrap_or(0);
        let vrn = crate::vu_download::extract_vrn(generation, &download_buffer);
        let imei_value = imei.clone().unwrap_or_default();
        // Structural offset (if any) is genuinely useful here even though the
        // transfer was already known-incomplete — it shows exactly how far the
        // well-formed data went before truncation, rather than just "somewhere".
        let tag = if generation == 2 {
            match crate::vu_download::validate_gen2_structure(&download_buffer) {
                Ok(_) => "incomplete".to_string(),
                Err(offset) => format!("incomplete_unparsed_at_{}", offset),
            }
        } else {
            "incomplete".to_string()
        };
        let saved_path = if !retain_failed_downloads() {
            log::warn!("[TCP] peer={} action=download_discarded reason={} bytes={} note=retain_failed_downloads_off", peer_addr, tag, download_buffer.len());
            None
        } else {
            match crate::vu_download::save_download(company.as_deref().unwrap_or(DEFAULT_COMPANY_ID), vrn.as_deref(), &imei_value, &download_buffer, Some(&tag)) {
                Ok(path) => {
                    log::warn!("[TCP] peer={} action=download_saved_incomplete path={:?} bytes={}", peer_addr, path, download_buffer.len());
                    Some(path)
                }
                Err(e) => {
                    log::error!("[TCP] peer={} action=download_save_failed err={}", peer_addr, e);
                    None
                }
            }
        };
        // Recorded for visibility only — never verified, so it doesn't count
        // towards the once-a-day limit.
        crate::database::record_download(crate::database::DownloadRecord {
            company: company.clone().unwrap_or_else(|| DEFAULT_COMPANY_ID.to_string()),
            imei: imei_value,
            vrn,
            verified: false,
            generation: download_meta.map(|(generation, _)| generation),
            data_types: download_meta.map(|(_, data_types)| data_types),
            bytes: download_buffer.len(),
            file_path: saved_path,
        }).await;
    }
}
