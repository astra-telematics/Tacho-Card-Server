//! The desktop app: the Tauri window, tray icon and single-instance handling. Only in
//! builds with the `desktop` feature (the default); `--no-default-features` builds the
//! server alone (always headless, with the web UI), with no Tauri or webview libraries.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Listener, Manager, WindowEvent};
use tauri_plugin_notification::NotificationExt;

use crate::config::{self, get_from_cache, CacheSection};
use crate::{emit_settings, global_app_handle, logger, resend_state, rt, smart_card, socket_server, users};

/// Set on the first "frontend-loaded" event; see the handler in `run()`.
static BACKEND_STARTED: AtomicBool = AtomicBool::new(false);

/// Set the first time the window is hidden to the tray; see `notify_hidden_to_tray`.
static TRAY_HINT_SHOWN: AtomicBool = AtomicBool::new(false);

/// Runs the desktop app until it's quit.
pub fn run() {
    // Tauri's async commands and ours share one runtime (see `rt`).
    tauri::async_runtime::set(rt::handle());

    let mut context = tauri::generate_context!();
    // Single-instance detection keys on the app identifier; make it per config file so
    // `--config` instances with different configs can run side by side.
    if let Some(suffix) = config::instance_id_suffix() {
        context.config_mut().identifier.push_str(&suffix);
    }

    // start builder to run tauri applicationrustup target add aarch64-pc-windows-msvc
    tauri::Builder::default()
        // Must be the first plugin: a second launch (with the same config) exits here
        // and brings the already-running instance's window to the front instead.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            log::info!("Second launch detected; showing the running instance.");
            show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
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
                        resend_state();
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
                    emit_settings();

                    // Run async function in the background with the Tauri runtime
                    // let app_handle_for_sc_monitor = app_handle.clone();
                    rt::spawn(async {
                        /*
                            This slip is needed as a temporary solution. Fix it later!
                            The fact is that the back-end starts faster than the front, and the sent event with card data arrives at the front-end before it has time to load.
                            *** In the near future, I will add a flag for the state of readiness to receive events from the backend. ***
                        */
                        // Start monitoring smart cards. This function will run fсorever with the loop
                        smart_card::sc_monitor().await;
                    });

                    rt::spawn(async {
                        // Start the TCP listen server (replacement for the outbound MQTT client).
                        // listen_port / connection_limit come from the `server` section of
                        // config.yaml; changes saved from the UI are applied live
                        // (see socket_server::apply_settings).
                        let listen_port: u16 = get_from_cache(CacheSection::Connection, "listen_port").parse().unwrap_or(config::DEFAULT_SERVER_LISTEN_PORT);
                        let connection_limit: u16 = get_from_cache(CacheSection::Connection, "connection_limit").parse().unwrap_or(config::DEFAULT_SERVER_CONNECTION_LIMIT);
                        socket_server::run(listen_port, connection_limit).await;
                    });
                });

                // With `appearance.minimize_to_tray` on (the default), closing the window
                // hides it to the tray and the backend keeps running; the tray's "Quit"
                // is then the way to exit. With it off, closing exits as normal.
                let close_window = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        // Empty = no appearance section yet, which means the default (on).
                        if get_from_cache(CacheSection::Appearance, "minimize_to_tray") == "false" {
                            log::info!("-== Application is closed by user ==-\n");
                            return;
                        }
                        api.prevent_close();
                        if let Err(e) = close_window.hide() {
                            log::error!("Failed to hide window to tray: {:?}", e);
                        }
                        log::info!("Window hidden to tray.");
                        notify_hidden_to_tray(close_window.app_handle());
                    }
                });
            }

            build_tray(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            config::update_card,           // update list of cards from the frontend
            config::update_connection,     // update connection config from the frontend
            config::update_download,       // update download config from the frontend
            config::update_logging,        // update logging config from the frontend
            config::update_appearance,     // update appearance config from the frontend
            config::get_config,            // current settings, for the config dialog's tabs
            config::get_companies,         // companies with their devices and cards
            config::save_company,          // create or edit a company
            config::delete_company,        // delete a company (its cards become unassigned)
            config::assign_card,           // move a card to a company, or unassign it
            config::assign_device,         // register a device (IMEI) to a company, or unregister it
            config::get_devices,           // registered and unknown devices
            crate::download_activity::get_active_downloads, // downloads in progress
            config::remove_card,           // remove card from config
            users::get_users,              // user accounts, for the Users page
            users::add_user,               // create a user
            users::set_user_password,      // change a user's password (not admin's)
            users::delete_user,            // delete a user (not admin)
            users::get_my_theme,           // the UI theme (admin's: no login on desktop)
            users::set_my_theme,           // change the UI theme
            smart_card::manual_sync_cards, // manual sync cards from the frontend
            logger::open_logs_folder,      // open the log directory in the OS file manager
        ])
        .run(context)
        .expect("error while running tauri application");
}

/// Shows, unminimises and focuses the main window (from the tray, or when a second
/// launch hands over to this instance).
fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Tells the user, once per run, that closing the window left the app running in the
/// tray — otherwise it looks like it exited while it's still holding the port and cards.
fn notify_hidden_to_tray(app: &AppHandle) {
    if TRAY_HINT_SHOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Err(e) = app
        .notification()
        .builder()
        .title("Tacho Card Server is still running")
        .body("It's been minimised to the system tray. Use Quit from the tray icon to exit.")
        .show()
    {
        log::warn!("Failed to show tray notification: {:?}", e);
    }
}

/// Tray icon: left click shows the window; the menu has Show and Quit.
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Tacho Card Server")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => {
                log::info!("-== Application is closed by user ==-\n");
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}
