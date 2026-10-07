// ───── Std Lib ─────
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fs;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

// ───── External Crates ─────
use serde::{Deserialize, Serialize};
use serde_yaml;
use lazy_static::lazy_static;
use tauri::Emitter;

// ───── Local Modules ─────
use crate::global_app_handle::emit_card_config_event;
// use crate::smart_card::manual_sync_cards;


// ----- Constants -----
pub const DEFAULT_LOG_RETENTION_DAYS: u32 = 7;
pub const DEFAULT_SERVER_LISTEN_PORT: u16 = 7878;
pub const DEFAULT_SERVER_CONNECTION_LIMIT: u16 = 10;
/// 1 = once a day. 0 disables the limit. See `database.rs`.
pub const DEFAULT_DOWNLOAD_INTERVAL_DAYS: u32 = 7;
/// Upper bound for `download_interval_days`. Regulations require a VU download at least
/// every 90 days; 60 leaves a month of headroom for devices that miss their slot.
pub const MAX_DOWNLOAD_INTERVAL_DAYS: u32 = 60;


/// Represents the configuration settings for the application.
#[derive(Serialize, Deserialize, Debug)]
pub struct ConfigurationFile {
    name: String,                           // The name of the application.
    version: String,                        // The version of the application.
    description: String,                    // A brief description of the application.
    appearance: Option<AppearanceConfig>,   // Optional UI configuration settings.
    connection: Option<ConnectionConfig>,   // Optional server configuration settings.
    logging: Option<LoggingConfig>,         // Optional log rotation/retention settings.
    download: Option<DownloadConfig>,       // Optional download configuration settings.
    cards: HashMap<String, CardConfig>,     // Hashmap of the cards with the CardConfig structure
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ConnectionConfig {
    pub listen_port: u16,
    pub connection_limit: u16,
}
// Each field is defaulted so a section missing one (hand-edited, or written by an older
// version) still parses, rather than failing and init_config() resetting the whole
// config — cards included — to defaults.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DownloadConfig {
    #[serde(default = "default_download_path")]
    pub download_path: String,
    #[serde(default = "default_download_interval_days")]
    pub download_interval_days: u32,
    #[serde(default)]
    pub retain_failed_downloads: bool,
}

fn default_download_interval_days() -> u32 {
    DEFAULT_DOWNLOAD_INTERVAL_DAYS
}

/// `get_default_download_path()`, falling back to a relative "downloads" directory if
/// the home directory can't be resolved.
pub fn default_download_path() -> String {
    get_default_download_path().unwrap_or_else(|_| "downloads".to_string())
}


/// Log file rotation/retention settings. See `logger.rs`. Generated with the
/// default retention once on first boot (or on upgrade from a config that
/// predates this field) in `init_config()`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LoggingConfig {
    /// How many days of rotated (compressed) log files to keep before they're
    /// deleted. A new log file is started on every process launch and,
    /// additionally, once per calendar day while running.
    pub retention_days: u32,
}

// Dark Theme enum, part of AppearanceConfig that contains data about the theme.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum DarkTheme {
    Auto,
    Dark,
    Light,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CardConfig {
    pub iccid: String,                          // ICCID
    pub expire: Option<u64>,                    // Expire date
    pub name: Option<String>,                   // Custom card name (for ease of user identification)
    pub card_type: Option<u8>,                  // typeOfTachographCardId: 1=Driver, 2=Workshop, 3=Control, 4=Company
    pub structure_version: Option<(u8, u8)>,    // cardStructureVersion (major, minor): major 0x00=Gen1, 0x01=Gen2; minor = data-element revision
    pub company_name: Option<String>,           // Company name (from EF_Identification, Company Card only)
    pub company_address: Option<String>,        // Company address (from EF_Identification, Company Card only)
    pub last_auth: Option<(u64, bool)>,         // Last completed authentication: (unix_timestamp, success_flag)
}
// UI Configuration structure, part of ConfigurationFile that contains data about how UI looks like.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppearanceConfig {
    pub dark_theme: DarkTheme,
    /// Closing the window hides it to the tray instead of exiting. Defaulted so
    /// configs written before this field existed still parse.
    #[serde(default = "default_minimize_to_tray")]
    pub minimize_to_tray: bool,
}

fn default_minimize_to_tray() -> bool {
    true
}

/// Config file path given with `--config` on the command line, if any. Set once at
/// startup, before anything reads the config.
static CONFIG_PATH_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

const DEFAULT_CONFIG_FILE_NAME: &str = "config.yaml";

/// Use `path` (an absolute config file path) instead of `~/Documents/tcs/config.yaml`.
/// Its directory becomes the data directory, so logs and the default downloads folder
/// move with it. Must be called before `init_config()`; later calls are ignored.
pub fn set_config_path_override(path: PathBuf) {
    let _ = CONFIG_PATH_OVERRIDE.set(path);
}

/// Suffix for the app identifier when `--config` is in use, so single-instance
/// detection is per config file: relaunching with the same config focuses the running
/// window, while a different config starts its own instance. `None` for the default
/// config, which keeps the plain identifier.
pub fn instance_id_suffix() -> Option<String> {
    use std::hash::{Hash, Hasher};

    use std::path::Component;

    let path = CONFIG_PATH_OVERRIDE.get()?;
    // Lexical normalisation (the path is already absolute): drop `.`, resolve `..`, and
    // unify separators, so `D:\tcs\config.yaml` and `d:/tcs/./config.yaml` match. Not
    // `fs::canonicalize`, which fails until the file exists and adds a `\\?\` prefix
    // on Windows, so two launches could disagree.
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    let key = normalized.to_string_lossy();
    #[cfg(target_os = "windows")]
    let key = key.to_lowercase();

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    Some(format!(".cfg{:016x}", hasher.finish()))
}

/// The directory holding the config file (`~/Documents/tcs` unless overridden with
/// `--config`). Logs and the default downloads folder live under it. Created if missing.
pub fn get_data_dir() -> io::Result<PathBuf> {
    let dir = match CONFIG_PATH_OVERRIDE.get() {
        Some(path) => path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "config path has no parent directory"))?,
        None => {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let home_dir = env::var("HOME");

            #[cfg(target_os = "windows")]
            let home_dir = env::var("USERPROFILE");

            let home = home_dir.map_err(|e| {
                log::error!("Failed to get home directory environment variable: {}", e);
                io::Error::new(io::ErrorKind::Other, "Failed to get home directory environment variable")
            })?;
            let mut dir = PathBuf::from(home);
            dir.push("Documents");
            dir.push("tcs");
            dir
        }
    };

    if let Err(e) = fs::create_dir_all(&dir) {
        log::error!("Failed to create data directory {:?}: {}", dir, e);
        return Err(e);
    }

    Ok(dir)
}

/// Retrieves the configuration file path: the `--config` path if one was given,
/// otherwise `~/Documents/tcs/config.yaml`. Creates the containing directory if needed.
pub fn get_config_path() -> io::Result<PathBuf> {
    let dir = get_data_dir()?;
    let file_name = CONFIG_PATH_OVERRIDE
        .get()
        .and_then(|path| path.file_name())
        .unwrap_or(DEFAULT_CONFIG_FILE_NAME.as_ref());
    Ok(dir.join(file_name))
}

/// `<data dir>/downloads`, created if missing.
pub fn get_default_download_path() -> io::Result<String> {
    let dir = get_data_dir()?.join("downloads");

    if let Err(e) = fs::create_dir_all(&dir) {
        log::error!("Failed to create download directory {:?}: {}", dir, e);
        return Err(e);
    }

    Ok(dir.to_str().unwrap_or("").to_string())
}

/// Load the configuration from the file.
/// This function reads the configuration file and parses it.
fn load_config(
    config_path: &Path,
) -> Result<ConfigurationFile, Box<dyn std::error::Error + Send + Sync>> {
    let mut config_contents = String::new();
    File::open(config_path)?.read_to_string(&mut config_contents)?;
    let config: ConfigurationFile = serde_yaml::from_str(&config_contents)?;
    Ok(config)
}

/// Saves the configuration to the file atomically.
/// Writes to a sibling temp file in the same directory, then renames it over the target.
/// This prevents config corruption (empty/partial file) if the process is killed mid-write.
fn save_config(
    config_path: &Path,
    config: &ConfigurationFile,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let yaml = serde_yaml::to_string(config)?;

    let parent = config_path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::Other, "config path has no parent directory")
    })?;
    let file_name = config_path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::Other, "config path has no file name")
    })?;

    let mut tmp_path = parent.to_path_buf();
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".tmp");
    tmp_path.push(tmp_name);

    {
        let mut tmp_file = File::create(&tmp_path)?;
        tmp_file.write_all(yaml.as_bytes())?;
        tmp_file.sync_all()?;
    }

    if let Err(e) = fs::rename(&tmp_path, config_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(Box::new(e));
    }

    Ok(())
}

/// Updates the configuration with a new card.
/// This function updates the configuration file with a new card's ATR and card number.
fn update_card_config(
    config_path: &Path,
    card_number: &str,
    content: CardConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut config = load_config(config_path)?;
    log::debug!("Loaded configuration: {:?}", config);

    // let mut needs_restart = false;
    let mut changed = false;

    match config.cards.get_mut(card_number) {
        Some(existing_card) => {
            if existing_card.iccid.is_empty() {
                // ICCID is being set for the first time
                log::debug!(
                    "Existing card with empty ICCID. Updating: iccid = {}, name = {:?}, expire = {:?}",
                    content.iccid,
                    content.name,
                    content.expire
                );
                existing_card.iccid = content.iccid;
                existing_card.expire = content.expire;
                existing_card.name = content.name;
                existing_card.card_type = content.card_type;
                existing_card.structure_version = content.structure_version;
                existing_card.company_name = content.company_name;
                existing_card.company_address = content.company_address;
                existing_card.last_auth = content.last_auth;
                // needs_restart = true;
                changed = true;
            } else {
                // Update optional fields only (no restart required)
                if existing_card.expire != content.expire
                    || existing_card.name != content.name
                    || existing_card.card_type != content.card_type
                    || existing_card.structure_version != content.structure_version
                    || existing_card.company_name != content.company_name
                    || existing_card.company_address != content.company_address
                    || existing_card.last_auth != content.last_auth
                {
                    log::debug!(
                        "Updating optional fields for card {}: name = {:?}, expire = {:?}, card_type = {:?}, structure_version = {:?}, company_name = {:?}, company_address = {:?}, last_auth = {:?}",
                        card_number,
                        content.name,
                        content.expire,
                        content.card_type,
                        content.structure_version,
                        content.company_name,
                        content.company_address,
                        content.last_auth
                    );
                    existing_card.expire = content.expire;
                    existing_card.name = content.name;
                    existing_card.card_type = content.card_type;
                    existing_card.structure_version = content.structure_version;
                    existing_card.company_name = content.company_name;
                    existing_card.company_address = content.company_address;
                    existing_card.last_auth = content.last_auth;
                    changed = true;
                }
            }
        }
        None => {
            // Add new card entirely
            log::debug!(
                "Adding new card: card_number = {}, iccid = {}, name = {:?}, expire = {:?}",
                card_number,
                content.iccid,
                content.name,
                content.expire
            );
            config.cards.insert(card_number.to_string(), content);
            // needs_restart = true;
            changed = true;
        }
    }

    if changed {
        // Save config to file
        save_config(config_path, &config)?;
        log::debug!("Configuration saved successfully");

        // Load into runtime cache
        load_config_to_cache(&config)?;
        log::debug!("Configuration loaded to cache successfully");

        // Emit frontend update event
        if let Some(card_config) = config.cards.get(card_number) {
            emit_card_config_event(
                "global-card-config-updated",
                card_number.to_string(),
                Some(card_config.clone())
            );
        }

        // // Restart connection if necessary
        // if needs_restart {
        //     log::info!("Restarting connection for card: {}", card_number);
        //     manual_sync_cards(card_number.to_string(), true).await;
        // }
    }

    Ok(())
}

/// Public function to update the configuration with a new card.
/// This function is a Tauri command that updates the configuration file with a new card's ATR and card number.
#[tauri::command]
pub fn update_card(cardnumber: &str, content: CardConfig) -> bool {
    let config_path = match get_config_path() {
        Ok(path) => path,
        Err(e) => {
            log::error!("Failed to get config path: {}", e);
            return false;
        }
    };

    match update_card_config(&config_path, cardnumber, content) {
        Ok(_) => {
            log::info!("The card, {} is added to the configuration!", cardnumber);
            true
        }
        Err(e) => {
            log::error!("Failed to update config: {}", e);
            false
        }
    }
}

pub fn update_connection_config(
    config_path: &Path,
    listen_port: u16,
    connection_limit: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut config = load_config(config_path)?;

    config.connection = Some(ConnectionConfig {
        listen_port,
        connection_limit,
    });
    save_config(config_path, &config)?;
    load_config_to_cache(&config)?;

    // Rebind / re-limit the running TCP server; no-op if neither value changed.
    crate::socket_server::apply_settings(listen_port, connection_limit);

    Ok(())
}

pub fn update_download_config(
    config_path: &Path,
    download_path: String,
    download_interval_days: u32,
    retain_failed_downloads: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut config = load_config(config_path)?;

    config.download = Some(DownloadConfig {
        download_path,
        download_interval_days,
        retain_failed_downloads,
    });
    save_config(config_path, &config)?;
    load_config_to_cache(&config)?;

    Ok(())
}

pub fn update_logging_config(
    config_path: &Path,
    log_retention_days: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut config = load_config(config_path)?;

    config.logging = Some(LoggingConfig {
        retention_days: log_retention_days,
    });
    save_config(config_path, &config)?;
    load_config_to_cache(&config)?;

    Ok(())
}

pub fn update_appearance_config(
    config_path: &Path,
    theme: &str,
    minimize_to_tray: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut config = load_config(config_path)?;

    config.appearance = Some(AppearanceConfig {
        dark_theme: match theme {
            "Auto" => DarkTheme::Auto,
            "Dark" => DarkTheme::Dark,
            "Light" => DarkTheme::Light,
            _ => DarkTheme::Auto,
        },
        minimize_to_tray,
    });
    save_config(config_path, &config)?;
    load_config_to_cache(&config)?;

    Ok(())
}

#[tauri::command]
pub async fn remove_card(
    cardnumber: String,
) -> Result<(), String> {
    let config_path = get_config_path().map_err(|e| {
        log::error!("Failed to get config path: {}", e);
        format!("Failed to get config path: {}", e)
    })?;

    remove_card_from_config(&config_path, &cardnumber)
        .await
        .map_err(|e| {
            log::error!("Failed to remove card from config: {}", e);
            format!("Failed to remove card from config: {}", e)
        })?;

    log::info!("Card {} removed from config", &cardnumber);

    Ok(())
}

pub async fn remove_card_from_config(
    config_path: &Path,
    card_number: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    log::debug!("Loading configuration from {:?}", config_path);
    let mut config = load_config(config_path)?;
    log::debug!("Loaded configuration: {:?}", config);

    if config.cards.remove(card_number).is_some() {
        save_config(config_path, &config)?;
        log::debug!("Configuration saved successfully after removal");

        load_config_to_cache(&config)?;
        log::debug!("Configuration loaded to cache successfully");

        emit_card_config_event("global-card-config-updated", card_number.to_string(), None);

        #[cfg(target_os = "linux")] {
            // "Super hack" to reload card states and trigger an event to update readers.

            use tokio::time::sleep;
            use tokio::time::Duration;
            use crate::smart_card::manual_sync_cards;

            sleep(Duration::from_millis(100)).await;
            manual_sync_cards(card_number.to_string(), false).await;
        }

        Ok(())
    } else {
        log::warn!("Cardnumber {} not found in configuration", card_number);
        Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Card not found in configuration",
        )))
    }
}

#[tauri::command]
pub fn update_connection(listen_port: String, connection_limit: String) -> bool {
    let config_path = match get_config_path() {
        Ok(path) => path,
        Err(e) => {
            log::error!("Failed to get config path: {}", e);
            return false;
        }
    };
    log::info!("Updating connection configuration: listen_port={}, connection_limit={}", listen_port, connection_limit);
    match update_connection_config(&config_path, listen_port.parse().unwrap_or(DEFAULT_SERVER_LISTEN_PORT), connection_limit.parse().unwrap_or(DEFAULT_SERVER_CONNECTION_LIMIT)) {
        Ok(_) => {
            log::info!("The connection configuration is updated: listen_port={}, connection_limit={}", listen_port, connection_limit);
            true
        }
        Err(e) => {
            log::error!("Failed to update connection configuration: {}", e);
            false
        }
    }
}

#[tauri::command]
pub fn update_appearance(theme: &str, minimize_to_tray: bool) -> bool {
    let config_path = match get_config_path() {
        Ok(path) => path,
        Err(e) => {
            log::error!("Failed to get config path: {}", e);
            return false;
        }
    };
    log::info!("Updating appearance configuration: theme={}, minimize_to_tray={}", theme, minimize_to_tray);
    match update_appearance_config(&config_path, theme, minimize_to_tray) {
        Ok(_) => {
            log::info!("The appearance configuration is updated: theme={}, minimize_to_tray={}", theme, minimize_to_tray);
            true
        }
        Err(e) => {
            log::error!("Failed to update appearance configuration: {}", e);
            false
        }
    }
}

#[tauri::command]
pub fn update_download(download_path: String, download_interval_days: String, retain_failed_downloads: bool) -> bool {
    let config_path = match get_config_path() {
        Ok(path) => path,
        Err(e) => {
            log::error!("Failed to get config path: {}", e);
            return false;
        }
    };
    log::info!("Updating download configuration: download_path={}, download_interval_days={}, retain_failed_downloads={}", download_path, download_interval_days, retain_failed_downloads);
    match update_download_config(&config_path, if download_path.trim().is_empty() { default_download_path() } else { download_path.trim().to_string() }, download_interval_days.parse().unwrap_or(DEFAULT_DOWNLOAD_INTERVAL_DAYS).min(MAX_DOWNLOAD_INTERVAL_DAYS), retain_failed_downloads) {
        Ok(_) => {
            log::info!("The download configuration is updated: download_path={}, download_interval_days={}, retain_failed_downloads={}", download_path, download_interval_days, retain_failed_downloads);
            true
        }
        Err(e) => {
            log::error!("Failed to update download configuration: {}", e);
            false
        }
    }
}

#[tauri::command]
pub fn update_logging(log_retention_days: String) -> bool {
    let config_path = match get_config_path() {
        Ok(path) => path,
        Err(e) => {
            log::error!("Failed to get config path: {}", e);
            return false;
        }
    };
    log::info!("Updating logging configuration: log_retention_days={}", log_retention_days);
    match update_logging_config(&config_path, log_retention_days.parse().unwrap_or(DEFAULT_LOG_RETENTION_DAYS)) {
        Ok(_) => {
            log::info!("The logging configuration is updated: log_retention_days={}", log_retention_days);
            true
        }
        Err(e) => {
            log::error!("Failed to update logging configuration: {}", e);
            false
        }
    }
}

/*
  initializing a global cache (HashMap<String, String>) using Mutex.
  Mapping card keys and matching them with the real company card number,
  which can only be entered manually
*/
#[derive(Default, Debug)]
pub struct CacheConfigData {
    pub cards: HashMap<String, CardConfig>,
    pub connection: Option<ConnectionConfig>,
    pub download: Option<DownloadConfig>,
    pub appearance: Option<AppearanceConfig>,
    pub logging: Option<LoggingConfig>,
}

lazy_static! {
    /// Global cache for card ATRs and numbers.
    /// Initializing a global cache (HashMap<String, String>) using Mutex.
    /// Mapping card keys and matching them with the real company card number,
    /// which can only be entered manually.
    static ref CACHE: Mutex<CacheConfigData> = Mutex::new(CacheConfigData::default());
}
#[derive(Debug)]
pub enum CacheSection {
    Cards,
    Connection,
    Download,
    Appearance,
    Logging,
}

/// Returns a clone of the CardConfig for the given card number from the runtime cache,
/// or None if the card is not known yet.
pub fn get_card_config_from_cache(card_number: &str) -> Option<CardConfig> {
    let cache = CACHE.lock().unwrap();
    cache.cards.get(card_number).cloned()
}

/// Records the final state of an authentication attempt in the card config:
/// `success == true` → green "success" line in UI; `false` → red "fail".
/// The processing state while auth is running is derived from Reader.authentication
/// in the frontend and is NOT stored here (it's transient, lost on restart).
pub fn record_auth_result(card_number: &str, success: bool) {
    let Some(mut cfg) = get_card_config_from_cache(card_number) else {
        log::warn!("record_auth_result: unknown card_number {}", card_number);
        return;
    };
    let ts = chrono::Utc::now().timestamp() as u64;
    cfg.last_auth = Some((ts, success));
    if !update_card(card_number, cfg) {
        log::error!(
            "record_auth_result: failed to persist last_auth for card_number {}",
            card_number
        );
    }
}

/// Async-safe variant of `record_auth_result`.
/// Offloads the blocking config write (disk I/O + sync mutexes) to a dedicated
/// blocking thread so the caller's tokio worker is not stalled. Use this from
/// async contexts such as the MQTT eventloop tasks.
pub async fn record_auth_result_async(card_number: &str, success: bool) {
    let card_number = card_number.to_string();
    if let Err(e) = tokio::task::spawn_blocking(move || {
        record_auth_result(&card_number, success);
    })
    .await
    {
        log::error!("record_auth_result_async: spawn_blocking failed: {:?}", e);
    }
}

/// Retrieves a value from the cache by key.
/// This function locks the cache, retrieves the value for the given key, and returns it.
pub fn get_from_cache(section: CacheSection, key: &str) -> String {
    let cache = CACHE.lock().unwrap();

    log::debug!("Accessing cache section: {:?}, key: {}", section, key);

    match section {
        CacheSection::Cards => {
            log::debug!("Looking up by ICCID: {}", key);

            for (card_number, config) in &cache.cards {
                log::debug!(
                    "Cache entry -> card_number: {}, iccid: {}, expire: {:?}",
                    card_number,
                    config.iccid,
                    config.expire
                );

                if config.iccid == key {
                    log::debug!(
                        "Match found: ICCID {} corresponds to card_number {}",
                        key,
                        card_number
                    );
                    return card_number.clone();
                }
            }

            log::debug!("No ICCID match found for: {}", key);
            "".to_string()
        }

        CacheSection::Connection => {
            log::debug!("Accessing Connection config");
            if let Some(connection) = &cache.connection {
                log::debug!("Connection config: listen port = {}, connection limit = {}", connection.listen_port, connection.connection_limit);
                match key {
                    "listen_port" => connection.listen_port.to_string(),
                    "connection_limit" => connection.connection_limit.to_string(),
                    _ => {
                        log::debug!("Unknown key for connection section: {}", key);
                        "".to_string()
                    }
                }
            } else {
                log::debug!("No connection config found");
                "".to_string()
            }
        }

        CacheSection::Appearance => {
            log::debug!("Accessing Appearance config");
            if let Some(appearance) = &cache.appearance {
                log::debug!("Appearance config: {:?}", appearance);
                match key {
                    "dark_theme" => format!("{:?}", appearance.dark_theme),
                    "minimize_to_tray" => appearance.minimize_to_tray.to_string(),
                    _ => {
                        log::debug!("Unknown key for appearance section: {}", key);
                        "".to_string()
                    }
                }
            } else {
                log::debug!("No appearance config found");
                "".to_string()
            }
        }

        CacheSection::Logging => {
            log::debug!("Accessing Logging config");
            if let Some(logging) = &cache.logging {
                match key {
                    "retention_days" => logging.retention_days.to_string(),
                    _ => {
                        log::debug!("Unknown key for logging section: {}", key);
                        "".to_string()
                    }
                }
            } else {
                log::debug!("No logging config found");
                "".to_string()
            }
        }

        CacheSection::Download => {
            log::debug!("Accessing Download config");
            if let Some(download) = &cache.download {
                match key {
                    "download_path" => download.download_path.clone(),
                    "download_interval_days" => download.download_interval_days.to_string(),
                    "retain_failed_downloads" => download.retain_failed_downloads.to_string(),
                    _ => {
                        log::debug!("Unknown key for download section: {}", key);
                        "".to_string()
                    }
                }
            } else {
                log::debug!("No download config found");
                "".to_string()
            }
        }
    }
}

/// Loads the configuration file into the cache.
/// This function reads the configuration file, parses it, and loads the cards into the global cache,
/// which is used to synchronize the launch of asynchronous tasks for MQTT connection, as well as for display on the interface.
pub fn load_config_to_cache(
    config: &ConfigurationFile,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    log::debug!("load_config_to_cache");

    let mut cache = CACHE.lock().unwrap();
    *cache = CacheConfigData {
        cards: config.cards.clone(),
        connection: config.connection.clone(),
        appearance: config.appearance.clone(),
        logging: config.logging.clone(),
        download: config.download.clone(),
    };

    // trace_cache(&*cache);

    Ok(())
}

/// Initializes the configuration file.
/// This function creates a default configuration file if it does not exist, and loads it into the cache.
pub fn init_config() -> io::Result<()> {
    let config_path = get_config_path()?;
    let mut config: ConfigurationFile;

    if config_path.exists() {
        let mut contents = String::new();
        File::open(&config_path)?.read_to_string(&mut contents)?;

        match serde_yaml::from_str::<ConfigurationFile>(&contents) {
            Ok(mut loaded_config) => {
                loaded_config.version = env!("CARGO_PKG_VERSION").to_string();
                config = loaded_config;
            }
            Err(e) => {
                log::error!("Config parse failed ({}). Resetting to default config.", e);
                config = generate_default_config();
            }
        }
    } else {
        log::debug!("Config file not found. Generating default config.");
        config = generate_default_config();
    }

    if config.logging.is_none() {
        config.logging = Some(LoggingConfig {
            retention_days: DEFAULT_LOG_RETENTION_DAYS,
        });
    }

    if config.download.is_none() {
        config.download = Some(DownloadConfig {
            download_path: default_download_path(),
            download_interval_days: DEFAULT_DOWNLOAD_INTERVAL_DAYS,
            retain_failed_downloads: false,
        });
    }

    // Correct a hand-edited interval above the regulatory cap, so config.yaml and the
    // UI show what's actually enforced.
    if let Some(download) = config.download.as_mut() {
        if download.download_interval_days > MAX_DOWNLOAD_INTERVAL_DAYS {
            eprintln!(
                "download_interval_days {} exceeds the maximum of {}; using {}.",
                download.download_interval_days, MAX_DOWNLOAD_INTERVAL_DAYS, MAX_DOWNLOAD_INTERVAL_DAYS
            );
            download.download_interval_days = MAX_DOWNLOAD_INTERVAL_DAYS;
        }
    }

    if config.connection.is_none() {
        config.connection = Some(ConnectionConfig {
            listen_port: 7878,
            connection_limit: 100,
        });
    }

    save_config(&config_path, &config)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    log::debug!("config: saved config");

    /*
        Send data of all cards in events one by one to the front.
    */
    for (card_number, card_config) in &config.cards {
        emit_card_config_event(
            "global-card-config-updated",
            card_number.clone(),
            Some(card_config.clone()),
        );
    }

    load_config_to_cache(&config)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    Ok(())
}

// Default structure config
fn generate_default_config() -> ConfigurationFile {
    ConfigurationFile {
        name: "Tacho Card Server".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        description: "Application for the tachograph cards authentication".to_string(),
        appearance: Some(AppearanceConfig {
            dark_theme: DarkTheme::Auto,
            minimize_to_tray: true,
        }),
        connection: Some(ConnectionConfig { listen_port: 7878, connection_limit: 100 }),
        download: Some(DownloadConfig { download_path: default_download_path(), download_interval_days: DEFAULT_DOWNLOAD_INTERVAL_DAYS, retain_failed_downloads: false }),
        logging: Some(LoggingConfig { retention_days: DEFAULT_LOG_RETENTION_DAYS }),
        cards: HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_tmp_dir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut dir = std::env::temp_dir();
        dir.push(format!("tcs-test-{}-{}-{}-{}", tag, pid, nanos, seq));
        fs::create_dir_all(&dir).expect("create tmp dir");
        dir
    }

    fn sample_config() -> ConfigurationFile {
        let mut cards = HashMap::new();
        cards.insert(
            "ABCDEF0123456789".to_string(),
            CardConfig {
                iccid: "1122334455667788".to_string(),
                expire: Some(1_700_000_000),
                name: Some("My Company Card".to_string()),
                card_type: Some(4),
                structure_version: Some((1, 0)),
                company_name: Some("Acme Logistics".to_string()),
                company_address: Some("Street 1".to_string()),
                last_auth: Some((1_700_000_500, true)),
            },
        );
        ConfigurationFile {
            name: "TCS".to_string(),
            version: "0.0.0-test".to_string(),
            description: "test".to_string(),
            appearance: Some(AppearanceConfig {
                dark_theme: DarkTheme::Auto,
                minimize_to_tray: true,
            }),
            connection: Some(ConnectionConfig {
                listen_port: 7878,
                connection_limit: 100,
            }),
            download: Some(DownloadConfig {
                download_path: default_download_path(),
                download_interval_days: DEFAULT_DOWNLOAD_INTERVAL_DAYS,
                retain_failed_downloads: false,
            }),
            logging: Some(LoggingConfig { retention_days: 7 }),
            cards,
        }
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = unique_tmp_dir("roundtrip");
        let path = dir.join("config.yaml");

        let cfg = sample_config();
        save_config(&path, &cfg).expect("save");

        let loaded = load_config(&path).expect("load");
        assert_eq!(loaded.cards.len(), 1);
        let card = loaded.cards.get("ABCDEF0123456789").expect("card present");
        assert_eq!(card.iccid, "1122334455667788");
        assert_eq!(card.card_type, Some(4));
        assert_eq!(card.last_auth, Some((1_700_000_500, true)));
        assert_eq!(
            loaded.connection.as_ref().map(|s| s.listen_port),
            Some(DEFAULT_SERVER_LISTEN_PORT)
        );
        assert_eq!(
            loaded.connection.as_ref().map(|s| s.connection_limit),
            Some(DEFAULT_SERVER_CONNECTION_LIMIT)
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_is_atomic_no_tmp_leftover() {
        let dir = unique_tmp_dir("atomic");
        let path = dir.join("config.yaml");
        let tmp_path = dir.join("config.yaml.tmp");

        save_config(&path, &sample_config()).expect("save");
        assert!(path.exists(), "final config must exist");
        assert!(!tmp_path.exists(), "tmp file must be renamed away");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_overwrites_existing_file_safely() {
        let dir = unique_tmp_dir("overwrite");
        let path = dir.join("config.yaml");

        save_config(&path, &sample_config()).expect("first save");
        let first_meta = fs::metadata(&path).expect("meta1");

        let mut second = sample_config();
        second.cards.clear();
        save_config(&path, &second).expect("second save");

        let loaded = load_config(&path).expect("load second");
        assert!(loaded.cards.is_empty(), "second save must replace contents");
        assert!(first_meta.is_file());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_malformed_yaml() {
        let dir = unique_tmp_dir("malformed");
        let path = dir.join("config.yaml");
        fs::write(&path, b"this: is: not: yaml: [[[").expect("write garbage");

        let res = load_config(&path);
        assert!(res.is_err(), "load must fail on malformed yaml");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_default_config_is_well_formed() {
        let cfg = generate_default_config();
        assert!(!cfg.name.is_empty());
        assert!(!cfg.version.is_empty());
        assert!(cfg.cards.is_empty());
    }

    #[test]
    fn card_config_yaml_roundtrip_preserves_optional_fields() {
        let card = CardConfig {
            iccid: "1234567890123456".to_string(),
            expire: None,
            name: None,
            card_type: None,
            structure_version: None,
            company_name: None,
            company_address: None,
            last_auth: None,
        };
        let yaml = serde_yaml::to_string(&card).expect("ser");
        let back: CardConfig = serde_yaml::from_str(&yaml).expect("de");
        assert_eq!(back.iccid, "1234567890123456");
        assert!(back.expire.is_none());
        assert!(back.last_auth.is_none());
    }
}

/// Re-sends every cached card config to the frontend, e.g. after a webview reload
/// wiped its state. `init_config()` does the same from the file on first load.
pub fn emit_card_configs() {
    let cards = CACHE.lock().unwrap().cards.clone();
    for (card_number, card_config) in cards {
        emit_card_config_event("global-card-config-updated", card_number, Some(card_config));
    }
}

/// Every setting the frontend's config dialog shows, keyed as in the
/// `global-config-*` events. All values are strings (see `get_from_cache`).
fn config_payload() -> HashMap<&'static str, String> {
    HashMap::from([
        ("listen_port", get_from_cache(CacheSection::Connection, "listen_port")),
        ("connection_limit", get_from_cache(CacheSection::Connection, "connection_limit")),
        ("download_path", get_from_cache(CacheSection::Download, "download_path")),
        ("download_interval_days", get_from_cache(CacheSection::Download, "download_interval_days")),
        ("retain_failed_downloads", get_from_cache(CacheSection::Download, "retain_failed_downloads")),
        ("retention_days", get_from_cache(CacheSection::Logging, "retention_days")),
        ("dark_theme", get_from_cache(CacheSection::Appearance, "dark_theme")),
        ("minimize_to_tray", get_from_cache(CacheSection::Appearance, "minimize_to_tray")),
    ])
}

/// Current settings, for a config section to load when it's created — the dialog's
/// contents only exist while it's open, so they miss the startup `global-config-*`
/// events.
#[tauri::command]
pub fn get_config() -> HashMap<&'static str, String> {
    config_payload()
}

pub fn emit_global_config_connection(app: &tauri::AppHandle) -> Result<(), Box<dyn Error>> {
    let payload: HashMap<&'static str, String> = HashMap::from([
        ("listen_port", get_from_cache(CacheSection::Connection, "listen_port")),
        ("connection_limit", get_from_cache(CacheSection::Connection, "connection_limit")),
    ]);
    // Emit this data as a global event to update front-end fields
    if let Err(e) = app.emit("global-config-connection", payload) {
        return Err(Box::new(e));
    }

    Ok(())
}

pub fn emit_global_config_download(app: &tauri::AppHandle) -> Result<(), Box<dyn Error>> {
    let payload: HashMap<&'static str, String> = HashMap::from([
        ("download_path", get_from_cache(CacheSection::Download, "download_path")),
        ("download_interval_days", get_from_cache(CacheSection::Download, "download_interval_days")),
        ("retain_failed_downloads", get_from_cache(CacheSection::Download, "retain_failed_downloads")),
    ]);
    // Emit this data as a global event to update front-end fields
    if let Err(e) = app.emit("global-config-download", payload) {
        return Err(Box::new(e));
    }

    Ok(())
}

pub fn emit_global_config_logging(app: &tauri::AppHandle) -> Result<(), Box<dyn Error>> {
    let payload: HashMap<&'static str, String> = HashMap::from([
        ("retention_days", get_from_cache(CacheSection::Logging, "retention_days")),
    ]);
    // Emit this data as a global event to update front-end fields
    if let Err(e) = app.emit("global-config-logging", payload) {
        return Err(Box::new(e));
    }

    Ok(())
}

pub fn emit_global_config_appearance(app: &tauri::AppHandle) -> Result<(), Box<dyn Error>> {
    let payload: HashMap<&'static str, String> = HashMap::from([
        ("dark_theme", get_from_cache(CacheSection::Appearance, "dark_theme")),
        ("minimize_to_tray", get_from_cache(CacheSection::Appearance, "minimize_to_tray")),
    ]);
    // Emit this data as a global event to update front-end fields
    if let Err(e) = app.emit("global-config-appearance", payload) {
        return Err(Box::new(e));
    }

    Ok(())
}