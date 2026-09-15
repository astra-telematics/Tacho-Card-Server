// ───── Std Lib ─────
use std::error::Error as StdError;
use std::ffi::CStr;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;

// ───── Crates ─────
use log::{debug, error, info, warn};
use once_cell::sync::OnceCell;
use tokio::time::Duration;

use tauri::async_runtime::Mutex;

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
            // make sure CARD_POOL doesn't keep believing a card from this reader
            // is still available/checked-out once the reader can't be monitored
            // any more.
            crate::card_pool::CARD_POOL.remove_by_reader(rs.name());
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

async fn process_reader_states(reader_states: &mut [ReaderState]) -> Result<(), SmartCardError> {
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

        let atr = hex::encode(rs.atr());
        let protocol = parse_atr_and_get_protocol(&atr);

        let card_state_string = format!("{:?}", rs.event_state());
        log::debug!("card_state_string {}", card_state_string);

        let mut card_number = String::new();
        let mut iccid = String::new();

        let action = should_register_new_card(reader_name_string, &atr).await;

        match action {
            CardProcessingResult::Create => match ManagedCard::new(reader_name, protocol) {
                Ok(managed_card) => match managed_card.get_iccid().await {
                    Ok(received_iccid) => {
                        log::info!("ICCID: {}", received_iccid);

                        iccid = received_iccid;
                        card_number = get_from_cache(CacheSection::Cards, &iccid);

                        log::info!(
                            "Card {} (reader {:?}) detected — added to the card pool for devices to check out",
                            card_number,
                            rs.name()
                        );
                        crate::card_pool::CARD_POOL.add_card(managed_card);
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
                // Drops the card if it's idle in the pool, or flags + wakes the
                // holding session immediately if it was checked out — see
                // CardGuard::revoked/replace_dead_card in card_pool.rs.
                crate::card_pool::CARD_POOL.remove_by_reader(rs.name());
            }
            CardProcessingResult::Ignore => {}
        }

        if action != CardProcessingResult::Ignore {
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
                "{:?} {:?} {:?}, {:?}, Protocol: {:?}",
                rs.name(),
                rs.event_state(),
                atr,
                card_number,
                protocol
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
    // CARD_POOL (idle or checked out) → register new card.
    if !reader_name.is_empty() && !atr.is_empty() {
        if !crate::card_pool::CARD_POOL.contains_reader(reader_name) {
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
                process_reader_states(&mut reader_states).await
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

/// Parses the ATR and extracts the communication protocol (T=0 or T=1).
///
/// # Arguments
/// - `atr`: A string containing the ATR in hexadecimal format.
///
/// # Returns
/// - `String`: The communication protocol ("T0", "T1", or "Unknown").
pub fn parse_atr_and_get_protocol(atr: &str) -> Protocols {
    fn protocol_from_td(td: u8) -> Protocols {
        match td & 0x0F {
            0x00 => Protocols::T0,
            0x01 => Protocols::T1,
            _ => Protocols::T0,
        }
    }

    let atr_bytes = match hex::decode(atr) {
        Ok(bytes) => bytes,
        Err(_) => {
            log::error!("Invalid ATR format: {}", atr);
            return Protocols::T0;
        }
    };

    if atr_bytes.len() < 2 {
        log::warn!("ATR is too short: {:?}", atr_bytes);
        return Protocols::T0;
    }

    let mut index = 1;
    let y1 = atr_bytes[index] >> 4;
    index += 1;

    // Skip TA1, TB1, TC1 depends on Y1
    if y1 & 0x1 != 0 { index += 1; } // TA1
    if y1 & 0x2 != 0 { index += 1; } // TB1
    if y1 & 0x4 != 0 { index += 1; } // TC1

    // TD1
    let td1 = if y1 & 0x8 != 0 && index < atr_bytes.len() {
        let td1 = atr_bytes[index];
        index += 1;
        Some(td1)
    } else {
        None
    };

    // TD2 (if was TD1)
    let td2 = if let Some(td1) = td1 {
        let y2 = td1 >> 4;
        // Skip TA2, TB2, TC2
        if y2 & 0x1 != 0 { index += 1; } // TA2
        if y2 & 0x2 != 0 { index += 1; } // TB2
        if y2 & 0x4 != 0 { index += 1; } // TC2

        if y2 & 0x8 != 0 && index < atr_bytes.len() {
            Some(atr_bytes[index])
        } else {
            None
        }
    } else {
        None
    };

    // If TD2 exists — it is default protocol
    if let Some(td2) = td2 {
        return protocol_from_td(td2);
    }

    // If TD2 is not presented, but TD1 it is — use it
    if let Some(td1) = td1 {
        return protocol_from_td(td1);
    }

    // Default value if have no TD1 and TD2
    Protocols::T0
}

// Manual card sync function.
// This function is used to manually sync cards from anywhere in the program.
// Manually sync cards. Clicking on the button in the frontend will trigger this function
#[tauri::command]
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

    process_reader_states(&mut reader_states)
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
    protocol: Protocols,
    pub iccid: OnceCell<String>,
}

impl ManagedCard {
    /// Name of the reader this card is seated in — used by `card_pool` to drop a
    /// pooled-but-idle card when its reader reports the card removed.
    pub fn reader_name(&self) -> &CStr {
        &self.reader_name
    }

    pub fn new(reader_name: &CStr, protocol: Protocols) -> DynResult<Self> {
        debug!(
            "ManagedCard::new() called. Reader: '{}', Protocol: {:?}",
            reader_name.to_string_lossy(),
            protocol
        );

        let card = Self::create_card(reader_name, protocol)?;
        debug!(
            "Card successfully created for reader: '{}'",
            reader_name.to_string_lossy()
        );

        Ok(Self {
            inner: Arc::new(Mutex::new(card)),
            reader_name: Arc::from(reader_name.to_owned()),
            protocol,
            iccid: OnceCell::new(),
        })
    }

    fn create_card(reader_name: &CStr, protocol: Protocols) -> DynResult<Card> {
        let ctx = Context::establish(Scope::User)
            .map_err(|err| {
                log::error!("Failed to establish context: {}", err);
                Box::<dyn StdError + Send + Sync>::from(err)
            })?;

        let card = catch_pcsc_panic(|| ctx.connect(reader_name, ShareMode::Shared, protocol))?
            .map_err(|err| {
                log::error!("Failed to connect to card: {}", err);
                Box::<dyn StdError + Send + Sync>::from(err)
            })?;

        Ok(card)
    }

    pub async fn reconnect(&self) {
        debug!(
            "Attempting to reconnect card for reader: {}",
            self.reader_name.to_string_lossy()
        );

        let reconnect_result: Result<(), String> = {
            let mut card = self.inner.lock().await;
            match catch_pcsc_panic(move || {
                card.reconnect(ShareMode::Shared, Protocols::ANY, Disposition::ResetCard)
            }) {
                Ok(Ok(())) => Ok(()),
                Ok(Err(pcsc_err)) => Err(pcsc_err.to_string()),
                Err(panic_err) => Err(panic_err.to_string()),
            }
        };

        match reconnect_result {
            Ok(_) => {
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

    pub async fn recreate(&self) -> DynResult<()> {
        let new_card = Self::create_card(&self.reader_name, self.protocol)?;
        let mut lock = self.inner.lock().await;
        *lock = new_card;

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

        debug!(
            "Cloned card for blocking transmission. Sending to spawn_blocking..."
        );

        let response = tauri::async_runtime::spawn_blocking(move || {
            debug!("Entered spawn_blocking thread. Preparing buffer and locking card...");

            let mut rapdu_buf = [0u8; MAX_BUFFER_SIZE];

            let locked = card.blocking_lock();
            debug!("Lock acquired. Transmitting...");

            match catch_pcsc_panic(|| locked.transmit(&apdu, &mut rapdu_buf)) {
                Ok(Ok(response)) => {
                    let encoded = hex::encode(response);
                    debug!("APDU transmit success. Encoded response: {}", encoded);
                    Ok(encoded)
                }
                Ok(Err(err)) => {
                    error!("APDU transmit failed: {}", err);
                    Err(format!("Transmit error: {}", err))
                }
                Err(panic_err) => {
                    error!("APDU transmit panicked: {}", panic_err);
                    Err(format!("Transmit panicked: {}", panic_err))
                }
            }
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
