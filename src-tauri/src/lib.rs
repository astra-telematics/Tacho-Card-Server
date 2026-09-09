#[cfg_attr(mobile, tauri::mobile_entry_point)]

// ───── Modules ─────
mod config;             // Configuration handling.
mod logger;             // Logging functionality.
mod smart_card;         // PCSC module for smart card operations.
mod apdu_sniffer;       // Passive sniffer for plaintext EF data in proxied APDUs.
mod global_app_handle;  // Global access to app state and emitters.
mod socket_server;      // TCP listen server (replacement for the outbound MQTT client).
mod device_protocol;    // Device protocol message definitions and parsing.
// mod com_port;          // COM port handling.

// ───── External Crates ─────
use tauri::{async_runtime, Listener, Manager, WindowEvent};

use crate::config::{CacheSection, get_from_cache}; // Tauri application framework and async runtime.

pub fn run() {
    // Headless mode: run the card-monitoring / MQTT backend with no window or tray,
    // for use as a background service (e.g. on a server or via a system service manager).
    if std::env::args().any(|arg| arg == "--headless") {
        run_headless();
        return;
    }

    // start builder to run tauri applicationrustup target add aarch64-pc-windows-msvc
    tauri::Builder::default()
        .setup(move |app| {            
            // Obtain a lightweight reference to the app for convenient interaction
            let app_handle = app.app_handle();

            // Initialize the global application handle
            global_app_handle::set_app_handle(app_handle.clone());

            if let Some(window) = app.get_webview_window("main") {
                // getting Application version foriom the Cargo.toml file
                let version = env!("CARGO_PKG_VERSION");
                // Form new Title with the version
                let title = format!("v{}", version);
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

                    // Initialize logging. This function configures the logging system using the `fern` crate.
                    // need to debug later. Add checking for the init result
                    //
                    logger::setup_logging();

                    // Initialize configuration. This function reads the configuration file and initializes the configuration structure.
                    // The configuration file is located in the `assets` directory and is named `config.yaml`.
                    match config::init_config() {
                        Ok(_) => log::info!("Config initialized successfully."),
                        Err(e) => log::error!("Failed to initialize config: {}", e),
                    }

                    println!("Received event with payload: {:?}", event.payload());
                    // Load server configuration from cache to frontend using event
                    match config::emit_global_config_server(&front_app_handle) {
                        Ok(_) => println!("Global config server emitted successfully."),
                        Err(e) => println!("Failed to emit global config server: {:?}", e),
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
                        // bind_addr / soft_connection_limit are configurable via the
                        // `tcp_server` section of config.yaml.
                        let listen_port: u16 = get_from_cache(CacheSection::Server, "listen_port").parse().unwrap_or(7878);
                        let connection_limit: u16 = get_from_cache(CacheSection::Server, "connection_limit").parse().unwrap_or(1000);
                        if let Err(e) = socket_server::run(
                            listen_port,
                            connection_limit,
                        )
                        .await
                        {
                            log::error!("[TCP] phase=listen status=failed err={:?}", e);
                        }
                    });

                    // Spawn a background task to monitor the COM port for the Smart Card Rack device. This will run concurrently with the main application.
                    // async_runtime::spawn(async {
                    //     // Find the COM port for the Smart Card Rack device using its VID and PID. If found, start monitoring it.
                    //     if let Ok(Some(port)) = com_port::find_card_reader_by_vid_pid(0x0403, 0x6015).await {
                    //         log::info!("Smart Card Rack found at: {}", port);
                            
                    //         // Start continuous monitoring in the background for the COM port. This function will run indefinitely, checking for card insertions and removals.
                    //         match com_port::monitor_card_reader(port).await {
                    //             Ok(_) => log::info!("COM port monitor finished"),
                    //             Err(e) => log::error!("COM port monitor error: {}", e),
                    //         }
                    //     } else {
                    //         log::warn!("Smart Card Rack device not found (VID:0x0403, PID:0x6015)");
                    //     }
                    // });
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

    logger::setup_logging();

    match config::init_config() {
        Ok(_) => log::info!("Config initialized successfully."),
        Err(e) => log::error!("Failed to initialize config: {}", e),
    }

    async_runtime::spawn(async {
        smart_card::sc_monitor().await;
    });

    async_runtime::spawn(async {
        // Start the TCP listen server (replacement for the outbound MQTT client).
        // bind_addr / soft_connection_limit are configurable via the
        // `tcp_server` section of config.yaml.
        let listen_port: u16 = get_from_cache(CacheSection::Server, "listen_port").parse().expect("Invalid listen_port in cache");
        let max_connections: u16 = get_from_cache(CacheSection::Server, "connection_limit").parse().expect("Invalid connection_limit in cache");
        if let Err(e) = socket_server::run(
            listen_port,
            max_connections,
        )
        .await
        {
            log::error!("[TCP] phase=listen status=failed err={:?}", e);
        }
    });

    log::info!("-== Application is running in headless mode ==-");
    println!("Tacho Card Server running in headless mode (no UI). Press Ctrl+C to exit.");

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
