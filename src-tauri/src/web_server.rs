//! The web UI, for headless mode (Docker / NAS), where there's no window.
//!
//! Serves the same Vue app the desktop window shows, on its own port (default 8080,
//! `--web-port` to change it) so the device port can be exposed to the internet while
//! this one stays on the LAN. Plain HTTP: put it behind a TLS reverse proxy (e.g. DSM's)
//! if it has to leave the LAN. See "Users and the web UI" in `multi-company design.md`.
//!
//! - `POST /api/login`, `POST /api/logout`, `GET /api/session`: session cookie login.
//! - `POST /api/invoke/<command>`: the same commands the desktop app calls through
//!   Tauri, with the same JSON arguments (see `dispatch`). Errors are a JSON string, as
//!   Tauri gives them to the frontend.
//! - `GET /api/events`: the frontend events (`global_app_handle::emit`) as Server-Sent
//!   Events, named as in Tauri.
//! - Anything else: the embedded frontend build (`dist/spa`).
//!
//! Everything but login needs a session. POSTs must be JSON, which, with the
//! `SameSite=Strict` cookie, keeps other sites from making requests as a logged-in user.

// ───── Std Lib ─────
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ───── External Crates ─────
use axum::extract::{ConnectInfo, Path};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream;
use once_cell::sync::Lazy;
use rand::RngCore;
use rust_embed::RustEmbed;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;

// ───── Local Modules ─────
use crate::config::{self, CardConfig};
use crate::global_app_handle::subscribe_web_events;
use crate::{smart_card, users};

pub const DEFAULT_WEB_PORT: u16 = 8080;

const SESSION_COOKIE: &str = "tcs_session";
/// A session ends after this long without a request from it.
const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(12 * 60 * 60);
/// Failed logins from one address before each further attempt has to wait.
const FREE_LOGIN_FAILURES: u32 = 5;
/// The wait starts at 1 s and doubles per failure, up to this.
const MAX_LOGIN_DELAY: Duration = Duration::from_secs(60);
/// Failures older than this are forgotten.
const LOGIN_FAILURE_MEMORY: Duration = Duration::from_secs(60 * 60);
/// How often an event stream with nothing to send checks its session is still valid.
const EVENT_STREAM_SESSION_CHECK: Duration = Duration::from_secs(30);

/// The frontend build. Embedded in release builds; read from disk in debug builds.
/// Missing if the frontend hasn't been built (`npm run build`), in which case pages
/// get a 503 explaining that.
#[derive(RustEmbed)]
#[folder = "../dist/spa"]
#[allow_missing = true]
struct Ui;

pub async fn run(port: u16) {
    let app = Router::new()
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/session", get(session))
        .route("/api/invoke/{command}", post(invoke))
        .route("/api/events", get(events))
        .fallback(static_file)
        .into_make_service_with_connect_info::<SocketAddr>();

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(e) => {
            log::error!("[WEB] status=bind_failed addr={} err={}", addr, e);
            return;
        }
    };
    log::info!("[WEB] status=listening addr={}", addr);
    if let Err(e) = axum::serve(listener, app).await {
        log::error!("[WEB] status=stopped err={}", e);
    }
}

// ───── Errors ─────

/// A JSON string error body, the shape Tauri gives command errors to the frontend.
fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(Value::String(message.into()))).into_response()
}

fn not_logged_in() -> Response {
    error(StatusCode::UNAUTHORIZED, "Not logged in.")
}

// ───── Sessions ─────

struct Session {
    username: String,
    last_seen: Instant,
}

/// Logged-in sessions by token. In memory, so a restart logs everyone out.
static SESSIONS: Lazy<Mutex<HashMap<String, Session>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn sessions() -> std::sync::MutexGuard<'static, HashMap<String, Session>> {
    SESSIONS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn start_session(username: &str) -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let mut sessions = sessions();
    sessions.retain(|_, session| session.last_seen.elapsed() < SESSION_IDLE_TIMEOUT);
    sessions.insert(token.clone(), Session { username: username.to_string(), last_seen: Instant::now() });
    token
}

/// The username for a live session. `touch` counts this as activity, which keeps the
/// session alive; an event stream checking it doesn't.
fn session_user(token: &str, touch: bool) -> Option<String> {
    let mut sessions = sessions();
    let session = sessions.get_mut(token)?;
    if session.last_seen.elapsed() >= SESSION_IDLE_TIMEOUT {
        sessions.remove(token);
        return None;
    }
    if touch {
        session.last_seen = Instant::now();
    }
    Some(session.username.clone())
}

/// Ends a user's sessions (deleted, or password changed), except `keep` if given.
fn end_user_sessions(username: &str, keep: Option<&str>) {
    sessions().retain(|token, session| session.username != username || Some(token.as_str()) == keep);
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(SESSION_COOKIE)?.strip_prefix('=').map(str::to_string))
}

/// The session token and username of a logged-in request.
fn authenticate(headers: &HeaderMap) -> Option<(String, String)> {
    let token = session_token(headers)?;
    let username = session_user(&token, true)?;
    Some((token, username))
}

fn session_cookie(token: &str, max_age_seconds: u64) -> HeaderValue {
    let cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        SESSION_COOKIE, token, max_age_seconds
    );
    HeaderValue::from_str(&cookie).expect("session cookie is ASCII")
}

// ───── Login throttling ─────

struct LoginFailures {
    count: u32,
    last: Instant,
}

static LOGIN_FAILURES: Lazy<Mutex<HashMap<IpAddr, LoginFailures>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn login_failures() -> std::sync::MutexGuard<'static, HashMap<IpAddr, LoginFailures>> {
    LOGIN_FAILURES.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How long `ip` must wait before its next login attempt, if at all.
fn login_wait(ip: IpAddr) -> Option<Duration> {
    let failures = login_failures();
    let entry = failures.get(&ip)?;
    if entry.count < FREE_LOGIN_FAILURES || entry.last.elapsed() >= LOGIN_FAILURE_MEMORY {
        return None;
    }
    let delay = Duration::from_secs(1u64 << (entry.count - FREE_LOGIN_FAILURES).min(16)).min(MAX_LOGIN_DELAY);
    delay.checked_sub(entry.last.elapsed()).filter(|wait| !wait.is_zero())
}

fn record_login_failure(ip: IpAddr) {
    let mut failures = login_failures();
    failures.retain(|_, entry| entry.last.elapsed() < LOGIN_FAILURE_MEMORY);
    let entry = failures.entry(ip).or_insert(LoginFailures { count: 0, last: Instant::now() });
    entry.count += 1;
    entry.last = Instant::now();
}

// ───── Handlers ─────

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct SessionInfo {
    username: String,
}

async fn login(ConnectInfo(peer): ConnectInfo<SocketAddr>, Json(request): Json<LoginRequest>) -> Response {
    let ip = peer.ip();
    if let Some(wait) = login_wait(ip) {
        let seconds = wait.as_secs_f64().ceil() as u64;
        let mut response = error(
            StatusCode::TOO_MANY_REQUESTS,
            format!("Too many failed logins. Try again in {} s.", seconds),
        );
        response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        return response;
    }

    let username = request.username.trim().to_string();
    let check_username = username.clone();
    let valid = tokio::task::spawn_blocking(move || users::verify_login(&check_username, &request.password))
        .await
        .unwrap_or(false);
    if !valid {
        record_login_failure(ip);
        log::warn!("[WEB] action=login_failed username={} from={}", username, ip);
        return error(StatusCode::UNAUTHORIZED, "Wrong username or password.");
    }

    login_failures().remove(&ip);
    let token = start_session(&username);
    log::info!("[WEB] action=login username={} from={}", username, ip);
    let mut response = Json(SessionInfo { username }).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, session_cookie(&token, SESSION_IDLE_TIMEOUT.as_secs()));
    response
}

async fn logout(headers: HeaderMap) -> Response {
    if let Some(token) = session_token(&headers) {
        if let Some(session) = sessions().remove(&token) {
            log::info!("[WEB] action=logout username={}", session.username);
        }
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(header::SET_COOKIE, session_cookie("", 0));
    response
}

async fn session(headers: HeaderMap) -> Response {
    match authenticate(&headers) {
        Some((_, username)) => Json(SessionInfo { username }).into_response(),
        None => not_logged_in(),
    }
}

async fn invoke(headers: HeaderMap, Path(command): Path<String>, Json(args): Json<Value>) -> Response {
    let Some((token, username)) = authenticate(&headers) else {
        return not_logged_in();
    };
    match dispatch(&command, args, &token, &username).await {
        Ok(value) => Json(value).into_response(),
        Err(Unknown) => error(StatusCode::NOT_FOUND, format!("Unknown command '{}'.", command)),
        Err(Failed(message)) => {
            log::debug!("[WEB] command={} username={} err={}", command, username, message);
            error(StatusCode::BAD_REQUEST, message)
        }
    }
}

async fn events(headers: HeaderMap) -> Response {
    let Some((token, _)) = authenticate(&headers) else {
        return not_logged_in();
    };
    let receiver = subscribe_web_events();
    // Ends when the session does (logout, expiry, user deleted), so a page left open
    // doesn't keep receiving events after it's no longer logged in.
    let stream = stream::unfold((receiver, token), |(mut receiver, token)| async move {
        loop {
            match tokio::time::timeout(EVENT_STREAM_SESSION_CHECK, receiver.recv()).await {
                Ok(Ok(event)) => {
                    session_user(&token, false)?;
                    let sse = Event::default().event(event.name).data(event.payload.to_string());
                    return Some((Ok::<_, Infallible>(sse), (receiver, token)));
                }
                Ok(Err(RecvError::Lagged(missed))) => {
                    log::warn!("[WEB] event stream fell behind; {} event(s) skipped", missed);
                }
                Ok(Err(RecvError::Closed)) => return None,
                Err(_) => {
                    session_user(&token, false)?;
                }
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

async fn static_file(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return error(StatusCode::NOT_FOUND, "Not found.");
    }
    // The app uses hash routing, so any other path is the app itself.
    let (path, file) = match Ui::get(if path.is_empty() { "index.html" } else { path }) {
        Some(file) => (path, file),
        None => match Ui::get("index.html") {
            Some(file) => ("index.html", file),
            None => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "The web UI isn't built into this binary. Build the frontend (npm run build), then rebuild.",
                )
                    .into_response()
            }
        },
    };
    let is_page = path.is_empty() || path.ends_with(".html");
    let mut response = file.data.into_response();
    let response_headers = response.headers_mut();
    if let Ok(content_type) = HeaderValue::from_str(file.metadata.mimetype()) {
        response_headers.insert(header::CONTENT_TYPE, content_type);
    }
    response_headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if is_page {
        // Don't let another site frame the admin UI, and pick up new builds right away.
        response_headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    response
}

// ───── Commands ─────

enum DispatchError {
    Unknown,
    Failed(String),
}
use DispatchError::{Failed, Unknown};

impl From<String> for DispatchError {
    fn from(message: String) -> Self {
        Failed(message)
    }
}

fn args<T: DeserializeOwned>(args: Value) -> Result<T, DispatchError> {
    serde_json::from_value(args).map_err(|e| Failed(format!("Bad arguments: {}", e)))
}

fn json<T: Serialize>(value: T) -> Result<Value, DispatchError> {
    serde_json::to_value(value).map_err(|e| Failed(e.to_string()))
}

#[derive(Deserialize)]
struct CardNumberArg {
    cardnumber: String,
}

#[derive(Deserialize)]
struct UpdateCardArgs {
    cardnumber: String,
    content: CardConfig,
}

#[derive(Deserialize)]
struct SyncCardsArgs {
    #[serde(default)]
    readername: String,
    #[serde(default)]
    restart: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateConnectionArgs {
    listen_port: String,
    connection_limit: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateDownloadArgs {
    download_path: String,
    retain_failed_downloads: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateLoggingArgs {
    log_retention_days: String,
}

#[derive(Deserialize)]
struct ThemeArg {
    theme: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangePasswordArgs {
    current_password: String,
    new_password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveCompanyArgs {
    id: String,
    name: String,
    connection_limit: u16,
    download_interval_days: u32,
    create: bool,
}

#[derive(Deserialize)]
struct IdArg {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssignCardArgs {
    card_number: String,
    company: Option<String>,
}

#[derive(Deserialize)]
struct AssignDeviceArgs {
    imei: String,
    company: Option<String>,
}

#[derive(Deserialize)]
struct UsernameArg {
    username: String,
}

#[derive(Deserialize)]
struct UserPasswordArgs {
    username: String,
    password: String,
}

/// Runs a command for a web client: the same functions, arguments (camelCase, as
/// Tauri's `invoke` takes them) and results as the desktop app. Desktop-only commands
/// (`open_logs_folder`, `update_appearance`) aren't here. `token` and `username` are the
/// caller's session; the `*_my_*` commands act on that user.
async fn dispatch(command: &str, args_value: Value, token: &str, username: &str) -> Result<Value, DispatchError> {
    match command {
        "get_my_theme" => json(users::get_theme(username.to_string()).await?),
        "set_my_theme" => {
            let a: ThemeArg = args(args_value)?;
            json(users::set_theme(username.to_string(), a.theme).await?)
        }
        "change_my_password" => {
            let a: ChangePasswordArgs = args(args_value)?;
            users::change_own_password(username.to_string(), a.current_password, a.new_password).await?;
            // Log out everywhere else with the old password.
            end_user_sessions(username, Some(token));
            Ok(Value::Null)
        }
        "resend_state" => {
            crate::resend_state();
            Ok(Value::Null)
        }
        "manual_sync_cards" => {
            let a: SyncCardsArgs = args(args_value)?;
            json(smart_card::manual_sync_cards(a.readername, a.restart).await?)
        }
        "remove_card" => {
            let a: CardNumberArg = args(args_value)?;
            json(config::remove_card(a.cardnumber).await?)
        }
        "get_users" => json(users::get_users().await?),
        "add_user" => {
            let a: UserPasswordArgs = args(args_value)?;
            json(users::add_user(a.username, a.password).await?)
        }
        "set_user_password" => {
            let a: UserPasswordArgs = args(args_value)?;
            let username = a.username.clone();
            users::set_user_password(a.username, a.password).await?;
            // Log out everywhere else with the old password.
            end_user_sessions(&username, Some(token));
            Ok(Value::Null)
        }
        "delete_user" => {
            let a: UsernameArg = args(args_value)?;
            let username = a.username.clone();
            users::delete_user(a.username).await?;
            end_user_sessions(&username, None);
            Ok(Value::Null)
        }
        _ => {
            // The rest are synchronous and read or write config.yaml.
            let command = command.to_string();
            tokio::task::spawn_blocking(move || dispatch_blocking(&command, args_value))
                .await
                .map_err(|e| Failed(e.to_string()))?
        }
    }
}

fn dispatch_blocking(command: &str, args_value: Value) -> Result<Value, DispatchError> {
    match command {
        "get_config" => json(config::get_config()),
        "update_card" => {
            let a: UpdateCardArgs = args(args_value)?;
            json(config::update_card(&a.cardnumber, a.content))
        }
        "update_connection" => {
            let a: UpdateConnectionArgs = args(args_value)?;
            json(config::update_connection(a.listen_port, a.connection_limit))
        }
        "update_download" => {
            let a: UpdateDownloadArgs = args(args_value)?;
            json(config::update_download(a.download_path, a.retain_failed_downloads))
        }
        "update_logging" => {
            let a: UpdateLoggingArgs = args(args_value)?;
            json(config::update_logging(a.log_retention_days))
        }
        "get_companies" => json(config::get_companies()),
        "save_company" => {
            let a: SaveCompanyArgs = args(args_value)?;
            json(config::save_company(a.id, a.name, a.connection_limit, a.download_interval_days, a.create)?)
        }
        "delete_company" => {
            let a: IdArg = args(args_value)?;
            json(config::delete_company(a.id)?)
        }
        "assign_card" => {
            let a: AssignCardArgs = args(args_value)?;
            json(config::assign_card(a.card_number, a.company)?)
        }
        "assign_device" => {
            let a: AssignDeviceArgs = args(args_value)?;
            json(config::assign_device(a.imei, a.company)?)
        }
        "get_devices" => json(config::get_devices()),
        "get_active_downloads" => json(crate::download_activity::get_active_downloads()),
        _ => Err(Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_token_is_read_from_cookie_header() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("theme=dark; tcs_session=abc123; other=1"));
        assert_eq!(session_token(&headers).as_deref(), Some("abc123"));
        headers.insert(header::COOKIE, HeaderValue::from_static("tcs_session_old=x"));
        assert_eq!(session_token(&headers), None);
    }

    #[test]
    fn login_wait_starts_after_free_failures_and_is_capped() {
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        for _ in 0..FREE_LOGIN_FAILURES - 1 {
            record_login_failure(ip);
        }
        assert_eq!(login_wait(ip), None);
        record_login_failure(ip);
        assert!(login_wait(ip).is_some_and(|wait| wait <= Duration::from_secs(1)));
        for _ in 0..20 {
            record_login_failure(ip);
        }
        assert!(login_wait(ip).is_some_and(|wait| wait <= MAX_LOGIN_DELAY && wait > Duration::from_secs(59)));
    }
}
