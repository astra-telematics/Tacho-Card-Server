//! SQLite record of completed VU downloads, stored as `tcs.db` in the data directory
//! (next to config.yaml, so it follows `--config`).
//!
//! Used to enforce a minimum interval between downloads from the same device
//! (`server.download_interval_days` in config.yaml; 1 = once a day, 7 = once a week,
//! 0 = no limit). A device (IMEI) whose last verified download was less than that many
//! days ago is told at `ConnAnnounce` to come back shortly after midnight on the day it
//! becomes eligible (see `socket_server::handle_connection`). Days are the server's
//! local calendar dates. Unverified downloads (CRC mismatch, malformed, incomplete) are
//! recorded for visibility but don't count, so the device can retry straight away.
//!
//! Every failure here is logged and treated as "no download recorded" — a database
//! problem must never stop a device from downloading.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Days, Duration, Local, NaiveDate, TimeZone};
use rand::Rng;
use rusqlite::{params, Connection};

const DB_FILE_NAME: &str = "tcs.db";

/// Bumped (with a matching step in `migrate`) whenever the schema changes.
const SCHEMA_VERSION: i32 = 1;

/// Spread devices that become eligible on the same day over this window after
/// midnight, so they don't all reconnect at 00:00:00 at once.
const MIDNIGHT_JITTER_SECONDS: i64 = 15 * 60;

/// Opened on first use. `None` if opening failed (logged once); every call then
/// behaves as if nothing is recorded.
static DB: OnceLock<Option<Mutex<Connection>>> = OnceLock::new();

fn db_path() -> std::io::Result<PathBuf> {
    Ok(crate::config::get_data_dir()?.join(DB_FILE_NAME))
}

fn open() -> Option<Mutex<Connection>> {
    let path = match db_path() {
        Ok(path) => path,
        Err(e) => {
            log::error!("[DB] status=path_failed err={}", e);
            return None;
        }
    };
    let result = Connection::open(&path).and_then(|mut conn| {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&mut conn)?;
        Ok(conn)
    });
    match result {
        Ok(conn) => {
            log::info!("[DB] status=opened path={:?}", path);
            Some(Mutex::new(conn))
        }
        Err(e) => {
            log::error!("[DB] status=open_failed path={:?} err={}", path, e);
            None
        }
    }
}

fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 1 {
        let tx = conn.transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS downloads (
                id            INTEGER PRIMARY KEY,
                imei          TEXT    NOT NULL,
                vrn           TEXT,
                downloaded_at INTEGER NOT NULL, -- unix seconds (UTC)
                local_date    TEXT    NOT NULL, -- server-local YYYY-MM-DD
                verified      INTEGER NOT NULL, -- 1 = CRC and structure OK
                generation    INTEGER,
                data_types    INTEGER,
                bytes         INTEGER NOT NULL,
                file_path     TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_downloads_imei_date ON downloads (imei, local_date);",
        )?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
    }
    Ok(())
}

fn with_db<T>(default: T, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> T {
    let Some(db) = DB.get_or_init(open) else {
        return default;
    };
    let conn = match db.lock() {
        Ok(conn) => conn,
        Err(poisoned) => poisoned.into_inner(),
    };
    match f(&conn) {
        Ok(value) => value,
        Err(e) => {
            log::error!("[DB] status=query_failed err={}", e);
            default
        }
    }
}

/// A finished (or abandoned) download, as handed to `record_download`.
pub struct DownloadRecord {
    pub imei: String,
    pub vrn: Option<String>,
    pub verified: bool,
    pub generation: Option<u8>,
    pub data_types: Option<u8>,
    pub bytes: usize,
    pub file_path: Option<PathBuf>,
}

fn insert_download(conn: &Connection, record: &DownloadRecord, now: DateTime<Local>) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO downloads
            (imei, vrn, downloaded_at, local_date, verified, generation, data_types, bytes, file_path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            record.imei,
            record.vrn,
            now.timestamp(),
            now.format("%Y-%m-%d").to_string(),
            record.verified,
            record.generation,
            record.data_types,
            record.bytes as i64,
            record.file_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
        ],
    )?;
    Ok(())
}

/// Server-local date of `imei`'s most recent verified download, if any.
fn last_verified_download_date(conn: &Connection, imei: &str) -> rusqlite::Result<Option<NaiveDate>> {
    let date: Option<String> = conn.query_row(
        "SELECT MAX(local_date) FROM downloads WHERE imei = ?1 AND verified = 1",
        params![imei],
        |row| row.get(0),
    )?;
    Ok(date.and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok()))
}

/// The first date a device may download again: `interval_days` after its last verified
/// download (1 = the next day, 7 = the same weekday next week).
fn next_allowed_date(last: NaiveDate, interval_days: u32) -> NaiveDate {
    last.checked_add_days(Days::new(interval_days as u64)).unwrap_or(NaiveDate::MAX)
}

fn record_download_blocking(record: DownloadRecord) {
    with_db((), |conn| insert_download(conn, &record, Local::now()));
}

fn next_allowed_date_blocking(imei: String, interval_days: u32) -> Option<NaiveDate> {
    let last = with_db(None, |conn| last_verified_download_date(conn, &imei))?;
    Some(next_allowed_date(last, interval_days))
}

/// Records a download. Runs on a blocking thread so the connection's task isn't
/// stalled on disk I/O.
pub async fn record_download(record: DownloadRecord) {
    if let Err(e) = tokio::task::spawn_blocking(move || record_download_blocking(record)).await {
        log::error!("[DB] status=record_failed err={:?}", e);
    }
}

/// If `imei` isn't allowed to download yet — its last verified download was less than
/// `interval_days` days ago (server-local dates) — how long until it may. `None` means
/// go ahead: no limit configured (`interval_days == 0`), no verified download on
/// record, or the interval has passed.
pub async fn time_until_next_download(imei: &str, interval_days: u32) -> Option<std::time::Duration> {
    // Belt and braces: config load and save already clamp, but never exceed the cap.
    let interval_days = interval_days.min(crate::config::MAX_DOWNLOAD_INTERVAL_DAYS);
    if interval_days == 0 {
        return None;
    }
    let imei = imei.to_string();
    let next = tokio::task::spawn_blocking(move || next_allowed_date_blocking(imei, interval_days))
        .await
        .unwrap_or_else(|e| {
            log::error!("[DB] status=check_failed err={:?}", e);
            None
        })?;
    let now = Local::now();
    if now.date_naive() >= next {
        return None;
    }
    let jitter = Duration::seconds(rand::thread_rng().gen_range(0..=MIDNIGHT_JITTER_SECONDS));
    Some(time_until_start_of(next, now, jitter))
}

/// Time from `now` until local midnight at the start of `date`, plus `jitter`. At
/// least one second, so a device is never told to retry immediately.
fn time_until_start_of(date: NaiveDate, now: DateTime<Local>, jitter: Duration) -> std::time::Duration {
    let start = date
        .and_hms_opt(0, 0, 0)
        // `earliest()` handles a DST change at midnight; if midnight doesn't exist
        // locally at all, fall back to counting whole days.
        .and_then(|naive| Local.from_local_datetime(&naive).earliest())
        .unwrap_or_else(|| now + Duration::days((date - now.date_naive()).num_days()));
    (start - now + jitter)
        .to_std()
        .unwrap_or_default()
        .max(std::time::Duration::from_secs(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        migrate(&mut conn).expect("migrate");
        conn
    }

    fn record(imei: &str, verified: bool) -> DownloadRecord {
        DownloadRecord {
            imei: imei.to_string(),
            vrn: Some("AB12CDE".to_string()),
            verified,
            generation: Some(2),
            data_types: Some(0x01),
            bytes: 1234,
            file_path: None,
        }
    }

    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).earliest().unwrap()
    }

    #[test]
    fn last_verified_download_is_the_latest_verified_one() {
        let conn = test_db();
        insert_download(&conn, &record("356938035643809", true), local(2026, 9, 20, 10, 0)).unwrap();
        insert_download(&conn, &record("356938035643809", true), local(2026, 9, 23, 10, 0)).unwrap();
        // Later, but unverified — must not count.
        insert_download(&conn, &record("356938035643809", false), local(2026, 9, 25, 10, 0)).unwrap();

        assert_eq!(
            last_verified_download_date(&conn, "356938035643809").unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 23)
        );
        assert_eq!(last_verified_download_date(&conn, "490154203237518").unwrap(), None);
    }

    #[test]
    fn next_allowed_date_adds_whole_days() {
        let wed = NaiveDate::from_ymd_opt(2026, 9, 23).unwrap();
        assert_eq!(next_allowed_date(wed, 1), NaiveDate::from_ymd_opt(2026, 9, 24).unwrap());
        assert_eq!(next_allowed_date(wed, 7), NaiveDate::from_ymd_opt(2026, 9, 30).unwrap());
    }

    #[test]
    fn time_until_start_of_counts_to_local_midnight() {
        let now = local(2026, 9, 25, 22, 0);
        let tomorrow = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        let next_week = NaiveDate::from_ymd_opt(2026, 10, 2).unwrap();
        assert_eq!(time_until_start_of(tomorrow, now, Duration::zero()).as_secs(), 2 * 3600);
        assert_eq!(time_until_start_of(tomorrow, now, Duration::seconds(60)).as_secs(), 2 * 3600 + 60);
        // 7 days minus 22 h, give or take an hour if a DST change falls in between.
        let week = time_until_start_of(next_week, now, Duration::zero()).as_secs() as i64;
        assert!((week - (6 * 86400 + 2 * 3600)).abs() <= 3600, "{week}");
    }

    #[test]
    fn migrate_is_idempotent() {
        let mut conn = test_db();
        migrate(&mut conn).expect("second migrate");
        let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }
}
