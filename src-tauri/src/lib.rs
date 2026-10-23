// ───── Modules ─────
mod config;             // Configuration handling.
mod logger;             // Logging functionality.
mod smart_card;         // PCSC module for smart card operations.
mod apdu_sniffer;       // Passive sniffer for plaintext EF data in proxied APDUs.
mod global_app_handle;  // Global access to app state and emitters.
mod socket_server;      // TCP listen server (replacement for the outbound MQTT client).
mod device_protocol;    // Device protocol message definitions and parsing.
mod card_pool;          // Bounded pool of physical cards shared across authenticating devices.
mod vu_download;        // VU data download: CRC verification, VRN extraction, storage.
mod download_activity;  // Downloads in progress, for the UI.
mod firmware;
mod partial_downloads;  // Downloads whose connection dropped, kept for the device to resume.
mod database;           // SQLite: download history (download interval) and card session lengths.
mod users;              // User accounts (database) and the built-in admin, whose password is in config.yaml.
mod company_directory;  // Which company a device belongs to (IMEI registry today; tracking platform later).
#[cfg(not(feature = "desktop"))]
mod web_server;         // Server build's web UI: login, the commands over HTTP, events over SSE.
mod rt;                 // The tokio runtime, shared with Tauri in the desktop app.
#[cfg(feature = "desktop")]
mod desktop;            // The Tauri window, tray icon and single-instance handling.
mod admission;          // Admit a device now or tell it when to come back (queue wait, backlog, download interval).
// mod com_port;          // COM port handling.

#[cfg(not(feature = "desktop"))]
use crate::config::{CacheSection, get_from_cache};

use std::path::PathBuf;

/// Runs the desktop app, or in the server build (without the `desktop` feature) the
/// headless server with its web UI. Each build is one or the other.
#[cfg_attr(all(mobile, feature = "desktop"), tauri::mobile_entry_point)]
pub fn run() {
    // `--config <path>` must be applied before anything reads the config or starts logging.
    match config_path_from_args(std::env::args().skip(1)) {
        Ok(Some(path)) => config::set_config_path_override(path),
        Ok(None) => {}
        Err(e) => exit_with_usage_error(&e),
    }

    #[cfg(feature = "desktop")]
    desktop::run();

    #[cfg(not(feature = "desktop"))]
    {
        let web_port = match web_port_from_args(std::env::args().skip(1)) {
            Ok(port) => port.unwrap_or(web_server::DEFAULT_WEB_PORT),
            Err(e) => exit_with_usage_error(&e),
        };
        run_headless(web_port);
    }
}

/// Prints a command-line mistake and exits.
fn exit_with_usage_error(message: &str) -> ! {
    attach_console();
    eprintln!("{}", message);
    std::process::exit(2);
}

/// Sends the frontend's settings (the `global-config-*` events).
pub(crate) fn emit_settings() {
    config::emit_global_config_connection();
    config::emit_global_config_download();
    config::emit_global_config_logging();
    config::emit_global_config_appearance();
}

/// Sends a freshly (re)loaded page everything it otherwise only learns from events:
/// settings, card configs, and reader state. Used for desktop reloads and by web clients
/// (`web_server`), and it goes to every client — they're all idempotent updates.
pub(crate) fn resend_state() {
    config::emit_card_configs();
    emit_settings();
    download_activity::emit_active_downloads();
    rt::spawn(async {
        if let Err(e) = smart_card::manual_sync_cards(String::new(), false).await {
            log::error!("Failed to resync readers: {}", e);
        }
    });
}

/// The server build: runs the backend (card readers, device server) and the web UI,
/// with no window or tray, until Ctrl+C or SIGTERM (`docker stop`). Frontend events go
/// to logged-in web clients (`global_app_handle::emit`).
#[cfg(not(feature = "desktop"))]
fn run_headless(web_port: u16) {
    // Config first — logging reads log_retention_days from the config cache
    // this populates, so it must run before logger::setup_logging(). No
    // logger exists yet here, so log::* would be silently dropped;
    // eprintln!/println! are deliberate, matching logger.rs's own
    // pre-init fallback pattern.
    match config::init_config() {
        Ok(_) => println!("Config initialized successfully."),
        Err(e) => eprintln!("Failed to initialize config: {}", e),
    }

    logger::setup_logging();

    if let Some(port) = config::pinned_listen_port() {
        log::info!("[CONFIG] listen_port={} pinned_by={}", port, config::LISTEN_PORT_ENV);
    }
    if let Some(path) = config::pinned_download_path() {
        log::info!("[CONFIG] download_path={:?} pinned_by={}", path, config::DOWNLOAD_PATH_ENV);
    }

    rt::spawn(async {
        smart_card::sc_monitor().await;
    });

    rt::spawn(async {
        // listen_port / connection_limit come from the `connection` section of config.yaml
        // (or TCS_LISTEN_PORT);
        // changes saved from the UI are applied live (see socket_server::apply_settings).
        let listen_port: u16 = get_from_cache(CacheSection::Connection, "listen_port").parse().unwrap_or(config::DEFAULT_SERVER_LISTEN_PORT);
        let connection_limit: u16 = get_from_cache(CacheSection::Connection, "connection_limit").parse().unwrap_or(config::DEFAULT_SERVER_CONNECTION_LIMIT);
        socket_server::run(listen_port, connection_limit).await;
    });

    rt::spawn(web_server::run(web_port));

    log::info!("-== Application is running (headless) ==-");
    log::info!("Tacho Card Server running headless; web UI on port {}. Ctrl+C to exit.", web_port);

    rt::block_on(shutdown_signal());

    log::info!("-== Application is closed (headless) ==-
");
}

/// Waits for Ctrl+C, or SIGTERM on Unix. In Docker the server is PID 1, which ignores
/// SIGTERM without a handler, so `docker stop` would otherwise wait and then kill it.
#[cfg(not(feature = "desktop"))]
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => log::info!("Ctrl+C received; shutting down."),
                    _ = terminate.recv() => log::info!("SIGTERM received; shutting down."),
                }
                return;
            }
            Err(e) => log::error!("Can't listen for SIGTERM ({}); only Ctrl+C will stop the server.", e),
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// Reads `--web-port <port>` / `--web-port=<port>` (the server build's web UI port).
#[cfg(not(feature = "desktop"))]
fn web_port_from_args(args: impl Iterator<Item = String>) -> Result<Option<u16>, String> {
    let mut args = args;
    let mut raw: Option<String> = None;
    while let Some(arg) = args.next() {
        if arg == "--web-port" {
            raw = Some(args.next().ok_or("--web-port requires a port, e.g. --web-port 8080")?);
        } else if let Some(value) = arg.strip_prefix("--web-port=") {
            raw = Some(value.to_string());
        }
    }
    match raw {
        None => Ok(None),
        Some(raw) => match raw.trim().parse::<u16>() {
            Ok(port) if port > 0 => Ok(Some(port)),
            _ => Err(format!("--web-port must be a port number (1-65535), not '{}'", raw)),
        },
    }
}

/// Reads `--config <path>` / `--config=<path>` from the command line. The path may be a
/// config file (created with defaults if it doesn't exist) or an existing directory, in
/// which case `config.yaml` inside it is used. Relative paths are resolved against the
/// current working directory.
fn config_path_from_args(args: impl Iterator<Item = String>) -> Result<Option<PathBuf>, String> {
    let mut args = args;
    let mut raw: Option<String> = None;
    while let Some(arg) = args.next() {
        if arg == "--config" {
            raw = Some(args.next().ok_or("--config requires a path, e.g. --config D:\\tcs\\config.yaml")?);
        } else if let Some(value) = arg.strip_prefix("--config=") {
            raw = Some(value.to_string());
        }
    }
    let Some(raw) = raw else { return Ok(None) };
    if raw.trim().is_empty() {
        return Err("--config requires a path, e.g. --config D:\\tcs\\config.yaml".to_string());
    }

    let mut path = PathBuf::from(raw.trim());
    if path.is_relative() {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("Failed to resolve relative --config path: {}", e))?;
        path = cwd.join(path);
    }
    if path.is_dir() {
        path.push("config.yaml");
    }
    Ok(Some(path))
}

/// Attaches the process to the console of whatever launched it (e.g. cmd/PowerShell),
/// so stdout/stderr are visible when running as a release build (which is built with
/// windows_subsystem = "windows" and therefore starts with no console at all).
/// Fails harmlessly if there is no parent console (e.g. launched from Explorer or as
/// a service) or if a console is already attached (debug builds) — output then just
/// stays in the log file as before.
#[cfg(all(windows, feature = "desktop"))]
fn attach_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(all(windows, feature = "desktop")))]
fn attach_console() {}
