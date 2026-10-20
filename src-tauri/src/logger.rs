use std::env;
use std::path::PathBuf;

use flexi_logger::{Age, Cleanup, Criterion, Duplicate, FileSpec, Logger, LoggerHandle, Naming};
use once_cell::sync::OnceCell;
use reqwest;
use serde::Deserialize;

use crate::global_app_handle::emit_notification_event;
use crate::global_app_handle::NotificationPayload;

#[derive(Deserialize, Debug)]
struct Release {
    tag_name: String,
}

/// Keeps the flexi_logger handle alive for the program's lifetime — dropping it
/// flushes and shuts down the file writer, so it must live at least as long as
/// logging is needed (in practice, forever — nothing ever clears this).
static LOGGER_HANDLE: OnceCell<LoggerHandle> = OnceCell::new();

/// `<data dir>/logs` — see `config::get_data_dir()` (follows `--config`).
fn log_directory() -> PathBuf {
    match crate::config::get_data_dir() {
        Ok(dir) => dir.join("logs"),
        Err(e) => {
            eprintln!("Failed to resolve data directory ({}). Logging to current directory.", e);
            PathBuf::from(".").join("tcs-logs").join("logs")
        }
    }
}

/// Opens the log directory in the OS file manager. Desktop only.
#[cfg(feature = "desktop")]
#[tauri::command]
pub fn open_logs_folder() -> Result<(), String> {
    let log_dir = log_directory();
    std::fs::create_dir_all(&log_dir).map_err(|e| {
        log::error!("Failed to create log directory {:?}: {}", log_dir, e);
        format!("Failed to create log directory: {}", e)
    })?;
    tauri_plugin_opener::open_path(&log_dir, None::<&str>).map_err(|e| {
        log::error!("Failed to open log directory {:?}: {}", log_dir, e);
        format!("Failed to open log directory: {}", e)
    })
}

/// Sets up logging: a fresh log file every time the process starts, an
/// additional rollover once per calendar day if it keeps running that long,
/// older rotated files compressed, and anything past the retention window
/// deleted. Everything is also duplicated to stdout, so running interactively
/// (or the server build, which has a console) still shows live output
/// — tailing the file and watching the console both work.
///
/// Must be called *after* `config::init_config()` — it reads
/// `log_retention_days` from the config cache (falling back to
/// `config::DEFAULT_LOG_RETENTION_DAYS` if that cache isn't populated yet, so
/// a config load failure here is degraded, not fatal).
pub fn setup_logging() {
    // Only one global logger can ever be installed; a second `start()` fails with
    // "logger already initialized", which must not be reported as an access error.
    if LOGGER_HANDLE.get().is_some() {
        return;
    }

    let log_dir = log_directory();
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        eprintln!("Failed to create log directory {:?}: {}", log_dir, e);
        return;
    }

    let retention_days: usize = crate::config::get_from_cache(crate::config::CacheSection::Logging, "retention_days")
        .parse()
        .unwrap_or(crate::config::DEFAULT_LOG_RETENTION_DAYS as usize);

    let logger_result = Logger::try_with_str("debug")
        .map(|logger| {
            logger
                .log_to_file(FileSpec::default().directory(&log_dir).basename("tacho-card-server"))
                .rotate(
                    Criterion::Age(Age::Day),
                    Naming::Timestamps,
                    Cleanup::KeepCompressedFiles(retention_days),
                )
                .duplicate_to_stdout(Duplicate::All)
                .format(flexi_logger::opt_format)
        })
        .and_then(|logger| logger.start());

    match logger_result {
        Ok(handle) => {
            let _ = LOGGER_HANDLE.set(handle);
        }
        Err(e) => {
            eprintln!("Failed to initialize logging at {:?}: {}", log_dir, e);

            let payload = NotificationPayload {
                notification_type: "access".to_string(),
                message: "No permission to write log file".to_string(),
            };
            emit_notification_event("global-notification", payload);

            return;
        }
    }

    log::info!("-== Application is launched ==-");
    log::info!("Log directory: {:?}, retention: {} day(s)", log_dir, retention_days);
    match crate::config::get_config_path() {
        Ok(path) => log::info!("Config file: {:?}", path),
        Err(e) => log::error!("Failed to resolve config file path: {}", e),
    }

    // Check for the latest version asynchronously
    crate::rt::spawn(async {
        if let Err(e) = check_latest_version().await {
            log::error!("Error checking latest version: {}", e);
        }
    });

    // Log system information
    log_system_info();
}

fn log_system_info() {
    let os_type = sys_info::os_type().unwrap_or_else(|_| "Unknown".to_string());
    let os_release = sys_info::os_release().unwrap_or_else(|_| "Unknown".to_string());
    let hostname = sys_info::hostname().unwrap_or_else(|_| "Unknown".to_string());
    let cpu_num = sys_info::cpu_num().unwrap_or_else(|_| 0);
    let cpu_speed = sys_info::cpu_speed().map_or_else(|_| "Unknown".to_string(), |speed| format!("{} MHz", speed));
    let mem_info = sys_info::mem_info().map_or_else(|_| "Unknown".to_string(), |mem| format!("total {} KB, free {} KB", mem.total, mem.free));

    log::info!(
        "OS Type: {}, OS Release: {}, Hostname: {}, Number of CPUs: {} ({}), Memory: {}",
        os_type, os_release, hostname, cpu_num, cpu_speed, mem_info
    );
}

async fn check_latest_version() -> Result<(), reqwest::Error> {
    let url = "https://api.github.com/repos/astra-telematics/Tacho-Card-Server/releases/latest";
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header("User-Agent", "reqwest")
        .send()
        .await?;

    if response.status().is_success() {
        let release: Release = response.json().await?;
        // log::info!("Latest release info: {:?}", release);

        let latest_version = release.tag_name;
        let current_version = env!("CARGO_PKG_VERSION");

        let latest_version_num = version_to_number(&latest_version);
        let current_version_num = version_to_number(current_version);

        if current_version_num > latest_version_num {
            log::info!(
                "Version (current: {}, latest: {})",
                current_version,
                latest_version
            );
        } else if current_version_num < latest_version_num {
            log::info!(
                "Version (current: {}, latest: {}). New one is available, use the link to download: {}",
                current_version,
                latest_version,
                url
            );

            let payload = NotificationPayload {
                notification_type: "version".to_string(),
                message: format!("New version {} is available, use the link to download: {}", latest_version, url).into(),
            };
            emit_notification_event("global-notification", payload);
        } else {
            log::info!(
                "Version (current: {}, latest: {}). You are using the latest version.",
                current_version,
                latest_version
            );
        }
    } else {
        log::warn!("Version. Failed to fetch the latest release info: {}", response.status());
    }

    Ok(())
}

fn version_to_number(version: &str) -> u32 {
    version
        .trim_start_matches('v')
        .split('.')
        .filter_map(|s| s.parse::<u32>().ok())
        .fold(0, |acc, num| acc * 100 + num)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_to_number_strips_v_prefix_and_packs_components() {
        // 0.7.2 → 0*10000 + 7*100 + 2 = 702
        assert_eq!(version_to_number("0.7.2"), 702);
        assert_eq!(version_to_number("v0.7.2"), 702);
        assert_eq!(version_to_number("1.0.0"), 10000);
    }

    #[test]
    fn version_to_number_orders_versions_correctly() {
        assert!(version_to_number("v0.7.3") > version_to_number("v0.7.2"));
        assert!(version_to_number("v0.8.0") > version_to_number("v0.7.99"));
        assert!(version_to_number("v1.0.0") > version_to_number("v0.99.99"));
    }

    #[test]
    fn version_to_number_handles_garbage_components() {
        // Non-numeric chunks are skipped silently.
        assert_eq!(version_to_number("v1.x.3"), 103);
    }
}
