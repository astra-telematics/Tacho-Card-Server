#[cfg_attr(mobile, tauri::mobile_entry_point)]

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
// mod com_port;          // COM port handling.

// ───── External Crates ─────
use tauri::{async_runtime, Listener, Manager, WindowEvent};

use crate::config::{CacheSection, get_from_cache}; // Tauri application framework and async runtime.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set on the first "frontend-loaded" event; see the handler in `run()`.
static BACKEND_STARTED: AtomicBool = AtomicBool::new(false);

pub fn run() {
    // Headless mode: run the card-monitoring / MQTT backend with no window or tray,
    // for use as a background service (e.g. on a server or via a system service manager).
    if std::env::args().any(|arg| arg == "--headless") {
        run_headless();
        return;
    }

    // start builder to run tauri applicationrustup target add aarch64-pc-windows-msvc
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            // Obtain a lightweight reference to the app for convenient interaction
            let app_handle = app.app_handle();

            // Initialize the global application handle
            global_app_handle::set_app_handle(app_handle.clone());

            if let Some(window) = app.get_webview_window("main") {
                // getting Application version foriom the Cargo.toml file
                let version = env!("CARGO_PKG_VERSION");
                // Form new Title with the version
                let title = format!("Tacho Card Server: v{}", version);
                // Set new title to the window
                window
                    .set_title(&title)
                    .expect("Failed to set window title");

                let front_app_handle = app_handle.clone();

                // Frontend loading is late, so we execute a callback to the "frontend-loaded" event which the front sends when it is loaded
                window.listen("frontend-loaded", move |event: tauri::Event| {
                    #[cfg(target_os = "linux")]
                    {   // Temporary solution only for linux because webview does not load even after response from front.
                        // Apparently loading occurs later, not like Windows and MacOS. Fix later.
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    }
                    #[cfg(target_os = "windows")] {
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    }

                    // "frontend-loaded" fires on every webview load, including reloads
                    // (Ctrl+R, Vite full reloads in dev). Backend startup must only run
                    // once per process; later loads just re-send state to the fresh page.
                    if BACKEND_STARTED.swap(true, Ordering::SeqCst) {
                        log::debug!("Frontend reloaded; re-sending state.");
                        config::emit_card_configs();
                        if let Err(e) = config::emit_global_config_server(&front_app_handle) {
                            log::error!("Failed to emit global config server: {:?}", e);
                        }
                        async_runtime::spawn(async {
                            if let Err(e) = smart_card::manual_sync_cards(String::new(), false).await {
                                log::error!("Failed to resync readers after reload: {}", e);
                            }
                        });
                        return;
                    }

                    // Initialize configuration first — logging reads log_retention_days
                    // from the config cache this populates, so it must run before
                    // logger::setup_logging(). No logger exists yet at this point, so
                    // log::* calls here would be silently dropped — eprintln!/println!
                    // are deliberate, not an oversight, matching logger.rs's own
                    // pre-init fallback pattern.
                    match config::init_config() {
                        Ok(_) => println!("Config initialized successfully."),
                        Err(e) => eprintln!("Failed to initialize config: {}", e),
                    }

                    // Initialize logging. This function configures the logging system
                    // (rotation/retention/console duplication) via `flexi_logger`.
                    logger::setup_logging();

                    log::debug!("Received event with payload: {:?}", event.payload());
                    // Load server configuration from cache to frontend using event
                    match config::emit_global_config_server(&front_app_handle) {
                        Ok(_) => log::debug!("Global config server emitted successfully."),
                        Err(e) => log::error!("Failed to emit global config server: {:?}", e),
                    }

                    // Run async function in the background with the Tauri runtime
                    // let app_handle_for_sc_monitor = app_handle.clone();
                    async_runtime::spawn(async {
                        /*
                            This slip is needed as a temporary solution. Fix it later!
                            The fact is that the back-end starts faster than the front, and the sent event with card data arrives at the front-end before it has time to load.
                            *** In the near future, I will add a flag for the state of readiness to receive events from the backend. ***
                        */
                        // Start monitoring smart cards. This function will run fсorever with the loop
                        smart_card::sc_monitor().await;
                    });

                    async_runtime::spawn(async {
                        // Start the TCP listen server (replacement for the outbound MQTT client).
                        // listen_port / connection_limit come from the `server` section of
                        // config.yaml; changes saved from the UI are applied live
                        // (see socket_server::apply_settings).
                        let listen_port: u16 = get_from_cache(CacheSection::Server, "listen_port").parse().unwrap_or(config::DEFAULT_SERVER_LISTEN_PORT);
                        let connection_limit: u16 = get_from_cache(CacheSection::Server, "connection_limit").parse().unwrap_or(config::DEFAULT_SERVER_CONNECTION_LIMIT);
                        socket_server::run(listen_port, connection_limit).await;
                    });
                });

                // Handle the application close event to log this.
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { .. } = event {
                        log::info!("-== Application is closed by user ==-\n");
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            config::update_card,           // update list of cards from the frontend
            config::update_server,         // update server config from the frontend
            config::remove_card,            // remove card from config
            smart_card::manual_sync_cards, // manual sync cards from the frontend
            logger::open_logs_folder,      // open the log directory in the OS file manager
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Runs the backend (card monitoring + MQTT connection) without creating a
/// window or tray icon. Config is read from / must be edited in the same
/// config.yaml the UI would otherwise manage. There is no global app handle
/// in this mode, so frontend event emissions (`global_app_handle`) are
/// skipped automatically and just get logged.
fn run_headless() {
    // In release builds the binary is built with windows_subsystem = "windows" (see main.rs),
    // which starts the process with no console attached at all, so stdout/stderr writes
    // silently go nowhere. Attach to the launching terminal's console (if any) so println!/log
    // output shows up live when run from cmd/PowerShell. Must happen before any stdout use.
    attach_console();

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

    async_runtime::spawn(async {
        smart_card::sc_monitor().await;
    });

    async_runtime::spawn(async {
        // Start the TCP listen server (replacement for the outbound MQTT client).
        // listen_port / connection_limit come from the `server` section of config.yaml.
        let listen_port: u16 = get_from_cache(CacheSection::Server, "listen_port").parse().expect("Invalid listen_port in cache");
        let max_connections: u16 = get_from_cache(CacheSection::Server, "connection_limit").parse().expect("Invalid connection_limit in cache");
        socket_server::run(listen_port, max_connections).await;
    });

    log::info!("-== Application is running in headless mode ==-");
    log::info!("Tacho Card Server running in headless mode (no UI). Press Ctrl+C to exit.");

    // Keep the process alive; the monitoring/MQTT tasks run in the background.
    async_runtime::block_on(async {
        let _ = tokio::signal::ctrl_c().await;
    });

    log::info!("-== Application is closed by user (headless) ==-\n");
}

/// Attaches the process to the console of whatever launched it (e.g. cmd/PowerShell),
/// so stdout/stderr are visible when running as a release build (which is built with
/// windows_subsystem = "windows" and therefore starts with no console at all).
/// Fails harmlessly if there is no parent console (e.g. launched from Explorer or as
/// a service) or if a console is already attached (debug builds) — output then just
/// stays in the log file as before.
#[cfg(windows)]
fn attach_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_console() {}
