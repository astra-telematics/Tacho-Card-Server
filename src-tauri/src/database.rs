//! SQLite database (`tcs.db` in the data directory, next to config.yaml, so it follows
//! `--config`). Holds:
//!
//! - **Downloads**: every VU download received, tagged with the device's company, used
//!   to enforce a minimum interval between downloads from the same device (the
//!   company's `download_interval_days` in config.yaml; 1 = once a day, 7 = once a
//!   week, 0 = no limit). A device (IMEI) whose
//!   last verified download was less than that many days ago may not download again
//!   until the start of the day it becomes eligible; `admission.rs` works out when to
//!   send it back. Days are the server's local calendar dates. Unverified downloads
//!   (CRC mismatch, or incomplete) are recorded for visibility but don't count, so the
//!   device can retry straight away.
//! - **Card sessions**: how long each device held a card, per company, so
//!   `admission.rs`'s averages survive restarts.
//! - **Device auths**: each device's latest authentication attempt and whether it
//!   succeeded, for the Devices page.
//! - **Users**: accounts, their password hashes and their UI themes (see `users.rs`).
//! - **Unknown devices**: devices turned away because their company isn't known (more
//!   than one company, and their IMEI isn't registered), so they can be listed in the UI
//!   and assigned to a company.
//!
//! Every failure here is logged and treated as "nothing recorded" — a database problem
//! must never stop a device from downloading.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Days, Local, NaiveDate, TimeZone};
use rusqlite::{params, Connection};

const DB_FILE_NAME: &str = "tcs.db";

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
        tx.pragma_update(None, "user_version", 1)?;
        tx.commit()?;
    }
    if version < 2 {
        let tx = conn.transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS card_sessions (
                id          INTEGER PRIMARY KEY,
                ended_at    INTEGER NOT NULL, -- unix seconds (UTC)
                duration_ms INTEGER NOT NULL  -- card assigned -> card released
            );",
        )?;
        tx.pragma_update(None, "user_version", 2)?;
        tx.commit()?;
    }
    if version < 3 {
        // Companies. Rows from before them belong to the `default` company, which is
        // what an upgraded config gets too.
        let tx = conn.transaction()?;
        let default = crate::config::DEFAULT_COMPANY_ID;
        tx.execute_batch(&format!(
            "ALTER TABLE downloads ADD COLUMN company TEXT NOT NULL DEFAULT '{default}';
             ALTER TABLE card_sessions ADD COLUMN company TEXT NOT NULL DEFAULT '{default}';
             CREATE INDEX IF NOT EXISTS idx_downloads_company_imei_date
                 ON downloads (company, imei, local_date);"
        ))?;
        tx.pragma_update(None, "user_version", 3)?;
        tx.commit()?;
    }
    if version < 4 {
        let tx = conn.transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS unknown_devices (
                imei             TEXT    PRIMARY KEY,
                first_seen       INTEGER NOT NULL, -- unix seconds (UTC)
                last_seen        INTEGER NOT NULL, -- unix seconds (UTC)
                attempts         INTEGER NOT NULL,
                firmware_version TEXT
            );",
        )?;
        tx.pragma_update(None, "user_version", 4)?;
        tx.commit()?;
    }
    if version < 5 {
        let tx = conn.transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                username      TEXT    PRIMARY KEY,
                password_hash TEXT    NOT NULL, -- Argon2id, PHC string format
                created_at    INTEGER NOT NULL, -- unix seconds (UTC)
                updated_at    INTEGER NOT NULL  -- unix seconds (UTC)
            );",
        )?;
        tx.pragma_update(None, "user_version", 5)?;
        tx.commit()?;
    }
    if version < 6 {
        let tx = conn.transaction()?;
        tx.execute_batch(
            "ALTER TABLE users ADD COLUMN theme TEXT NOT NULL DEFAULT 'Auto'; -- 'Light' | 'Auto' | 'Dark'",
        )?;
        tx.pragma_update(None, "user_version", 6)?;
        tx.commit()?;
    }
    if version < 7 {
        let tx = conn.transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS device_auths (
                imei      TEXT    PRIMARY KEY,
                last_at   INTEGER NOT NULL, -- unix seconds (UTC) of the latest AuthOutcome
                succeeded INTEGER NOT NULL  -- 1 = that authentication succeeded
            );",
        )?;
        tx.pragma_update(None, "user_version", 7)?;
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
    pub company: String,
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
            (company, imei, vrn, downloaded_at, local_date, verified, generation, data_types, bytes, file_path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            record.company,
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

/// Server-local date of `imei`'s most recent verified download for `company`, if any.
fn last_verified_download_date(conn: &Connection, company: &str, imei: &str) -> rusqlite::Result<Option<NaiveDate>> {
    let date: Option<String> = conn.query_row(
        "SELECT MAX(local_date) FROM downloads WHERE company = ?1 AND imei = ?2 AND verified = 1",
        params![company, imei],
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

fn next_allowed_date_blocking(company: String, imei: String, interval_days: u32) -> Option<NaiveDate> {
    let last = with_db(None, |conn| last_verified_download_date(conn, &company, &imei))?;
    Some(next_allowed_date(last, interval_days))
}

/// Records a download. Runs on a blocking thread so the connection's task isn't
/// stalled on disk I/O.
pub async fn record_download(record: DownloadRecord) {
    if let Err(e) = tokio::task::spawn_blocking(move || record_download_blocking(record)).await {
        log::error!("[DB] status=record_failed err={:?}", e);
    }
}

/// If `imei` isn't allowed to download yet — its last verified download for `company`
/// was less than `interval_days` days ago (server-local dates) — the local midnight at
/// the start of the day it may. `None` means go ahead now: no limit configured
/// (`interval_days == 0`), no verified download on record, or the interval has passed.
pub async fn next_download_allowed_at(company: &str, imei: &str, interval_days: u32) -> Option<DateTime<Local>> {
    // Belt and braces: config load and save already clamp, but never exceed the cap.
    let interval_days = interval_days.min(crate::config::MAX_DOWNLOAD_INTERVAL_DAYS);
    if interval_days == 0 {
        return None;
    }
    let (company, imei) = (company.to_string(), imei.to_string());
    let next = tokio::task::spawn_blocking(move || next_allowed_date_blocking(company, imei, interval_days))
        .await
        .unwrap_or_else(|e| {
            log::error!("[DB] status=check_failed err={:?}", e);
            None
        })?;
    let now = Local::now();
    if now.date_naive() >= next {
        return None;
    }
    Some(start_of_day(next))
}

/// Local midnight at the start of `date`. `earliest()` handles a DST change at
/// midnight; if midnight doesn't exist locally at all, 01:00 is used instead.
fn start_of_day(date: NaiveDate) -> DateTime<Local> {
    [0, 1]
        .iter()
        .find_map(|&hour| {
            date.and_hms_opt(hour, 0, 0)
                .and_then(|naive| Local.from_local_datetime(&naive).earliest())
        })
        .unwrap_or_else(Local::now)
}

fn insert_card_session(
    conn: &Connection,
    company: &str,
    duration: std::time::Duration,
    ended_at: DateTime<Local>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO card_sessions (company, ended_at, duration_ms) VALUES (?1, ?2, ?3)",
        params![company, ended_at.timestamp(), duration.as_millis() as i64],
    )?;
    Ok(())
}

/// The most recent `limit` card session durations, oldest first — for one company, or
/// across all of them if `company` is `None`.
fn recent_card_sessions(
    conn: &Connection,
    company: Option<&str>,
    limit: usize,
) -> rusqlite::Result<Vec<std::time::Duration>> {
    let mut stmt = conn.prepare(
        "SELECT duration_ms FROM (
             SELECT id, duration_ms FROM card_sessions
             WHERE ?1 IS NULL OR company = ?1
             ORDER BY id DESC LIMIT ?2
         ) ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![company, limit as i64], |row| row.get::<_, i64>(0))?;
    rows.map(|ms| ms.map(|ms| std::time::Duration::from_millis(ms.max(0) as u64)))
        .collect()
}

/// Records how long one of `company`'s devices held a card. Blocking; call off the
/// async runtime.
pub fn record_card_session_blocking(company: String, duration: std::time::Duration) {
    with_db((), |conn| insert_card_session(conn, &company, duration, Local::now()));
}

/// The most recent `limit` card session durations, oldest first — for one company, or
/// all of them if `company` is `None`. Blocking.
pub fn recent_card_sessions_blocking(company: Option<&str>, limit: usize) -> Vec<std::time::Duration> {
    with_db(Vec::new(), |conn| recent_card_sessions(conn, company, limit))
}

fn user_password_hash(conn: &Connection, username: &str) -> rusqlite::Result<Option<String>> {
    match conn.query_row(
        "SELECT password_hash FROM users WHERE username = ?1",
        params![username],
        |row| row.get(0),
    ) {
        Ok(hash) => Ok(Some(hash)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

fn set_user_password_hash(conn: &Connection, username: &str, password_hash: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO users (username, password_hash, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT(username) DO UPDATE SET password_hash = excluded.password_hash, updated_at = excluded.updated_at",
        params![username, password_hash, now],
    )?;
    Ok(())
}

/// A user's stored password hash, if the user exists. `Err` only if the database can't
/// be read, so callers can tell "no such user" from "couldn't check". Blocking.
pub fn user_password_hash_blocking(username: &str) -> Result<Option<String>, String> {
    let db = DB.get_or_init(open).as_ref().ok_or("database unavailable")?;
    let conn = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    user_password_hash(&conn, username).map_err(|e| e.to_string())
}

/// Creates a user, or replaces an existing user's password hash. Blocking.
pub fn set_user_password_hash_blocking(username: &str, password_hash: &str) -> Result<(), String> {
    let db = DB.get_or_init(open).as_ref().ok_or("database unavailable")?;
    let conn = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    set_user_password_hash(&conn, username, password_hash, Local::now().timestamp()).map_err(|e| e.to_string())
}

fn user_theme(conn: &Connection, username: &str) -> rusqlite::Result<Option<String>> {
    match conn.query_row("SELECT theme FROM users WHERE username = ?1", params![username], |row| row.get(0)) {
        Ok(theme) => Ok(Some(theme)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// A user's theme; `Ok(None)` if there's no such user. Blocking.
pub fn user_theme_blocking(username: &str) -> Result<Option<String>, String> {
    let db = DB.get_or_init(open).as_ref().ok_or("database unavailable")?;
    let conn = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    user_theme(&conn, username).map_err(|e| e.to_string())
}

/// Sets a user's theme; `Ok(false)` if there's no such user. Blocking.
pub fn set_user_theme_blocking(username: &str, theme: &str) -> Result<bool, String> {
    let db = DB.get_or_init(open).as_ref().ok_or("database unavailable")?;
    let conn = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    conn.execute("UPDATE users SET theme = ?2 WHERE username = ?1", params![username, theme])
        .map(|updated| updated == 1)
        .map_err(|e| e.to_string())
}

/// A user account as listed in the UI (no password hash).
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct UserRecord {
    pub username: String,
    /// Unix seconds (UTC).
    pub created_at: i64,
    /// Unix seconds (UTC): when the password was last set.
    pub updated_at: i64,
}

fn users(conn: &Connection) -> rusqlite::Result<Vec<UserRecord>> {
    let mut stmt = conn.prepare("SELECT username, created_at, updated_at FROM users ORDER BY username")?;
    let rows = stmt.query_map([], |row| {
        Ok(UserRecord { username: row.get(0)?, created_at: row.get(1)?, updated_at: row.get(2)? })
    })?;
    rows.collect()
}

/// Inserts a new user. `Ok(false)` if the username is already taken.
fn create_user(conn: &Connection, username: &str, password_hash: &str, now: i64) -> rusqlite::Result<bool> {
    let inserted = conn.execute(
        "INSERT INTO users (username, password_hash, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT(username) DO NOTHING",
        params![username, password_hash, now],
    )?;
    Ok(inserted == 1)
}

/// Replaces an existing user's password hash. `Ok(false)` if there's no such user.
fn update_user_password_hash(conn: &Connection, username: &str, password_hash: &str, now: i64) -> rusqlite::Result<bool> {
    let updated = conn.execute(
        "UPDATE users SET password_hash = ?2, updated_at = ?3 WHERE username = ?1",
        params![username, password_hash, now],
    )?;
    Ok(updated == 1)
}

/// Every user, by username. Blocking.
pub fn users_blocking() -> Result<Vec<UserRecord>, String> {
    let db = DB.get_or_init(open).as_ref().ok_or("database unavailable")?;
    let conn = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    users(&conn).map_err(|e| e.to_string())
}

/// Creates a user; `Ok(false)` if the username is taken. Blocking.
pub fn create_user_blocking(username: &str, password_hash: &str) -> Result<bool, String> {
    let db = DB.get_or_init(open).as_ref().ok_or("database unavailable")?;
    let conn = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    create_user(&conn, username, password_hash, Local::now().timestamp()).map_err(|e| e.to_string())
}

/// Changes an existing user's password hash; `Ok(false)` if there's no such user. Blocking.
pub fn update_user_password_hash_blocking(username: &str, password_hash: &str) -> Result<bool, String> {
    let db = DB.get_or_init(open).as_ref().ok_or("database unavailable")?;
    let conn = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    update_user_password_hash(&conn, username, password_hash, Local::now().timestamp()).map_err(|e| e.to_string())
}

/// Deletes a user; `Ok(false)` if there was no such user. Blocking.
pub fn delete_user_blocking(username: &str) -> Result<bool, String> {
    let db = DB.get_or_init(open).as_ref().ok_or("database unavailable")?;
    let conn = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    conn.execute("DELETE FROM users WHERE username = ?1", params![username])
        .map(|deleted| deleted == 1)
        .map_err(|e| e.to_string())
}

/// A device turned away because its company isn't known.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct UnknownDevice {
    pub imei: String,
    /// Unix seconds (UTC).
    pub first_seen: i64,
    /// Unix seconds (UTC).
    pub last_seen: i64,
    /// How many times it has tried to connect.
    pub attempts: i64,
    pub firmware_version: Option<String>,
    /// Vehicle registration, if it has downloaded before (filled in by
    /// `config::get_devices`).
    pub vrn: Option<String>,
}

fn upsert_unknown_device(conn: &Connection, imei: &str, firmware_version: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO unknown_devices (imei, first_seen, last_seen, attempts, firmware_version)
         VALUES (?1, ?2, ?2, 1, ?3)
         ON CONFLICT(imei) DO UPDATE SET
             last_seen = excluded.last_seen,
             attempts = attempts + 1,
             firmware_version = excluded.firmware_version",
        params![imei, now, firmware_version],
    )?;
    Ok(())
}

fn unknown_devices(conn: &Connection) -> rusqlite::Result<Vec<UnknownDevice>> {
    let mut stmt = conn.prepare(
        "SELECT imei, first_seen, last_seen, attempts, firmware_version
         FROM unknown_devices ORDER BY last_seen DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(UnknownDevice {
            imei: row.get(0)?,
            first_seen: row.get(1)?,
            last_seen: row.get(2)?,
            attempts: row.get(3)?,
            firmware_version: row.get(4)?,
            vrn: None,
        })
    })?;
    rows.collect()
}

/// Records a device turned away because its company isn't known. Runs on a blocking
/// thread so the connection's task isn't stalled on disk I/O.
pub async fn record_unknown_device(imei: &str, firmware_version: &str) {
    let (imei, firmware_version) = (imei.to_string(), firmware_version.to_string());
    let result = tokio::task::spawn_blocking(move || {
        with_db((), |conn| upsert_unknown_device(conn, &imei, &firmware_version, Local::now().timestamp()))
    })
    .await;
    if let Err(e) = result {
        log::error!("[DB] status=record_unknown_device_failed err={:?}", e);
    }
}

/// Devices turned away because their company isn't known, most recent first. Blocking.
pub fn unknown_devices_blocking() -> Vec<UnknownDevice> {
    with_db(Vec::new(), unknown_devices)
}

/// Drops a device from the unknown list, e.g. once it's been assigned. Blocking.
pub fn forget_unknown_device_blocking(imei: &str) {
    with_db((), |conn| {
        conn.execute("DELETE FROM unknown_devices WHERE imei = ?1", params![imei])?;
        Ok(())
    });
}

fn imeis_with_downloads(conn: &Connection, company: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT imei FROM downloads WHERE company = ?1 ORDER BY imei")?;
    let rows = stmt.query_map(params![company], |row| row.get(0))?;
    rows.collect()
}

/// Each IMEI's vehicle registration from its latest verified download that had one.
fn latest_vrns(conn: &Connection) -> rusqlite::Result<std::collections::HashMap<String, String>> {
    // SQLite takes the bare `vrn` from the row that has the MAX(id).
    let mut stmt = conn.prepare(
        "SELECT imei, vrn, MAX(id) FROM downloads
         WHERE verified = 1 AND vrn IS NOT NULL AND vrn <> ''
         GROUP BY imei",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// IMEI → vehicle registration from its latest verified download. Blocking.
pub fn latest_vrns_blocking() -> std::collections::HashMap<String, String> {
    with_db(Default::default(), latest_vrns)
}

/// Each IMEI's latest verified download time (unix seconds), across companies.
fn last_downloads(conn: &Connection) -> rusqlite::Result<std::collections::HashMap<String, i64>> {
    let mut stmt =
        conn.prepare("SELECT imei, MAX(downloaded_at) FROM downloads WHERE verified = 1 GROUP BY imei")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// IMEI → latest verified download time (unix seconds). Blocking.
pub fn last_downloads_blocking() -> std::collections::HashMap<String, i64> {
    with_db(Default::default(), last_downloads)
}

fn upsert_device_auth(conn: &Connection, imei: &str, succeeded: bool, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO device_auths (imei, last_at, succeeded) VALUES (?1, ?2, ?3)
         ON CONFLICT(imei) DO UPDATE SET last_at = excluded.last_at, succeeded = excluded.succeeded",
        params![imei, now, succeeded],
    )?;
    Ok(())
}

fn device_auths(conn: &Connection) -> rusqlite::Result<std::collections::HashMap<String, (i64, bool)>> {
    let mut stmt = conn.prepare("SELECT imei, last_at, succeeded FROM device_auths")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?;
    rows.collect()
}

/// Records a device's authentication outcome (its latest attempt). Runs on a blocking
/// thread so the connection's task isn't stalled on disk I/O.
pub async fn record_device_auth(imei: &str, succeeded: bool) {
    let imei = imei.to_string();
    let result = tokio::task::spawn_blocking(move || {
        with_db((), |conn| upsert_device_auth(conn, &imei, succeeded, Local::now().timestamp()))
    })
    .await;
    if let Err(e) = result {
        log::error!("[DB] status=record_device_auth_failed err={:?}", e);
    }
}

/// IMEI → (latest authentication time in unix seconds, whether it succeeded). Blocking.
pub fn device_auths_blocking() -> std::collections::HashMap<String, (i64, bool)> {
    with_db(Default::default(), device_auths)
}

/// IMEIs that have sent at least one download for `company`. Blocking.
pub fn imeis_with_downloads_blocking(company: &str) -> Vec<String> {
    with_db(Vec::new(), |conn| imeis_with_downloads(conn, company))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        migrate(&mut conn).expect("migrate");
        conn
    }

    fn record(company: &str, imei: &str, verified: bool) -> DownloadRecord {
        DownloadRecord {
            company: company.to_string(),
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

    const IMEI: &str = "356938035643809";

    #[test]
    fn last_verified_download_is_the_latest_verified_one() {
        let conn = test_db();
        insert_download(&conn, &record("acme", IMEI, true), local(2026, 9, 20, 10, 0)).unwrap();
        insert_download(&conn, &record("acme", IMEI, true), local(2026, 9, 23, 10, 0)).unwrap();
        // Later, but unverified — must not count.
        insert_download(&conn, &record("acme", IMEI, false), local(2026, 9, 25, 10, 0)).unwrap();

        assert_eq!(
            last_verified_download_date(&conn, "acme", IMEI).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 23)
        );
        assert_eq!(last_verified_download_date(&conn, "acme", "490154203237518").unwrap(), None);
    }

    #[test]
    fn downloads_only_count_for_their_own_company() {
        let conn = test_db();
        insert_download(&conn, &record("acme", IMEI, true), local(2026, 9, 23, 10, 0)).unwrap();
        assert_eq!(last_verified_download_date(&conn, "other", IMEI).unwrap(), None);
    }

    #[test]
    fn next_allowed_date_adds_whole_days() {
        let wed = NaiveDate::from_ymd_opt(2026, 9, 23).unwrap();
        assert_eq!(next_allowed_date(wed, 1), NaiveDate::from_ymd_opt(2026, 9, 24).unwrap());
        assert_eq!(next_allowed_date(wed, 7), NaiveDate::from_ymd_opt(2026, 9, 30).unwrap());
    }

    #[test]
    fn start_of_day_is_local_midnight() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        assert_eq!(start_of_day(date), local(2026, 9, 26, 0, 0));
    }

    #[test]
    fn recent_card_sessions_returns_the_latest_oldest_first() {
        let conn = test_db();
        let now = Local::now();
        for (company, secs) in [("acme", 10), ("other", 15), ("acme", 20), ("acme", 30), ("other", 35), ("acme", 40)] {
            insert_card_session(&conn, company, std::time::Duration::from_secs(secs), now).unwrap();
        }
        let secs = |sessions: Vec<std::time::Duration>| -> Vec<u64> { sessions.iter().map(|d| d.as_secs()).collect() };
        assert_eq!(secs(recent_card_sessions(&conn, Some("acme"), 3).unwrap()), vec![20, 30, 40]);
        assert_eq!(secs(recent_card_sessions(&conn, None, 3).unwrap()), vec![30, 35, 40]);
    }

    #[test]
    fn users_are_created_once_listed_updated_and_deleted() {
        let conn = test_db();
        assert!(create_user(&conn, "bob", "h1", 100).unwrap());
        assert!(!create_user(&conn, "bob", "h2", 200).unwrap(), "username already taken");
        assert_eq!(user_password_hash(&conn, "bob").unwrap().as_deref(), Some("h1"));
        assert!(update_user_password_hash(&conn, "bob", "h3", 300).unwrap());
        assert!(!update_user_password_hash(&conn, "nobody", "h", 300).unwrap());
        assert_eq!(
            users(&conn).unwrap(),
            vec![UserRecord { username: "bob".into(), created_at: 100, updated_at: 300 }]
        );
    }

    #[test]
    fn user_password_hash_is_created_then_replaced() {
        let conn = test_db();
        assert_eq!(user_password_hash(&conn, "admin").unwrap(), None);
        set_user_password_hash(&conn, "admin", "hash-1", 100).unwrap();
        set_user_password_hash(&conn, "admin", "hash-2", 200).unwrap();
        assert_eq!(user_password_hash(&conn, "admin").unwrap().as_deref(), Some("hash-2"));
        let (created, updated): (i64, i64) = conn
            .query_row("SELECT created_at, updated_at FROM users WHERE username = 'admin'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((created, updated), (100, 200));
    }

    #[test]
    fn unknown_devices_are_upserted_and_listed_newest_first() {
        let conn = test_db();
        upsert_unknown_device(&conn, IMEI, "1.0", 100).unwrap();
        upsert_unknown_device(&conn, "490154203237518", "1.0", 150).unwrap();
        upsert_unknown_device(&conn, IMEI, "1.1", 200).unwrap();
        let devices = unknown_devices(&conn).unwrap();
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].imei, IMEI);
        assert_eq!((devices[0].first_seen, devices[0].last_seen, devices[0].attempts), (100, 200, 2));
        assert_eq!(devices[0].firmware_version.as_deref(), Some("1.1"));
    }

    #[test]
    fn latest_vrn_is_from_the_newest_verified_download_with_one() {
        let conn = test_db();
        let with_vrn = |vrn: Option<&str>, verified: bool| DownloadRecord {
            vrn: vrn.map(str::to_string),
            ..record("acme", IMEI, verified)
        };
        insert_download(&conn, &with_vrn(Some("OLD1"), true), local(2026, 9, 20, 10, 0)).unwrap();
        insert_download(&conn, &with_vrn(Some("NEW1"), true), local(2026, 9, 21, 10, 0)).unwrap();
        insert_download(&conn, &with_vrn(None, true), local(2026, 9, 22, 10, 0)).unwrap();
        insert_download(&conn, &with_vrn(Some("BAD1"), false), local(2026, 9, 23, 10, 0)).unwrap();
        let vrns = latest_vrns(&conn).unwrap();
        assert_eq!(vrns.get(IMEI).map(String::as_str), Some("NEW1"));
        assert_eq!(vrns.len(), 1);
    }

    #[test]
    fn last_download_is_the_latest_verified_one() {
        let conn = test_db();
        insert_download(&conn, &record("acme", IMEI, true), local(2026, 9, 20, 10, 0)).unwrap();
        insert_download(&conn, &record("other", IMEI, true), local(2026, 9, 21, 10, 0)).unwrap();
        insert_download(&conn, &record("acme", IMEI, false), local(2026, 9, 22, 10, 0)).unwrap();
        let last = last_downloads(&conn).unwrap();
        assert_eq!(last.get(IMEI), Some(&local(2026, 9, 21, 10, 0).timestamp()));
    }

    #[test]
    fn device_auth_keeps_the_latest_attempt() {
        let conn = test_db();
        upsert_device_auth(&conn, IMEI, true, 100).unwrap();
        upsert_device_auth(&conn, IMEI, false, 200).unwrap();
        assert_eq!(device_auths(&conn).unwrap().get(IMEI), Some(&(200, false)));
    }

    #[test]
    fn user_theme_defaults_to_auto_and_can_be_set() {
        let conn = test_db();
        assert_eq!(user_theme(&conn, "bob").unwrap(), None);
        create_user(&conn, "bob", "h", 100).unwrap();
        assert_eq!(user_theme(&conn, "bob").unwrap().as_deref(), Some("Auto"));
        conn.execute("UPDATE users SET theme = 'Dark' WHERE username = 'bob'", []).unwrap();
        assert_eq!(user_theme(&conn, "bob").unwrap().as_deref(), Some("Dark"));
    }

    #[test]
    fn imeis_with_downloads_are_per_company() {
        let conn = test_db();
        insert_download(&conn, &record("acme", IMEI, false), local(2026, 9, 23, 10, 0)).unwrap();
        insert_download(&conn, &record("acme", IMEI, true), local(2026, 9, 24, 10, 0)).unwrap();
        insert_download(&conn, &record("other", "490154203237518", true), local(2026, 9, 24, 10, 0)).unwrap();
        assert_eq!(imeis_with_downloads(&conn, "acme").unwrap(), vec![IMEI.to_string()]);
    }

    #[test]
    fn upgrade_from_v2_assigns_existing_rows_to_the_default_company() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE downloads (id INTEGER PRIMARY KEY, imei TEXT NOT NULL, vrn TEXT,
                 downloaded_at INTEGER NOT NULL, local_date TEXT NOT NULL, verified INTEGER NOT NULL,
                 generation INTEGER, data_types INTEGER, bytes INTEGER NOT NULL, file_path TEXT);
             CREATE TABLE card_sessions (id INTEGER PRIMARY KEY, ended_at INTEGER NOT NULL,
                 duration_ms INTEGER NOT NULL);
             INSERT INTO downloads (imei, downloaded_at, local_date, verified, bytes)
                 VALUES ('356938035643809', 0, '2026-09-23', 1, 10);
             PRAGMA user_version = 2;",
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(
            last_verified_download_date(&conn, crate::config::DEFAULT_COMPANY_ID, IMEI).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 23)
        );
    }
}
