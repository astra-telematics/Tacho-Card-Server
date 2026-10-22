// ───── Std Lib ─────
use std::error::Error as StdError;
use std::ffi::CStr;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

// ───── Crates ─────
use log::{debug, error, info, warn};
use once_cell::sync::OnceCell;
use tokio::time::Duration;

use tokio::sync::Mutex;

// ───── PCSC ─────
use pcsc::*;
use pcsc::{Card, Protocols, State as PcscState};

// ───── Local Modules ─────
use crate::config::{get_from_cache, CacheSection};
use crate::global_app_handle::card_emit_event;

// ───── Constants ─────
const MAX_BUFFER_SIZE: usize = 260; // Buffer size for smart card communication.
const READERS_BUFFER_SIZE: usize = 2048;
const MANUAL_SYNC_TIMEOUT_SECS: u64 = 1;
const SW_TECHNICAL_PROBLEM: &str = "6F00";

type DynError = Box<dyn StdError + Send + Sync>;
type DynResult<T> = Result<T, DynError>;


/// Represents errors that can occur while interacting with smart card readers.
#[derive(Debug)] // Enables use of `{:?}` for logging and debugging
pub enum SmartCardError {
    /// Error indicating that the specified reader is no longer available or not recognized.
    UnknownReader,

    /// A catch-all for other types of errors, represented as a string message.
    Other(String),
}

impl std::fmt::Display for SmartCardError {
    /// Provides a user-friendly string representation of the error.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SmartCardError::UnknownReader => write!(f, "UnknownReader"),
            SmartCardError::Other(s) => write!(f, "Other: {}", s),
        }
    }
}

impl std::error::Error for SmartCardError {
    // This enables interoperability with other error-handling APIs,
    // such as `?` operator, logging, and integration with `anyhow` or `thiserror`.
}

impl From<pcsc::Error> for SmartCardError {
    /// Converts a `pcsc::Error` into a `SmartCardError`.
    ///
    /// Attempts to classify `UnknownReader` errors specifically,
    /// all other errors are wrapped in `SmartCardError::Other`.
    fn from(err: pcsc::Error) -> Self {
        if err.to_string().contains("UnknownReader") {
            SmartCardError::UnknownReader
        } else {
            SmartCardError::Other(err.to_string())
        }
    }
}

fn setup_reader_states(
    ctx: &Context,
    readers_buf: &mut [u8],
    reader_states: &mut Vec<ReaderState>,
) -> DynResult<()> {
    // Remove dead readers.
    fn is_dead(rs: &ReaderState) -> bool {
        rs.event_state().intersects(PcscState::UNKNOWN | PcscState::IGNORE)
    }

    for rs in reader_states.iter() {
        if is_dead(rs) {
            log::debug!("Removing {:?}", rs.name());
            // The reader itself is gone (e.g. unplugged), not just its card —
            // make sure CARD_POOLS doesn't keep believing a card from this reader
            // is still available/checked-out once the reader can't be monitored
            // any more.
            crate::card_pool::CARD_POOLS.remove_by_reader(rs.name());
        }
    }

    reader_states.retain(|rs| !is_dead(rs));
    // Add new readers.

    let names = match ctx.list_readers(readers_buf) {
        Ok(names) => names,
        Err(e) => {
            log::error!("Failed to list readers: {:?}", e);
            return Err(Box::new(e)); // Return the error
        }
    };

    for name in names {
        if !reader_states.iter().any(|rs| rs.name() == name) {
            log::debug!("Reader {:?} has been connected to the computer", name);
            reader_states.push(ReaderState::new(name, PcscState::UNAWARE));
        }
    }

    // Update the view of the state to wait on.
    for rs in reader_states.iter_mut() {
        rs.sync_current_state();
    }

    Ok(())
}

/// ICCID of the card in each reader, by reader name, as last read when it was inserted.
/// Lets a resync (`resend_known`) report cards that are already loaded.
static READER_ICCIDS: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashMap<String, String>>> =
    once_cell::sync::Lazy::new(Default::default);

fn reader_iccids() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, String>> {
    READER_ICCIDS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Registers inserted cards, drops removed ones, and tells the frontend
/// (`global-cards-sync`). With `resend_known`, readers whose card is already loaded are
/// reported too — for a page that has just (re)loaded and missed the original events.
async fn process_reader_states(reader_states: &mut [ReaderState], resend_known: bool) -> Result<(), SmartCardError> {
    for rs in reader_states {
        if rs.name() == PNP_NOTIFICATION() {
            continue;
        }

        if is_virtual_reader(rs.name()) {
            log::warn!("Virtual reader {:?} detected. Skipping...", rs.name());
            continue;
        }

        let reader_name = rs.name();
        let Ok(reader_name_string) = reader_name.to_str() else {
            log::warn!("Reader name is not valid UTF-8: {:?}. Skipping...", reader_name);
            continue;
        };

        let atr_bytes = rs.atr();
        let atr = hex::encode(atr_bytes);

        let card_state_string = format!("{:?}", rs.event_state());
        log::debug!("card_state_string {}", card_state_string);

        let mut card_number = String::new();
        let mut iccid = String::new();

        let action = should_register_new_card(reader_name_string, &atr).await;

        match action {
            CardProcessingResult::Create => match ManagedCard::new(reader_name, atr_bytes) {
                Ok(managed_card) => match managed_card.get_iccid().await {
                    Ok(received_iccid) => {
                        log::info!("ICCID: {}", received_iccid);

                        iccid = received_iccid;
                        card_number = get_from_cache(CacheSection::Cards, &iccid);
                        reader_iccids().insert(reader_name_string.to_string(), iccid.clone());

                        let company = crate::config::company_for_card(&card_number);
                        log::info!(
                            "Card {} (reader {:?}) detected — company {:?}",
                            card_number,
                            rs.name(),
                            company
                        );
                        crate::card_pool::CARD_POOLS.add_card(company.as_deref(), managed_card);
                    }
                    Err(e) => {
                        log::error!("Failed to get ICCID: {}", e);
                    }
                },
                Err(e) => {
                    log::error!(
                        "Failed to create ManagedCard for reader {}: {}",
                        reader_name_string,
                        e
                    );
                }
            },
            CardProcessingResult::Delete => {
                log::debug!("CARD DELETED {}", card_state_string);
                reader_iccids().remove(reader_name_string);
                // Drops the card if it's idle in the pool, or flags + wakes the
                // holding session immediately if it was checked out — see
                // CardGuard::revoked/replace_dead_card in card_pool.rs.
                crate::card_pool::CARD_POOLS.remove_by_reader(rs.name());
            }
            CardProcessingResult::Ignore => {
                if resend_known {
                    if let Some(known) = reader_iccids().get(reader_name_string).cloned() {
                        // Looked up again: the card may have been linked since.
                        card_number = get_from_cache(CacheSection::Cards, &known);
                        iccid = known;
                    }
                }
            }
        }

        if action != CardProcessingResult::Ignore || resend_known {
            card_emit_event(
                "global-cards-sync",
                iccid.into(),
                reader_name_string.into(),
                card_state_string.into(),
                card_number.clone().into(),
                None,
                None,
            );

            log::info!(
                "{:?} {:?} {:?}, {:?}",
                rs.name(),
                rs.event_state(),
                atr,
                card_number,
            );
        }
    }

    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum CardProcessingResult {
    Create,
    Delete,
    Ignore,
}

/// Determines what action should be taken for a card with the given reader name and ATR.
pub async fn should_register_new_card(reader_name: &str, atr: &str) -> CardProcessingResult {
    log::debug!("should_register_new_card");

    // Case 1: Both reader_name and atr are provided and not already known to
    // any card pool (idle or checked out) or held as unassigned → register new card.
    if !reader_name.is_empty() && !atr.is_empty() {
        if !crate::card_pool::CARD_POOLS.contains_reader(reader_name) {
            return CardProcessingResult::Create;
        }
    }

    // Case 2: ATR is empty → the card for this reader has been removed.
    if atr.is_empty() {
        return CardProcessingResult::Delete;
    }

    // No action needed
    CardProcessingResult::Ignore
}

/// Check if the reader is a virtual reader. This usually only applies to Windows.
fn is_virtual_reader(reader_name: &CStr) -> bool {
    // Convert the reader name to a lowercase string
    let reader_name_lower = reader_name.to_string_lossy().to_lowercase();

    // Check if the name contains keywords indicating a virtual reader
    reader_name_lower.contains("microsoft")
        || reader_name_lower.contains("virtual")
        || reader_name_lower.contains("remote")
}

// Automatically sync cards
pub async fn sc_monitor() -> ! {
    loop {
        log::debug!("Starting the outer loop to establish context...");
        let ctx = match Context::establish(Scope::User) {
            Ok(ctx) => {
                log::debug!("Successfully established context.");
                ctx
            }
            Err(e) => {
                log::error!(
                    "Failed to establish context: {:?}. Retrying in 5 seconds...",
                    e
                );
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let mut readers_buf = [0; READERS_BUFFER_SIZE];
        let mut reader_states: Vec<ReaderState> = vec![
            // Listen for reader insertions/removals, if supported.
            ReaderState::new(PNP_NOTIFICATION(), PcscState::UNAWARE),
        ];

        log::debug!("Initialized readers buffer and reader states.");

        loop {
            log::debug!("Starting the inner loop to monitor reader states...");
            if let Err(e) = setup_reader_states(&ctx, &mut readers_buf, &mut reader_states) {
                log::error!("Failed to setup_reader_states: {:?}", e);
                log::debug!("Exiting inner loop to re-establish context...");
                break; // Exit the inner loop to re-establish context
            }
            log::debug!(
                "Successfully set up reader states: {:?}",
                reader_states
                    .iter()
                    .map(|rs| rs.name().to_string_lossy())
                    .collect::<Vec<_>>()
            );

            if let Err(e) = ctx.get_status_change(None, &mut reader_states[..]) {
                log::error!("get_status_change failed: {:?}", e);
                // Small backoff prevents a tight reconnect loop if the PCSC
                // service is repeatedly returning errors (e.g. daemon down).
                tokio::time::sleep(Duration::from_secs(1)).await;
                break;
            }

            if let Err(e) =
                process_reader_states(&mut reader_states, false).await
            {
                match e {
                    SmartCardError::UnknownReader => {
                        log::warn!("Detected UnknownReader. Sleeping 3s to avoid busy loop!");
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }
                    SmartCardError::Other(msg) => {
                        log::error!("SmartCard error: {}", msg);
                        // Without a small backoff this branch could spin the
                        // outer reconnect loop very quickly under persistent
                        // PCSC failures.
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }

                break;
            }

            log::debug!("Waiting for the next status change...");
            tokio::task::yield_now().await;
        }

        log::debug!("Re-establishing context...");
    }
}

/// The protocol a terminal uses by default, absent any PPS — `TD1`'s low
/// nibble, per ISO/IEC 7816-3 (defaults to T=0 if `TD1` itself is absent).
/// Bit mapping for the presence nibble (T0/TDi's high nibble) is
/// b5=TA(i+1) (0x10), b6=TB(i+1) (0x20), b7=TC(i+1) (0x40), b8=TD(i+1)
/// (0x80) — confirmed against the ISO 7816-3 spec text, not assumed.
pub fn atr_default_protocol(atr: &[u8]) -> Protocols {
    if atr.len() < 2 {
        log::warn!("ATR too short to determine default protocol: {:?}", atr);
        return Protocols::T0;
    }

    let t0 = atr[1];
    let mut index = 2;

    if t0 & 0x10 != 0 { index += 1; } // TA1
    if t0 & 0x20 != 0 { index += 1; } // TB1
    if t0 & 0x40 != 0 { index += 1; } // TC1

    let td1 = if t0 & 0x80 != 0 && index < atr.len() {
        Some(atr[index])
    } else {
        None
    };

    match td1 {
        None => Protocols::T0, // No TD1 → protocol defaults to T=0.
        Some(td1) => match td1 & 0x0F {
            0x00 => Protocols::T0,
            0x01 => Protocols::T1,
            other => {
                log::warn!("Unexpected default protocol T={} in TD1; defaulting to T0", other);
                Protocols::T0
            }
        },
    }
}

/// Every protocol the ATR declares as available — walks the *full* TDi
/// chain (unlike `atr_default_protocol`, which stops at `TD1`), OR-ing in
/// each `TDi & 0x0F` encountered. A card can declare more than one (e.g.
/// `TD1` offering T=0 and `TD2` additionally offering T=1), which is exactly
/// the case that made PCSC's own auto-negotiation land on the wrong protocol
/// for a real company card this session — this is the telemetry field that
/// lets that be told apart from a single-protocol card in the field.
pub fn atr_supported_protocols(atr: &[u8]) -> Protocols {
    if atr.len() < 2 {
        log::warn!("ATR too short to determine supported protocols: {:?}", atr);
        return Protocols::T0;
    }

    let mut supported = Protocols::empty();
    let mut current = atr[1]; // T0
    let mut index = 2;
    let mut saw_td = false;

    loop {
        if current & 0x10 != 0 { index += 1; } // TA(i+1)
        if current & 0x20 != 0 { index += 1; } // TB(i+1)
        if current & 0x40 != 0 { index += 1; } // TC(i+1)

        if current & 0x80 != 0 && index < atr.len() {
            let td = atr[index];
            index += 1;
            saw_td = true;
            match td & 0x0F {
                0x00 => supported |= Protocols::T0,
                0x01 => supported |= Protocols::T1,
                other => log::warn!("Unexpected protocol T={} in TDi; ignoring", other),
            }
            current = td;
        } else {
            break;
        }
    }

    if !saw_td {
        supported = Protocols::T0; // No TD1 at all → T=0 is the only protocol.
    }

    supported
}

// Manual card sync function.
// This function is used to manually sync cards from anywhere in the program.
// Manually sync cards. Clicking on the button in the frontend will trigger this function
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn manual_sync_cards(
    _readername: String,
    restart: bool,
) -> Result<(), String> {
    log::debug!("Manual sync cards function is called. Restart: {}", restart);

    if restart {
        return Ok(());
    }

    let ctx = Context::establish(Scope::User)
        .map_err(|e| format!("failed to establish context: {e}"))?;
    log::debug!("Context established successfully.");

    let mut readers_buf = [0; READERS_BUFFER_SIZE];
    match ctx.list_readers(&mut readers_buf) {
        Ok(readers) => {
            if readers.count() == 0 {
                log::warn!("No readers found. Exiting...");
                return Ok(());
            }
            log::debug!("Available readers found");
        }
        Err(e) => {
            log::error!("Failed to list readers: {:?}", e);
            return Ok(());
        }
    }

    let mut reader_states = vec![
        // Listen for reader insertions/removals, if supported.
        ReaderState::new(PNP_NOTIFICATION(), State::UNAWARE),
    ];

    // setup readers states. Getting changes and other inits
    if let Err(e) = setup_reader_states(&ctx, &mut readers_buf, &mut reader_states) {
        log::error!("Failed to setup reader states: {:?}", e);
    }
    // waiting for the status change
    ctx.get_status_change(Some(Duration::from_secs(MANUAL_SYNC_TIMEOUT_SECS)), &mut reader_states)
        .map_err(|e| format!("failed to get status change: {e}"))?;

    process_reader_states(&mut reader_states, true)
        .await
        .map_err(|e| format!("Processing failed: {}", e))?;

    Ok(())
}
/// Runs a pcsc call that is known to `panic!()` on certain real-hardware conditions
/// instead of returning a `Result` — e.g. `pcsc::Error::from_raw` panics (in debug
/// builds) on a raw SCard status code it doesn't recognize, and `Card::transmit()`
/// panics unconditionally if the card's negotiated protocol came back undefined
/// (which can happen if a card is pulled mid-connect/transmit, or a flaky
/// reader/driver reports a bogus status). Converts either kind of panic into a
/// plain `Err` so a single misbehaving card/reader can't take down the caller
/// (the sc_monitor loop, or an in-flight APDU transmit) — callers already have
/// retry/recreate logic that treats this exactly like any other pcsc error.
fn catch_pcsc_panic<T>(f: impl FnOnce() -> T) -> DynResult<T> {
    panic::catch_unwind(AssertUnwindSafe(f)).map_err(|payload| {
        let msg = if let Some(s) = payload.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "non-string panic payload".to_string()
        };
        format!("pcsc call panicked: {}", msg).into()
    })
}

//////////////////////////////////////////////////
/// CARD WRAPER //////////////////////////////////
/// //////////////////////////////////////////////
#[derive(Clone)]
pub struct ManagedCard {
    inner: Arc<Mutex<Card>>,
    reader_name: Arc<CStr>,
    /// `Protocols` bits. pcsc's `DWORD` is `u32` on Windows/macOS but `u64` on 64-bit
    /// Linux, so values are cast to/from `u32` at each use; every protocol flag fits.
    protocol: Arc<AtomicU32>,
    atr: Arc<[u8]>,
    pub iccid: OnceCell<String>,
}

impl ManagedCard {
    /// Name of the reader this card is seated in — used by `card_pool` to drop a
    /// pooled-but-idle card when its reader reports the card removed.
    pub fn reader_name(&self) -> &CStr {
        &self.reader_name
    }

    /// Raw ATR (Answer To Reset) exactly as read from the physical reader.
    /// Sent to the device in the `Ready` message — the device never touches
    /// the card directly, so this is the only way it ever sees the ATR.
    pub fn atr(&self) -> &[u8] {
        &self.atr
    }

    /// The protocol PCSC actually negotiated with the card at connect time.
    pub fn protocol(&self) -> Protocols {
        Protocols::from_bits_retain(self.protocol.load(Ordering::Relaxed) as pcsc::ffi::DWORD)
    }

    /// Connects using the protocol the ATR itself declares as the default
    /// (`TD1`'s low nibble — the protocol a terminal uses absent any PPS),
    /// rather than letting PCSC auto-negotiate (`Protocols::ANY`). Real-world
    /// testing against a Stoneridge SE5000 (see TACHOGRAPH_RDL_HANDOFF.md)
    /// found PCSC's own auto-negotiation doesn't reliably land on the ATR's
    /// declared default for a dual-protocol card — it landed on T=1 here even
    /// though this card's `TD1` declares T=0 first — and the VU expects
    /// whichever protocol matches its own state, which starting from the
    /// ATR's default is the best available guess absent a per-VU cache.
    pub fn new(reader_name: &CStr, atr: &[u8]) -> DynResult<Self> {
        debug!(
            "ManagedCard::new() called. Reader: '{}'",
            reader_name.to_string_lossy(),
        );

        let (card, protocol) = Self::create_card(reader_name, atr)?;
        debug!(
            "Card successfully created for reader: '{}' (protocol: {:?})",
            reader_name.to_string_lossy(),
            protocol
        );

        Ok(Self {
            inner: Arc::new(Mutex::new(card)),
            reader_name: Arc::from(reader_name.to_owned()),
            protocol: Arc::new(AtomicU32::new(protocol.bits() as u32)),
            atr: Arc::from(atr),
            iccid: OnceCell::new(),
        })
    }

    fn create_card(reader_name: &CStr, atr: &[u8]) -> DynResult<(Card, Protocols)> {
        let ctx = Context::establish(Scope::User)
            .map_err(|err| {
                log::error!("Failed to establish context: {}", err);
                Box::<dyn StdError + Send + Sync>::from(err)
            })?;

        let requested = atr_default_protocol(atr);

        let card = catch_pcsc_panic(|| ctx.connect(reader_name, ShareMode::Shared, requested))?
            .map_err(|err| {
                log::error!("Failed to connect to card: {}", err);
                Box::<dyn StdError + Send + Sync>::from(err)
            })?;

        let negotiated = card.status2_owned().map_err(|err| {
            log::error!("Failed to read negotiated protocol after connect: {}", err);
            Box::<dyn StdError + Send + Sync>::from(err)
        })?;

        let protocol = match negotiated.protocol2() {
            Some(Protocol::T0) => Protocols::T0,
            Some(Protocol::T1) => Protocols::T1,
            other => {
                log::warn!("Unexpected negotiated protocol {:?}; defaulting to requested {:?}", other, requested);
                requested
            }
        };

        Ok((card, protocol))
    }

    pub async fn reconnect(&self) {
        debug!(
            "Attempting to reconnect card for reader: {}",
            self.reader_name.to_string_lossy()
        );

        let requested = atr_default_protocol(&self.atr);
        let reconnect_result: Result<(), String> = {
            let mut card = self.inner.lock().await;
            match catch_pcsc_panic(move || {
                card.reconnect(ShareMode::Shared, requested, Disposition::ResetCard)
            }) {
                Ok(Ok(())) => Ok(()),
                Ok(Err(pcsc_err)) => Err(pcsc_err.to_string()),
                Err(panic_err) => Err(panic_err.to_string()),
            }
        };

        match reconnect_result {
            Ok(_) => {
                self.protocol.store(requested.bits() as u32, Ordering::Relaxed);
                debug!(
                    "Card reconnected successfully for reader: {}",
                    self.reader_name.to_string_lossy()
                );
            }
            Err(e) => {
                warn!(
                    "Failed to reconnect card: {:?} for reader: {}. Will try to recreate.",
                    e,
                    self.reader_name.to_string_lossy()
                );

                if let Err(e) = self.recreate().await {
                    error!(
                        "Failed to recreate card after reconnect failure for reader {}: {}",
                        self.reader_name.to_string_lossy(),
                        e
                    );
                }
            }
        }
    }

    /// Force the card connection onto a specific protocol, rather than letting
    /// PCSC auto-negotiate — used once the far-end VU reveals which protocol it
    /// actually wants to talk (inferred from the length of its first APDU),
    /// which can differ from whatever `Protocols::ANY` settled on at connect
    /// time for a card that declares both T=0 and T=1 in its ATR.
    pub async fn reconnect_to_protocol(&self, protocol: Protocols) -> DynResult<()> {
        let mut card = self.inner.lock().await;
        // `ResetCard` (not `LeaveCard`) — a plain reconnect without a reset isn't
        // guaranteed to make the reader actually renegotiate T=0/T=1 with the
        // card; matches the disposition `reconnect()` already uses for the same
        // reason when recovering a stale card.
        match catch_pcsc_panic(move || {
            card.reconnect(ShareMode::Shared, protocol, Disposition::ResetCard)
        })? {
            Ok(()) => {
                self.protocol.store(protocol.bits() as u32, Ordering::Relaxed);
                debug!(
                    "Reconnected reader '{}' onto protocol {:?}",
                    self.reader_name.to_string_lossy(),
                    protocol
                );
                Ok(())
            }
            Err(pcsc_err) => Err(Box::<dyn StdError + Send + Sync>::from(pcsc_err)),
        }
    }

    pub async fn recreate(&self) -> DynResult<()> {
        let (new_card, protocol) = Self::create_card(&self.reader_name, &self.atr)?;
        let mut lock = self.inner.lock().await;
        *lock = new_card;
        self.protocol.store(protocol.bits() as u32, Ordering::Relaxed);

        info!(
            "Successfully recreated card object for reader: {}",
            self.reader_name.to_string_lossy()
        );

        Ok(())
    }

    // pub async fn disconnect(&self) -> Result<(), Box<dyn StdError + Send + Sync>> {
    //     let mut guard = self.inner.lock().await;

    //     let dummy_card = mem::replace(
    //         &mut *guard,
    //         Context::establish(Scope::User)?
    //             .connect(&self.reader_name, ShareMode::Shared, self.protocol)?
    //     );

    //     #[cfg(target_os = "linux")]
    //     {
    //         log::debug!("Linux-specific disconnect logic started.");

    //         // force trigger status update
    //         let mut reader_states = vec![
    //             pcsc::ReaderState::new(self.reader_name.as_ref(), pcsc::State::UNAWARE)
    //         ];

    //         match Context::establish(Scope::User)?.get_status_change(Some(Duration::from_millis(1)), &mut reader_states) {
    //             Ok(_) => log::debug!("Status change triggered successfully for {}", self.reader_name.to_string_lossy()),
    //             Err(e) => log::warn!("get_status_change failed on Linux: {}", e),
    //         }

    //         return Ok(());
    //     }

    //     #[cfg(target_os = "macos")]
    //     {
    //         return dummy_card
    //             .disconnect(pcsc::Disposition::ResetCard)
    //             .map_err(|(_, err)| Box::new(err) as _);
    //     }

    //     #[cfg(target_os = "windows")]
    //     {
    //         return dummy_card
    //             .disconnect(pcsc::Disposition::ResetCard)
    //             .map_err(|(_, err)| Box::new(err) as _);
    //     }
    // }

    pub async fn apdu_transmit(&self, apdu_hex: &str) -> DynResult<String> {
        debug!(
            "apdu_transmit() called for reader: {} with APDU HEX: {}",
            self.reader_name.to_string_lossy(),
            apdu_hex
        );

        let apdu = match hex::decode(apdu_hex) {
            Ok(data) => {
                debug!("APDU decoded successfully: {:?}", data);
                data
            }
            Err(err) => {
                error!("Failed to decode APDU '{}': {}", apdu_hex, err);
                return Err(format!("Decode error: {}", err).into());
            }
        };

        let card = Arc::clone(&self.inner);
        let reader_name = self.reader_name.to_string_lossy().into_owned();
        // See `collect_waiting_data`.
        let fetch_waiting_data = self.protocol() == Protocols::T0 && has_le_after_data(&apdu);

        debug!(
            "Cloned card for blocking transmission. Sending to spawn_blocking..."
        );

        let response = crate::rt::spawn_blocking(move || {
            debug!("Entered spawn_blocking thread. Preparing buffer and locking card...");

            let mut rapdu_buf = [0u8; MAX_BUFFER_SIZE];

            let mut locked = card.blocking_lock();
            debug!("Lock acquired. Transmitting...");

            let first = match catch_pcsc_panic(|| locked.transmit(&apdu, &mut rapdu_buf)) {
                Ok(Ok(response)) => {
                    debug!("APDU transmit success. Encoded response: {}", hex::encode(response));
                    Ok(response.to_vec())
                }
                // The card can be reset by something entirely outside our control —
                // on Windows, most commonly the OS's own Smart Card Plug and Play
                // service probing a freshly-inserted card for a minidriver match.
                // That invalidates our handle's cached state without us having done
                // anything wrong; PCSC's own docs call this out as the transient
                // error to recover from by reconnecting (without forcing another
                // reset) and retrying the exact same transmit once.
                Ok(Err(pcsc::Error::ResetCard)) => {
                    warn!("Card for reader {} was reset externally; reconnecting and retrying transmit once", reader_name);
                    match catch_pcsc_panic(|| locked.reconnect(ShareMode::Shared, Protocols::ANY, Disposition::LeaveCard)) {
                        Ok(Ok(())) => match catch_pcsc_panic(|| locked.transmit(&apdu, &mut rapdu_buf)) {
                            Ok(Ok(response)) => {
                                debug!("APDU transmit succeeded after reset-recovery. Encoded response: {}", hex::encode(response));
                                Ok(response.to_vec())
                            }
                            Ok(Err(err)) => {
                                error!("APDU transmit failed again after reset-recovery: {}", err);
                                Err(format!("Transmit error after reset-recovery: {}", err))
                            }
                            Err(panic_err) => {
                                error!("APDU transmit panicked after reset-recovery: {}", panic_err);
                                Err(format!("Transmit panicked after reset-recovery: {}", panic_err))
                            }
                        },
                        Ok(Err(err)) => {
                            error!("Reconnect after card reset failed: {}", err);
                            Err(format!("Reconnect after reset failed: {}", err))
                        }
                        Err(panic_err) => {
                            error!("Reconnect after card reset panicked: {}", panic_err);
                            Err(format!("Reconnect panicked after reset: {}", panic_err))
                        }
                    }
                }
                Ok(Err(err)) => {
                    error!("APDU transmit failed: {}", err);
                    Err(format!("Transmit error: {}", err))
                }
                Err(panic_err) => {
                    error!("APDU transmit panicked: {}", panic_err);
                    Err(format!("Transmit panicked: {}", panic_err))
                }
            };

            let response = first?;
            if !fetch_waiting_data || waiting_bytes(&response).is_none() {
                return Ok::<_, String>(hex::encode(response));
            }
            debug!("Card has data waiting ({}); fetching it with GET RESPONSE", hex::encode(&response));
            let response = collect_waiting_data(response, |command| {
                let mut buf = [0u8; MAX_BUFFER_SIZE];
                match catch_pcsc_panic(|| locked.transmit(command, &mut buf)) {
                    Ok(Ok(response)) => {
                        debug!("GET RESPONSE {} -> {}", hex::encode(command), hex::encode(response));
                        Ok(response.to_vec())
                    }
                    Ok(Err(err)) => Err(format!("GET RESPONSE failed: {}", err)),
                    Err(panic_err) => Err(format!("GET RESPONSE panicked: {}", panic_err)),
                }
            })?;
            Ok(hex::encode(response))
        })
        .await??;

        debug!(
            "apdu_transmit() complete for reader: {}. Final response: {}",
            self.reader_name.to_string_lossy(),
            response
        );

        Ok(response)
    }

    pub async fn send_apdu(
        &self,
        apdu_hex: &str,
        client_id: &str,
    ) -> String {
        debug!("{} Sending APDU command: {}", client_id, apdu_hex);

        // First attempt
        match self.apdu_transmit(apdu_hex).await {
            Ok(response) => {
                debug!("{} APDU response: {:?}", client_id, response);
                return response;
            }
            Err(err) => {
                error!(
                    "{} Failed to send APDU: {}. Attempting to recreate card...",
                    client_id,
                    err
                );
            }
        }

        // recreate attempt
        if let Err(e) = self.recreate().await {
            error!(
                "{} Failed to recreate card after APDU failure: {}",
                client_id,
                e
            );
            return SW_TECHNICAL_PROBLEM.to_string();
        }

        // Seccond attempt
        match self.apdu_transmit(apdu_hex).await {
            Ok(response) => {
                debug!(
                    "{} APDU response (after recreate): {:?}",
                    client_id,
                    response
                );
                response
            }
            Err(retry_err) => {
                error!(
                    "{} Retry failed: could not send APDU after recreate: {}",
                    client_id,
                    retry_err
                );
                SW_TECHNICAL_PROBLEM.to_string()
            }
        }
    }

    /// Returns the card ICCID using lazy caching.
    /// On first call, reads it from the card; subsequent calls return the cached value.
    pub async fn get_iccid(&self) -> DynResult<String> {
        if let Some(cached) = self.iccid.get() {
            log::debug!(
                "Returning cached ICCID for reader {}: {}",
                self.reader_name.to_string_lossy(),
                cached
            );
            return Ok(cached.clone());
        }

        log::debug!(
            "get_iccid() started for reader: {}",
            self.reader_name.to_string_lossy()
        );

        // SELECT EF_ICC (FID 0002) under MF
        let select_result = self.apdu_transmit("00A4020C020002").await?;

        if !select_result.ends_with("9000") {
            // Do NOT proceed to READ BINARY: on SW != 9000 the current EF is
            // unchanged, so READ would return bytes from whatever was previously
            // selected (e.g. EF_Identification from a prior auth session),
            // producing a garbage ICCID that happens to be part of cardNumber.
            return Err(
                format!("SELECT EF_ICC failed with status: {}", select_result).into()
            );
        }

        // READ BINARY: 8 bytes of cardExtendedSerialNumber at offset 1 (skipping clockStop)
        let read_response = self.apdu_transmit("00B0000108").await?;

        let Some(hex_data) = read_response.strip_suffix("9000") else {
            return Err(
                format!("READ BINARY EF_ICC returned unexpected status: {}", read_response).into()
            );
        };

        let bytes = hex::decode(hex_data)
            .map_err(|e| format!("Failed to decode ICCID hex: {}", e))?;

        if bytes.len() != 8 {
            return Err(
                format!("EF_ICC READ BINARY returned {} bytes, expected 8", bytes.len()).into()
            );
        }

        let iccid = bytes.iter().map(|b| format!("{:02X}", b)).collect::<String>();

        log::debug!("Final ICCID: {}", iccid);

        // Save ICCID, not got earlier
        let _ = self.iccid.set(iccid.clone());

        Ok(iccid)
    }

}

/// Whether a short command APDU ends with Le after its data (case 4: data both ways).
/// A VU talking T=1 sends those whole. On T=0 the Le of a case 4 command isn't sent,
/// so a VU talking T=0 leaves it off.
fn has_le_after_data(apdu: &[u8]) -> bool {
    apdu.len() > 5 && apdu[4] != 0 && apdu.len() == 5 + apdu[4] as usize + 1
}

/// `Some(xx)` if the response is `61 xx`, the T=0 "xx bytes waiting" status
/// (`xx` = 00 means 256).
fn waiting_bytes(response: &[u8]) -> Option<u8> {
    match response {
        [.., 0x61, available] => Some(*available),
        _ => None,
    }
}

/// The most GET RESPONSEs to send for one command. At up to 256 bytes each, that's far
/// more than any tachograph card response.
const MAX_GET_RESPONSES: usize = 16;

/// Under T=0 a card can't return a case 4 command's data with its status: it answers
/// `61 xx` and the terminal fetches the data with GET RESPONSE (ISO/IEC 7816-4). That's
/// part of how T=0 carries an APDU, so a VU talking T=1 never expects it: a T=1 card
/// answers with the data and `90 00` in one go, and the VU fails the authentication if
/// it gets `61 xx` (seen on a real truck, on Gen2 chip authentication's GENERAL
/// AUTHENTICATE).
///
/// So when the card is on T=0 and the VU sent a case 4 command the T=1 way (with Le),
/// the waiting data is fetched here and the VU gets the whole response, as a T=1 card
/// would send it. A VU talking T=0 sends case 4 commands without Le and handles `61 xx`
/// itself, so its commands pass through untouched. `transmit` sends one APDU to the card.
fn collect_waiting_data(
    first: Vec<u8>,
    mut transmit: impl FnMut(&[u8]) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    let mut response = first;
    for _ in 0..MAX_GET_RESPONSES {
        let Some(available) = waiting_bytes(&response) else {
            data.extend_from_slice(&response);
            return Ok(data);
        };
        // Any data before `61 xx` is part of the response too.
        data.extend_from_slice(&response[..response.len() - 2]);
        response = transmit(&[0x00, 0xC0, 0x00, 0x00, available])?;
    }
    Err(format!("card still had data waiting after {} GET RESPONSEs", MAX_GET_RESPONSES))
}

#[cfg(test)]
mod get_response_tests {
    use super::*;

    #[test]
    fn le_after_data_is_only_on_case_4_commands_sent_whole() {
        // GENERAL AUTHENTICATE from the truck: 5-byte header, 0x45 bytes of data, Le.
        let mut general_authenticate = vec![0x00, 0x86, 0x00, 0x00, 0x45];
        general_authenticate.extend(std::iter::repeat(0xAA).take(0x45));
        assert!(!has_le_after_data(&general_authenticate)); // as a T=0 VU sends it
        general_authenticate.push(0x00);
        assert!(has_le_after_data(&general_authenticate)); // as a T=1 VU sends it

        assert!(!has_le_after_data(&hex::decode("00a4020c020002").unwrap())); // SELECT: case 3
        assert!(!has_le_after_data(&hex::decode("00b0000012").unwrap())); // READ BINARY: case 2
        assert!(!has_le_after_data(&hex::decode("0084000008").unwrap())); // GET CHALLENGE: case 2
    }

    #[test]
    fn waiting_data_is_fetched_and_joined() {
        let waiting: Vec<u8> = (0..0x16).collect();
        let mut sent = Vec::new();
        let response = collect_waiting_data(vec![0x61, 0x16], |command| {
            sent.push(command.to_vec());
            Ok([waiting.as_slice(), &[0x90, 0x00]].concat())
        })
        .unwrap();
        assert_eq!(sent, vec![vec![0x00, 0xC0, 0x00, 0x00, 0x16]]);
        assert_eq!(response, [waiting.as_slice(), &[0x90, 0x00]].concat());
    }

    #[test]
    fn data_waiting_in_several_parts_is_fetched_until_the_end() {
        let mut replies = vec![vec![0x01, 0x02, 0x61, 0x01], vec![0x03, 0x90, 0x00]].into_iter();
        let response = collect_waiting_data(vec![0x61, 0x02], |_| Ok(replies.next().unwrap())).unwrap();
        assert_eq!(response, vec![0x01, 0x02, 0x03, 0x90, 0x00]);
    }

    #[test]
    fn a_final_response_is_returned_as_is() {
        let response = collect_waiting_data(vec![0x69, 0x82], |_| panic!("nothing to fetch")).unwrap();
        assert_eq!(response, vec![0x69, 0x82]);
    }

    #[test]
    fn a_card_that_never_finishes_is_an_error() {
        assert!(collect_waiting_data(vec![0x61, 0x01], |_| Ok(vec![0x00, 0x61, 0x01])).is_err());
    }
}

#[cfg(test)]
mod panic_guard_tests {
    use super::catch_pcsc_panic;

    #[test]
    fn catch_pcsc_panic_converts_str_panic_to_err() {
        let result = catch_pcsc_panic(|| -> u32 {
            panic!("pcsc::Card::transmit() does not work with direct connections");
        });
        let err = result.expect_err("panic must be converted to Err, not propagated");
        assert!(err.to_string().contains("does not work with direct connections"));
    }

    #[test]
    fn catch_pcsc_panic_converts_string_panic_to_err() {
        let result = catch_pcsc_panic(|| -> u32 { panic!("{}", "unknown PCSC error code: 0x1b1") });
        let err = result.expect_err("panic must be converted to Err, not propagated");
        assert!(err.to_string().contains("0x1b1"));
    }

    #[test]
    fn catch_pcsc_panic_passes_through_ok_value() {
        let result = catch_pcsc_panic(|| 42u32);
        assert_eq!(result.unwrap(), 42);
    }
}
