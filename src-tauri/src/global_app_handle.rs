// ───── Std Lib ─────
#[cfg(feature = "desktop")]
use std::sync::Mutex;

// ───── External Crates ─────
use serde::Serialize;
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};
#[cfg(not(feature = "desktop"))]
use {once_cell::sync::Lazy, tokio::sync::broadcast};

// ───── Local Modules ─────
use crate::config::CardConfig;

/// The desktop window's app handle, for emitting events to it.
#[cfg(feature = "desktop")]
static APP_HANDLE: Mutex<Option<AppHandle>> = Mutex::new(None);

#[cfg(feature = "desktop")]
fn app_handle_slot() -> std::sync::MutexGuard<'static, Option<AppHandle>> {
    // Poisoned only by an earlier panic; keep emitting rather than cascading it.
    APP_HANDLE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Sets the desktop window's app handle, so `emit` reaches the window.
#[cfg(feature = "desktop")]
pub fn set_app_handle(handle: AppHandle) {
    *app_handle_slot() = Some(handle);
}

/// An event for the frontend, as sent to web clients (`web_server`'s `/api/events`).
#[cfg(not(feature = "desktop"))]
#[derive(Clone, Debug)]
pub struct UiEvent {
    pub name: String,
    pub payload: serde_json::Value,
}

/// Events for web clients. Each `/api/events` stream subscribes; a client too slow to
/// keep up skips what it missed (it reloads state when it reconnects).
#[cfg(not(feature = "desktop"))]
static WEB_EVENTS: Lazy<broadcast::Sender<UiEvent>> = Lazy::new(|| broadcast::channel(256).0);

/// A receiver of every event emitted from now on, for a web client's event stream.
#[cfg(not(feature = "desktop"))]
pub fn subscribe_web_events() -> broadcast::Receiver<UiEvent> {
    WEB_EVENTS.subscribe()
}

/// Sends an event to the frontend: the desktop window in the desktop build, logged-in
/// web clients in the server build. Nobody being there to receive it is fine.
pub fn emit<S: Serialize>(event_name: &str, payload: S) {
    let payload = match serde_json::to_value(payload) {
        Ok(value) => value,
        Err(e) => {
            log::error!("emit '{}' failed to serialise payload: {}", event_name, e);
            return;
        }
    };
    #[cfg(feature = "desktop")]
    {
        let app_handle = app_handle_slot().clone();
        if let Some(app_handle) = app_handle {
            if let Err(e) = app_handle.emit(event_name, &payload) {
                log::error!("emit '{}' failed: {:?}", event_name, e);
            }
        }
    }
    // Err only means no web client is connected.
    #[cfg(not(feature = "desktop"))]
    let _ = WEB_EVENTS.send(UiEvent { name: event_name.to_string(), payload });
    log::debug!("'{}' has been sent", event_name);
}

/// Represents the state of a tachograph card.
///
/// This structure holds information about a tachograph card currently being
/// interacted with through a smart card reader.
///
/// # Fields
///
/// * `atr` - A string representing the Answer To Reset (ATR) of the card. The ATR is a sequence
///   of bytes returned by the card upon reset, identifying the card's communication parameters.
/// * `reader_name` - The name of the smart card reader through which the card is being accessed.
/// * `card_state` - A string describing the current state of the card (e.g., "Inserted", "Removed").
/// * `card_number` - The identification number of the tachograph card.
#[derive(Clone, serde::Serialize)]
pub struct TachoState {
    pub iccid: String,
    pub reader_name: String,
    pub card_state: String,
    pub card_number: String,
    pub online: Option<bool>,
    pub authentication: Option<bool>,
}

pub fn card_emit_event(event_name: &str, iccid: String, reader_name: String, card_state: String, card_number: String, online: Option<bool>, authentication: Option<bool>) {
    let payload = TachoState {
        iccid,
        reader_name,
        card_state,
        card_number,
        online,
        authentication
    };

    emit(event_name, payload);
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CardConfigPayload {
    pub card_number: String,
    pub content: Option<CardConfig>,
}

pub fn emit_card_config_event(event_name: &str, card_number: String, config: Option<CardConfig>) {
    let payload = CardConfigPayload {
        card_number,
        content: config,
    };

    emit(event_name, payload);
}

#[derive(Clone, Serialize)]
pub struct NotificationPayload {
    pub notification_type: String,
    pub message: String,
}

/// Emits the server status to the frontend.
pub fn server_emit_event(connections: u16) {
    emit("server-status", connections);
}

pub fn emit_notification_event(event_name: &str, payload: NotificationPayload) {
    emit(event_name, payload);
}