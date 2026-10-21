// ───── Std Lib ─────
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

// ───── External Crates ─────
use serde::{Deserialize, Serialize};
use serde_yaml;
use lazy_static::lazy_static;

// ───── Local Modules ─────
use crate::global_app_handle::{emit, emit_card_config_event};
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
/// Company created on first run, when config.yaml has no companies. Whatever its id,
/// while there's only one company every device and card belongs to it, so a
/// single-company install needs no setup. See `multi-company design.md`.
pub const DEFAULT_COMPANY_ID: &str = "default";
const DEFAULT_COMPANY_NAME: &str = "Default";

// ----- Settings pinned by environment variables -----
//
// For containers: the image sets these so the device port matches the one it exposes,
// and downloads land in the data volume. A pinned setting overrides config.yaml (whose
// value is kept but ignored) and can't be changed from the UI.

/// Device port (`connection.listen_port`).
pub const LISTEN_PORT_ENV: &str = "TCS_LISTEN_PORT";
/// Download folder (`download.download_path`).
pub const DOWNLOAD_PATH_ENV: &str = "TCS_DOWNLOAD_PATH";

/// The device port from `TCS_LISTEN_PORT`, if set. An invalid value is reported once
/// and ignored.
pub fn pinned_listen_port() -> Option<u16> {
    static PORT: std::sync::OnceLock<Option<u16>> = std::sync::OnceLock::new();
    *PORT.get_or_init(|| {
        let value = std::env::var(LISTEN_PORT_ENV).ok()?;
        match value.trim().parse::<u16>() {
            Ok(port) if port > 0 => Some(port),
            _ => {
                eprintln!("{}={:?} isn't a port number (1-65535); ignoring it.", LISTEN_PORT_ENV, value);
                None
            }
        }
    })
}

/// The download folder from `TCS_DOWNLOAD_PATH`, if set and not blank.
pub fn pinned_download_path() -> Option<String> {
    static PATH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        std::env::var(DOWNLOAD_PATH_ENV).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
    })
    .clone()
}


/// Represents the configuration settings for the application.
#[derive(Serialize, Deserialize, Debug)]
pub struct ConfigurationFile {
    name: String,                           // The name of the application.
    version: String,                        // The version of the application.
    description: String,                    // A brief description of the application.
    /// Password of the built-in `admin` account: in plaintext as generated (or typed in
    /// by hand), or its Argon2id hash once changed in the UI. The database is made to
    /// match it on every startup; if missing or blank a random one is generated and
    /// saved here (see `users.rs`).
    #[serde(default)]
    admin_password: Option<String>,
    appearance: Option<AppearanceConfig>,   // Optional UI configuration settings.
    connection: Option<ConnectionConfig>,   // Optional server configuration settings.
    logging: Option<LoggingConfig>,         // Optional log rotation/retention settings.
    download: Option<DownloadConfig>,       // Optional download configuration settings.
    #[serde(default)]
    companies: BTreeMap<String, CompanyConfig>, // Companies served, keyed by company id.
    /// Cards configured (card number known) but not yet assigned to any company.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    unassigned_cards: BTreeMap<String, CardConfig>,
}

impl ConfigurationFile {
    /// The only company, if there's exactly one: every device and card belongs to it.
    fn sole_company(&self) -> Option<String> {
        (self.companies.len() == 1).then(|| self.companies.keys().next().cloned()).flatten()
    }

    /// Every configured card keyed by card number, with its `company` filled in from
    /// where it sits in the file (`None` for unassigned cards).
    fn all_cards(&self) -> HashMap<String, CardConfig> {
        let mut cards = HashMap::new();
        for (company_id, company) in &self.companies {
            for (card_number, card) in &company.cards {
                let mut card = card.clone();
                card.company = Some(company_id.clone());
                cards.insert(card_number.clone(), card);
            }
        }
        for (card_number, card) in &self.unassigned_cards {
            let mut card = card.clone();
            card.company = None;
            cards.insert(card_number.clone(), card);
        }
        cards
    }

    /// One card, with its `company` filled in.
    fn card(&self, card_number: &str) -> Option<CardConfig> {
        self.all_cards().remove(card_number)
    }

    /// The stored entry for a card, wherever it sits.
    fn card_mut(&mut self, card_number: &str) -> Option<&mut CardConfig> {
        if let Some(company_id) = self
            .companies
            .iter()
            .find(|(_, company)| company.cards.contains_key(card_number))
            .map(|(id, _)| id.clone())
        {
            return self.companies.get_mut(&company_id)?.cards.get_mut(card_number);
        }
        self.unassigned_cards.get_mut(card_number)
    }

    /// Removes a card from wherever it sits.
    fn remove_card(&mut self, card_number: &str) -> Option<CardConfig> {
        for company in self.companies.values_mut() {
            if let Some(card) = company.cards.remove(card_number) {
                return Some(card);
            }
        }
        self.unassigned_cards.remove(card_number)
    }

    /// Stores a newly configured card: under the company its `company` names if that
    /// exists, else under `default` while that's the only company, else as unassigned.
    /// `company` isn't stored on the card itself: where it sits says that.
    fn insert_card(&mut self, card_number: &str, mut card: CardConfig) {
        let company = card
            .company
            .take()
            .filter(|company| self.companies.contains_key(company))
            .or_else(|| self.sole_company());
        match company.and_then(|company| self.companies.get_mut(&company)) {
            Some(company) => {
                company.cards.insert(card_number.to_string(), card);
            }
            None => {
                self.unassigned_cards.insert(card_number.to_string(), card);
            }
        }
    }

}

/// A company (client) served by this TCS: its devices, and limits that apply to them.
/// Keyed in `companies:` by its id — lowercase letters, digits and `-` — which names its
/// download subfolder and tags its database rows, so it must not change once data
/// exists. Fields are defaulted so a hand-written entry needs only a name.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CompanyConfig {
    pub name: String,
    /// Most devices of this company connected at once (checked at `ConnAnnounce`).
    #[serde(default = "default_company_connection_limit")]
    pub connection_limit: u16,
    /// Minimum whole days between verified downloads per device: 1 = once a day,
    /// 0 = no limit, at most `MAX_DOWNLOAD_INTERVAL_DAYS`.
    #[serde(default = "default_download_interval_days")]
    pub download_interval_days: u32,
    /// IMEIs of this company's devices.
    #[serde(default)]
    pub devices: Vec<String>,
    /// This company's cards, keyed by card number.
    #[serde(default)]
    pub cards: BTreeMap<String, CardConfig>,
}

fn default_company_connection_limit() -> u16 {
    DEFAULT_SERVER_CONNECTION_LIMIT
}

/// Whether `id` is usable as a company id: non-empty, lowercase letters, digits and
/// `-`, not starting with `-`. It names a download subfolder, so nothing else.
pub fn is_valid_company_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
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
    /// Root folder; each company's downloads go in a subfolder named by its id.
    #[serde(default = "default_download_path")]
    pub download_path: String,
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
    /// Id of the company whose devices this card serves. Not stored on the card in
    /// config.yaml — the card's position (under a company, or in `unassigned_cards`)
    /// says that; filled in when cards are read (`ConfigurationFile::all_cards`) so the
    /// cache and frontend events have it. Never changed by `update_card`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
}
/// Desktop window behaviour. (The theme is per user, in the database: see `users.rs`.)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppearanceConfig {
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
#[cfg(feature = "desktop")]
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

/// Held while config.yaml is read, changed and written back, so two writers (e.g. a UI
/// change and an authentication result) can't each load the file and overwrite the
/// other's change.
static CONFIG_WRITE: Mutex<()> = Mutex::new(());

fn config_write_lock() -> std::sync::MutexGuard<'static, ()> {
    CONFIG_WRITE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
    let _write = config_write_lock();
    let mut config = load_config(config_path)?;
    log::debug!("Loaded configuration: {:?}", config);

    // let mut needs_restart = false;
    let mut changed = false;

    match config.card_mut(card_number) {
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
            config.insert_card(card_number, content);
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
        if let Some(card_config) = config.card(card_number) {
            emit_card_config_event(
                "global-card-config-updated",
                card_number.to_string(),
                Some(card_config)
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
#[cfg_attr(feature = "desktop", tauri::command)]
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
    let _write = config_write_lock();
    let mut config = load_config(config_path)?;

    // A pinned port (TCS_LISTEN_PORT) can't be changed: keep whatever the file has.
    let stored_listen_port = match pinned_listen_port() {
        Some(_) => config.connection.as_ref().map_or(DEFAULT_SERVER_LISTEN_PORT, |c| c.listen_port),
        None => listen_port,
    };
    config.connection = Some(ConnectionConfig {
        listen_port: stored_listen_port,
        connection_limit,
    });
    save_config(config_path, &config)?;
    load_config_to_cache(&config)?;

    // Rebind / re-limit the running TCP server; no-op if neither value changed.
    crate::socket_server::apply_settings(pinned_listen_port().unwrap_or(stored_listen_port), connection_limit);

    Ok(())
}

pub fn update_download_config(
    config_path: &Path,
    download_path: String,
    retain_failed_downloads: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _write = config_write_lock();
    let mut config = load_config(config_path)?;

    // A pinned folder (TCS_DOWNLOAD_PATH) can't be changed: keep whatever the file has.
    let download_path = match pinned_download_path() {
        Some(_) => config.download.as_ref().map_or_else(default_download_path, |d| d.download_path.clone()),
        None => download_path,
    };
    config.download = Some(DownloadConfig {
        download_path,
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
    let _write = config_write_lock();
    let mut config = load_config(config_path)?;

    config.logging = Some(LoggingConfig {
        retention_days: log_retention_days,
    });
    save_config(config_path, &config)?;
    load_config_to_cache(&config)?;

    Ok(())
}

#[cfg(feature = "desktop")] // window behaviour only
pub fn update_appearance_config(
    config_path: &Path,
    minimize_to_tray: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _write = config_write_lock();
    let mut config = load_config(config_path)?;

    config.appearance = Some(AppearanceConfig { minimize_to_tray });
    save_config(config_path, &config)?;
    load_config_to_cache(&config)?;

    Ok(())
}

#[cfg_attr(feature = "desktop", tauri::command)]
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
    let removed = {
        let _write = config_write_lock();
        let mut config = load_config(config_path)?;
        log::debug!("Loaded configuration: {:?}", config);
        let removed = config.remove_card(card_number).is_some();
        if removed {
            save_config(config_path, &config)?;
            log::debug!("Configuration saved successfully after removal");
            load_config_to_cache(&config)?;
            log::debug!("Configuration loaded to cache successfully");
        }
        removed
    };

    if removed {
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

#[cfg_attr(feature = "desktop", tauri::command)]
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

#[cfg(feature = "desktop")]
#[tauri::command]
pub fn update_appearance(minimize_to_tray: bool) -> bool {
    let config_path = match get_config_path() {
        Ok(path) => path,
        Err(e) => {
            log::error!("Failed to get config path: {}", e);
            return false;
        }
    };
    log::info!("Updating appearance configuration: minimize_to_tray={}", minimize_to_tray);
    match update_appearance_config(&config_path, minimize_to_tray) {
        Ok(_) => {
            log::info!("The appearance configuration is updated: minimize_to_tray={}", minimize_to_tray);
            true
        }
        Err(e) => {
            log::error!("Failed to update appearance configuration: {}", e);
            false
        }
    }
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn update_download(download_path: String, retain_failed_downloads: bool) -> bool {
    let config_path = match get_config_path() {
        Ok(path) => path,
        Err(e) => {
            log::error!("Failed to get config path: {}", e);
            return false;
        }
    };
    log::info!("Updating download configuration: download_path={}, retain_failed_downloads={}", download_path, retain_failed_downloads);
    match update_download_config(&config_path, if download_path.trim().is_empty() { default_download_path() } else { download_path.trim().to_string() }, retain_failed_downloads) {
        Ok(_) => {
            log::info!("The download configuration is updated: download_path={}, retain_failed_downloads={}", download_path, retain_failed_downloads);
            true
        }
        Err(e) => {
            log::error!("Failed to update download configuration: {}", e);
            false
        }
    }
}

#[cfg_attr(feature = "desktop", tauri::command)]
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
    pub companies: BTreeMap<String, CompanyConfig>,
    /// IMEI → company id; `None` for an IMEI listed under more than one company,
    /// which is treated as unregistered. Built in `load_config_to_cache`.
    pub imei_index: HashMap<String, Option<String>>,
}

impl CacheConfigData {
    /// The only company, if there's exactly one: every device and card belongs to it.
    /// With more than one, a device's company comes from the IMEI registry.
    fn sole_company(&self) -> Option<String> {
        (self.companies.len() == 1).then(|| self.companies.keys().next().cloned()).flatten()
    }
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

/// The company a device belongs to: the only company if there's just one, otherwise
/// its IMEI's entry in the registry. `None` means unknown (unregistered, or listed
/// under two companies). Use through `company_directory`, not directly.
pub fn company_for_imei(imei: &str) -> Option<String> {
    let cache = CACHE.lock().unwrap();
    if let Some(company) = cache.sole_company() {
        return Some(company);
    }
    cache.imei_index.get(imei.trim()).cloned().flatten()
}

/// The company whose pool a card belongs in: the company it's stored under, or the only
/// company if there's just one. `card_number` may be empty (an ICCID not
/// configured yet). `None` means unassigned: the card isn't pooled.
pub fn company_for_card(card_number: &str) -> Option<String> {
    let cache = CACHE.lock().unwrap();
    let assigned = cache.cards.get(card_number).and_then(|card| card.company.clone());
    match assigned {
        Some(company) if cache.companies.contains_key(&company) => Some(company),
        Some(company) => {
            log::warn!("[CONFIG] card {} is assigned to unknown company {}", card_number, company);
            None
        }
        None => cache.sole_company(),
    }
}

/// A company's settings, if it exists.
pub fn get_company(company_id: &str) -> Option<CompanyConfig> {
    CACHE.lock().unwrap().companies.get(company_id).cloned()
}

/// Ids of every configured company.
pub fn company_ids() -> Vec<String> {
    CACHE.lock().unwrap().companies.keys().cloned().collect()
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

    let mut imei_index: HashMap<String, Option<String>> = HashMap::new();
    for (company_id, company) in &config.companies {
        for imei in &company.devices {
            let imei = imei.trim().to_string();
            match imei_index.get(&imei) {
                None => {
                    imei_index.insert(imei, Some(company_id.clone()));
                }
                Some(Some(other)) if other != company_id => {
                    log::error!(
                        "[CONFIG] IMEI {} is listed under companies {} and {}; treating it as unregistered",
                        imei, other, company_id
                    );
                    imei_index.insert(imei, None);
                }
                Some(_) => {} // repeated within one company, or already marked duplicate
            }
        }
    }

    // Settings pinned by environment variables replace config.yaml's in the cache, which
    // is what everything reads them from.
    let mut connection = config.connection.clone();
    if let Some(port) = pinned_listen_port() {
        connection
            .get_or_insert(ConnectionConfig {
                listen_port: port,
                connection_limit: DEFAULT_SERVER_CONNECTION_LIMIT,
            })
            .listen_port = port;
    }
    let mut download = config.download.clone();
    if let Some(path) = pinned_download_path() {
        match download.as_mut() {
            Some(download) => download.download_path = path,
            None => download = Some(DownloadConfig { download_path: path, retain_failed_downloads: false }),
        }
    }

    let mut cache = CACHE.lock().unwrap();
    *cache = CacheConfigData {
        cards: config.all_cards(),
        connection,
        appearance: config.appearance.clone(),
        logging: config.logging.clone(),
        download,
        companies: config.companies.clone(),
        imei_index,
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
            retain_failed_downloads: false,
        });
    }

    if config.connection.is_none() {
        config.connection = Some(ConnectionConfig {
            listen_port: 7878,
            connection_limit: 100,
        });
    }

    // First run (or every company removed by hand): create the `default` company, which
    // makes this a single-company install (see `DEFAULT_COMPANY_ID`).
    if config.companies.is_empty() {
        let connection_limit = config
            .connection
            .as_ref()
            .map(|c| c.connection_limit)
            .unwrap_or(DEFAULT_SERVER_CONNECTION_LIMIT);
        println!("No companies configured; creating company '{}'.", DEFAULT_COMPANY_ID);
        config.companies.insert(
            DEFAULT_COMPANY_ID.to_string(),
            CompanyConfig {
                name: DEFAULT_COMPANY_NAME.to_string(),
                connection_limit,
                download_interval_days: DEFAULT_DOWNLOAD_INTERVAL_DAYS,
                devices: Vec::new(),
                cards: BTreeMap::new(),
            },
        );
    }

    // Drop companies with unusable ids (their cards become unassigned), and correct
    // hand-edited intervals above the regulatory cap so config.yaml and the UI show
    // what's actually enforced. No logger yet, hence eprintln!.
    config.companies.retain(|id, _| {
        let valid = is_valid_company_id(id);
        if !valid {
            eprintln!(
                "Company id '{}' is invalid (use lowercase letters, digits and '-'); ignoring it.",
                id
            );
        }
        valid
    });
    for (id, company) in config.companies.iter_mut() {
        if company.download_interval_days > MAX_DOWNLOAD_INTERVAL_DAYS {
            eprintln!(
                "Company '{}': download_interval_days {} exceeds the maximum of {}; using {}.",
                id, company.download_interval_days, MAX_DOWNLOAD_INTERVAL_DAYS, MAX_DOWNLOAD_INTERVAL_DAYS
            );
            company.download_interval_days = MAX_DOWNLOAD_INTERVAL_DAYS;
        }
    }

    // Built-in admin account: config.yaml holds its password (or, once changed in the
    // UI, its hash). Missing or blank means generate a new one — which is also how a
    // lost admin password is reset.
    let admin_password = match &config.admin_password {
        Some(password) if !password.trim().is_empty() => password.clone(),
        _ => {
            let password = crate::users::generate_password();
            println!("No admin password in config.yaml; generated a new one and saved it there.");
            config.admin_password = Some(password.clone());
            password
        }
    };

    save_config(&config_path, &config)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    log::debug!("config: saved config");

    // Only once the password is safely in config.yaml: if this failed first, nobody
    // would know the new password. A failure here is retried on the next startup.
    if let Err(e) = crate::users::ensure_admin(&admin_password) {
        eprintln!("Failed to set the admin password in the database: {}", e);
    }

    /*
        Send data of all cards in events one by one to the front.
    */
    for (card_number, card_config) in config.all_cards() {
        emit_card_config_event("global-card-config-updated", card_number, Some(card_config));
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
        admin_password: None, // generated by init_config()
        appearance: Some(AppearanceConfig { minimize_to_tray: true }),
        connection: Some(ConnectionConfig { listen_port: 7878, connection_limit: 100 }),
        download: Some(DownloadConfig { download_path: default_download_path(), retain_failed_downloads: false }),
        logging: Some(LoggingConfig { retention_days: DEFAULT_LOG_RETENTION_DAYS }),
        // init_config() adds the `default` company.
        companies: BTreeMap::new(),
        unassigned_cards: BTreeMap::new(),
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
        let mut cards = BTreeMap::new();
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
                company: None,
            },
        );
        let mut companies = BTreeMap::new();
        companies.insert(
            "acme".to_string(),
            CompanyConfig {
                name: "Acme Logistics".to_string(),
                connection_limit: 50,
                download_interval_days: 7,
                devices: vec!["356938035643809".to_string()],
                cards,
            },
        );
        ConfigurationFile {
            name: "TCS".to_string(),
            version: "0.0.0-test".to_string(),
            description: "test".to_string(),
            admin_password: Some("test-password".to_string()),
            appearance: Some(AppearanceConfig { minimize_to_tray: true }),
            connection: Some(ConnectionConfig {
                listen_port: 7878,
                connection_limit: 100,
            }),
            download: Some(DownloadConfig {
                download_path: default_download_path(),
                retain_failed_downloads: false,
            }),
            logging: Some(LoggingConfig { retention_days: 7 }),
            companies,
            unassigned_cards: BTreeMap::new(),
        }
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = unique_tmp_dir("roundtrip");
        let path = dir.join("config.yaml");

        let cfg = sample_config();
        save_config(&path, &cfg).expect("save");

        let loaded = load_config(&path).expect("load");
        let cards = loaded.all_cards();
        assert_eq!(cards.len(), 1);
        let card = cards.get("ABCDEF0123456789").expect("card present");
        assert_eq!(card.iccid, "1122334455667788");
        assert_eq!(card.card_type, Some(4));
        assert_eq!(card.last_auth, Some((1_700_000_500, true)));
        assert_eq!(card.company.as_deref(), Some("acme"));
        let acme = loaded.companies.get("acme").expect("company present");
        assert_eq!(acme.connection_limit, 50);
        assert_eq!(acme.download_interval_days, 7);
        assert_eq!(acme.devices, vec!["356938035643809".to_string()]);
        assert_eq!(
            loaded.connection.as_ref().map(|s| s.listen_port),
            cfg.connection.as_ref().map(|s| s.listen_port)
        );
        assert_eq!(
            loaded.connection.as_ref().map(|s| s.connection_limit),
            cfg.connection.as_ref().map(|s| s.connection_limit)
        );

        let _ = fs::remove_dir_all(&dir);
    }

    fn card(iccid: &str) -> CardConfig {
        CardConfig {
            iccid: iccid.to_string(),
            expire: None,
            name: None,
            card_type: Some(4),
            structure_version: None,
            company_name: None,
            company_address: None,
            last_auth: None,
            company: None,
        }
    }

    #[test]
    fn cards_are_nested_under_their_company_on_disk() {
        let yaml = serde_yaml::to_string(&sample_config()).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        assert!(parsed["companies"]["acme"]["cards"]["ABCDEF0123456789"].is_mapping());
        // Company is implied by position, not repeated on the card.
        assert!(parsed["companies"]["acme"]["cards"]["ABCDEF0123456789"].get("company").is_none());
    }

    #[test]
    fn new_cards_go_to_the_only_company() {
        let mut cfg = generate_default_config();
        cfg.companies.insert(
            DEFAULT_COMPANY_ID.to_string(),
            CompanyConfig {
                name: "Default".to_string(),
                connection_limit: 10,
                download_interval_days: 1,
                devices: Vec::new(),
                cards: BTreeMap::new(),
            },
        );
        cfg.insert_card("A", card("1"));
        assert_eq!(cfg.card("A").unwrap().company.as_deref(), Some(DEFAULT_COMPANY_ID));
        assert!(cfg.remove_card("A").is_some());
        assert!(cfg.card("A").is_none());
    }

    #[test]
    fn company_ids_are_validated() {
        assert!(is_valid_company_id("acme"));
        assert!(is_valid_company_id("acme-logistics-2"));
        assert!(!is_valid_company_id(""));
        assert!(!is_valid_company_id("-acme"));
        assert!(!is_valid_company_id("Acme"));
        assert!(!is_valid_company_id("acme/../x"));
        assert!(!is_valid_company_id("acme logistics"));
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
        second.companies.values_mut().for_each(|company| company.cards.clear());
        save_config(&path, &second).expect("second save");

        let loaded = load_config(&path).expect("load second");
        assert!(loaded.all_cards().is_empty(), "second save must replace contents");
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
        assert!(cfg.all_cards().is_empty());
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
            company: None,
        };
        let yaml = serde_yaml::to_string(&card).expect("ser");
        let back: CardConfig = serde_yaml::from_str(&yaml).expect("de");
        assert_eq!(back.iccid, "1234567890123456");
        assert!(back.expire.is_none());
        assert!(back.last_auth.is_none());
    }
}

// ───── Companies, cards and devices: frontend commands ─────

/// A configured card, as listed in the UI.
#[derive(Serialize, Clone, Debug)]
pub struct CardEntry {
    pub card_number: String,
    #[serde(flatten)]
    pub card: CardConfig,
}

#[derive(Serialize, Clone, Debug)]
pub struct CompanyView {
    pub id: String,
    pub name: String,
    pub connection_limit: u16,
    pub download_interval_days: u32,
    pub devices: Vec<String>,
    pub cards: Vec<CardEntry>,
}

#[derive(Serialize, Clone, Debug)]
pub struct CompaniesView {
    pub companies: Vec<CompanyView>,
    pub unassigned_cards: Vec<CardEntry>,
}

fn card_entries<'a>(cards: impl Iterator<Item = (&'a String, &'a CardConfig)>) -> Vec<CardEntry> {
    let mut entries: Vec<CardEntry> = cards
        .map(|(card_number, card)| CardEntry { card_number: card_number.clone(), card: card.clone() })
        .collect();
    entries.sort_by(|a, b| a.card_number.cmp(&b.card_number));
    entries
}

/// Loads config.yaml, applies `change`, then saves it and refreshes the cache — all
/// under the write lock. Errors are strings for the UI to show.
fn change_config<T>(change: impl FnOnce(&mut ConfigurationFile) -> Result<T, String>) -> Result<T, String> {
    let path = get_config_path().map_err(|e| format!("Failed to find config.yaml: {}", e))?;
    let _write = config_write_lock();
    let mut config = load_config(&path).map_err(|e| format!("Failed to read config.yaml: {}", e))?;
    let result = change(&mut config)?;
    save_config(&path, &config).map_err(|e| format!("Failed to save config.yaml: {}", e))?;
    load_config_to_cache(&config).map_err(|e| format!("Failed to apply config: {}", e))?;
    Ok(result)
}

/// Replaces `admin_password` in config.yaml, e.g. with the hash of a password changed in
/// the UI. Blocking.
#[cfg(not(feature = "desktop"))] // changed from the web UI's account menu
pub fn set_admin_password(value: String) -> Result<(), String> {
    change_config(|config| {
        config.admin_password = Some(value);
        Ok(())
    })
}

/// Every company with its devices and cards, plus cards not assigned to any company.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_companies() -> CompaniesView {
    let cache = CACHE.lock().unwrap();
    let companies = cache
        .companies
        .iter()
        .map(|(id, company)| CompanyView {
            id: id.clone(),
            name: company.name.clone(),
            connection_limit: company.connection_limit,
            download_interval_days: company.download_interval_days,
            devices: company.devices.clone(),
            cards: card_entries(cache.cards.iter().filter(|(_, card)| card.company.as_deref() == Some(id))),
        })
        .collect();
    CompaniesView {
        companies,
        unassigned_cards: card_entries(cache.cards.iter().filter(|(_, card)| card.company.is_none())),
    }
}

/// Result of `save_company`.
#[derive(Serialize, Clone, Debug)]
pub struct SaveCompanyResult {
    /// Devices registered to the existing company automatically because this created
    /// the second company (see `save_company`).
    pub auto_registered_devices: usize,
}

/// Creates (`create = true`) or edits a company. Creating the second company ends
/// "every device belongs to the only company", so that company's devices — every IMEI
/// in its download history — are registered to it first, rather than suddenly being
/// rejected.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn save_company(
    id: String,
    name: String,
    connection_limit: u16,
    download_interval_days: u32,
    create: bool,
) -> Result<SaveCompanyResult, String> {
    let id = id.trim().to_string();
    let name = name.trim().to_string();
    if !is_valid_company_id(&id) {
        return Err("The id may only contain lowercase letters, digits and '-', and can't start with '-'.".into());
    }
    if name.is_empty() {
        return Err("Enter a name.".into());
    }
    if connection_limit == 0 {
        return Err("The connection limit must be at least 1.".into());
    }
    if download_interval_days > MAX_DOWNLOAD_INTERVAL_DAYS {
        return Err(format!("The download interval can be at most {} days.", MAX_DOWNLOAD_INTERVAL_DAYS));
    }

    let result = change_config(|config| {
        let mut auto_registered_devices = 0;
        if create {
            if config.companies.contains_key(&id) {
                return Err(format!("A company with id '{}' already exists.", id));
            }
            if let Some(sole_id) = config.sole_company() {
                let sole = config.companies.get_mut(&sole_id).expect("sole company exists");
                for imei in crate::database::imeis_with_downloads_blocking(&sole_id) {
                    if !sole.devices.contains(&imei) {
                        sole.devices.push(imei);
                        auto_registered_devices += 1;
                    }
                }
            }
            config.companies.insert(
                id.clone(),
                CompanyConfig {
                    name: name.clone(),
                    connection_limit,
                    download_interval_days,
                    devices: Vec::new(),
                    cards: BTreeMap::new(),
                },
            );
        } else {
            let company = config
                .companies
                .get_mut(&id)
                .ok_or_else(|| format!("No company with id '{}'.", id))?;
            company.name = name.clone();
            company.connection_limit = connection_limit;
            company.download_interval_days = download_interval_days;
        }
        Ok(SaveCompanyResult { auto_registered_devices })
    })?;
    log::info!(
        "[CONFIG] company={} action={} name={:?} connection_limit={} download_interval_days={} auto_registered_devices={}",
        id,
        if create { "created" } else { "updated" },
        name,
        connection_limit,
        download_interval_days,
        result.auto_registered_devices
    );
    Ok(result)
}

/// Deletes a company. Its cards become unassigned and its devices unknown. The last
/// company can't be deleted.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn delete_company(id: String) -> Result<(), String> {
    let moved_cards = change_config(|config| {
        if config.companies.len() <= 1 {
            return Err("The last company can't be deleted.".into());
        }
        let company = config
            .companies
            .remove(&id)
            .ok_or_else(|| format!("No company with id '{}'.", id))?;
        let moved: Vec<(String, String)> = company
            .cards
            .iter()
            .map(|(card_number, card)| (card_number.clone(), card.iccid.clone()))
            .collect();
        config.unassigned_cards.extend(company.cards);
        Ok(moved)
    })?;
    // Cards in a reader follow (to the remaining company if only one is left).
    for (card_number, iccid) in &moved_cards {
        crate::card_pool::CARD_POOLS.move_card(iccid, company_for_card(card_number).as_deref());
        emit_card_config_event("global-card-config-updated", card_number.clone(), get_card_config_from_cache(card_number));
    }
    log::info!("[CONFIG] company={} action=deleted cards_unassigned={}", id, moved_cards.len());
    Ok(())
}

/// Moves a configured card to `company`, or to unassigned for `None`. A card in a reader
/// follows straight away if it's idle, or when its current session ends.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn assign_card(card_number: String, company: Option<String>) -> Result<(), String> {
    let iccid = change_config(|config| {
        if let Some(company) = &company {
            if !config.companies.contains_key(company) {
                return Err(format!("No company with id '{}'.", company));
            }
        }
        let card = config
            .remove_card(&card_number)
            .ok_or_else(|| format!("No card {}.", card_number))?;
        let iccid = card.iccid.clone();
        match &company {
            Some(company) => {
                config.companies.get_mut(company).expect("checked above").cards.insert(card_number.clone(), card);
            }
            None => {
                config.unassigned_cards.insert(card_number.clone(), card);
            }
        }
        Ok(iccid)
    })?;
    if !iccid.is_empty() {
        crate::card_pool::CARD_POOLS.move_card(&iccid, company_for_card(&card_number).as_deref());
    }
    emit_card_config_event("global-card-config-updated", card_number.clone(), get_card_config_from_cache(&card_number));
    log::info!("[CONFIG] card={} action=assigned company={:?}", card_number, company);
    Ok(())
}

/// Registers a device (IMEI) to `company`, moving it from any other, or unregisters it
/// for `None`. Already-connected devices keep their current session.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn assign_device(imei: String, company: Option<String>) -> Result<(), String> {
    let imei = imei.trim().to_string();
    if imei.len() != 15 || !imei.chars().all(|c| c.is_ascii_digit()) || !luhn::valid(&imei) {
        return Err(format!("'{}' isn't a valid IMEI (15 digits with a valid check digit).", imei));
    }
    change_config(|config| {
        if let Some(company) = &company {
            if !config.companies.contains_key(company) {
                return Err(format!("No company with id '{}'.", company));
            }
        }
        for existing in config.companies.values_mut() {
            existing.devices.retain(|device| device.trim() != imei);
        }
        if let Some(company) = &company {
            config.companies.get_mut(company).expect("checked above").devices.push(imei.clone());
        }
        Ok(())
    })?;
    if company.is_some() {
        crate::database::forget_unknown_device_blocking(&imei);
    }
    log::info!("[CONFIG] imei={} action=assigned company={:?}", imei, company);
    Ok(())
}

/// A registered device.
#[derive(Serialize, Clone, Debug)]
pub struct RegisteredDevice {
    pub imei: String,
    pub company: String,
    /// Vehicle registration from its latest verified download; `None` until it has one.
    pub vrn: Option<String>,
    /// Unix seconds of its latest authentication attempt, and whether it succeeded.
    pub last_auth_at: Option<i64>,
    pub last_auth_succeeded: Option<bool>,
    /// Unix seconds of its latest verified download.
    pub last_download_at: Option<i64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct DevicesView {
    /// Set when there's only one company: every device belongs to it, registered or not.
    pub sole_company: Option<String>,
    pub registered: Vec<RegisteredDevice>,
    /// Devices turned away because their company isn't known, most recent first.
    pub unknown: Vec<crate::database::UnknownDevice>,
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_devices() -> DevicesView {
    let (sole_company, mut registered) = {
        let cache = CACHE.lock().unwrap();
        let registered: Vec<RegisteredDevice> = cache
            .companies
            .iter()
            .flat_map(|(id, company)| {
                company.devices.iter().map(|imei| RegisteredDevice {
                    imei: imei.trim().to_string(),
                    company: id.clone(),
                    vrn: None,
                    last_auth_at: None,
                    last_auth_succeeded: None,
                    last_download_at: None,
                })
            })
            .collect();
        (cache.sole_company(), registered)
    };
    registered.sort_by(|a, b| a.company.cmp(&b.company).then(a.imei.cmp(&b.imei)));
    let vrns = crate::database::latest_vrns_blocking();
    let unknown = crate::database::unknown_devices_blocking()
        .into_iter()
        .filter(|device| !registered.iter().any(|r| r.imei == device.imei))
        .map(|device| crate::database::UnknownDevice { vrn: vrns.get(&device.imei).cloned(), ..device })
        .collect();
    let auths = crate::database::device_auths_blocking();
    let downloads = crate::database::last_downloads_blocking();
    for device in &mut registered {
        device.vrn = vrns.get(&device.imei).cloned();
        let auth = auths.get(&device.imei);
        device.last_auth_at = auth.map(|&(at, _)| at);
        device.last_auth_succeeded = auth.map(|&(_, succeeded)| succeeded);
        device.last_download_at = downloads.get(&device.imei).copied();
    }
    DevicesView { sole_company, registered, unknown }
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
        ("retain_failed_downloads", get_from_cache(CacheSection::Download, "retain_failed_downloads")),
        ("retention_days", get_from_cache(CacheSection::Logging, "retention_days")),
        ("minimize_to_tray", get_from_cache(CacheSection::Appearance, "minimize_to_tray")),
        // Name of the environment variable pinning the setting, or empty if it isn't.
        ("listen_port_env", pinned_listen_port().map_or("", |_| LISTEN_PORT_ENV).to_string()),
        ("download_path_env", pinned_download_path().map_or("", |_| DOWNLOAD_PATH_ENV).to_string()),
    ])
}

/// Current settings, for a config section to load when it's created — the dialog's
/// contents only exist while it's open, so they miss the startup `global-config-*`
/// events.
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_config() -> HashMap<&'static str, String> {
    config_payload()
}

pub fn emit_global_config_connection() {
    let payload: HashMap<&'static str, String> = HashMap::from([
        ("listen_port", get_from_cache(CacheSection::Connection, "listen_port")),
        ("connection_limit", get_from_cache(CacheSection::Connection, "connection_limit")),
    ]);
    emit("global-config-connection", payload);
}

pub fn emit_global_config_download() {
    let payload: HashMap<&'static str, String> = HashMap::from([
        ("download_path", get_from_cache(CacheSection::Download, "download_path")),
        ("retain_failed_downloads", get_from_cache(CacheSection::Download, "retain_failed_downloads")),
    ]);
    emit("global-config-download", payload);
}

pub fn emit_global_config_logging() {
    let payload: HashMap<&'static str, String> = HashMap::from([
        ("retention_days", get_from_cache(CacheSection::Logging, "retention_days")),
    ]);
    emit("global-config-logging", payload);
}

pub fn emit_global_config_appearance() {
    let payload: HashMap<&'static str, String> = HashMap::from([
        ("minimize_to_tray", get_from_cache(CacheSection::Appearance, "minimize_to_tray")),
    ]);
    emit("global-config-appearance", payload);
}