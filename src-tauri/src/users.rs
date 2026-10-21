//! User accounts, stored in the database with Argon2id password hashes.
//!
//! There's always a built-in `admin` account. Its password lives in config.yaml
//! (`admin_password`), and the database is made to match it on every startup
//! (`ensure_admin`):
//!
//! - **Plaintext**, as generated on first start or typed in by hand.
//! - **An Argon2id hash**, once admin changes the password in the UI
//!   (`change_own_password`), so it's no longer readable there.
//! - **Missing or blank**: a new random password is generated and written back in
//!   plaintext — the way to recover a lost admin password. See `config::init_config`.
//!
//! Each user also has a UI theme (`get_theme` / `set_theme`); the desktop app, which has
//! no login, uses admin's.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::distributions::Alphanumeric;
use rand::{Rng, RngCore};

/// Username of the built-in administrator account.
pub const ADMIN_USERNAME: &str = "admin";

/// Length of a generated admin password: 20 letters and digits, ~119 bits.
const GENERATED_PASSWORD_LENGTH: usize = 20;

/// Shortest password accepted for a new user or a password change.
pub const MIN_PASSWORD_LENGTH: usize = 8;

/// Longest username accepted.
const MAX_USERNAME_LENGTH: usize = 32;

/// Checked against when a login names no existing user; see `verify_login`.
#[cfg(not(feature = "desktop"))]
static DUMMY_HASH: once_cell::sync::Lazy<String> =
    once_cell::sync::Lazy::new(|| hash_password("not a real password").unwrap_or_default());

/// A new random password of letters and digits.
pub fn generate_password() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(GENERATED_PASSWORD_LENGTH)
        .map(char::from)
        .collect()
}

/// Argon2id hash of `password` with a fresh random salt, in PHC string format.
fn hash_password(password: &str) -> Result<String, String> {
    let mut salt_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|e| e.to_string())?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| e.to_string())
}

/// Whether `password` matches a stored PHC-format hash. A malformed hash never matches.
fn password_matches(password: &str, stored_hash: &str) -> bool {
    PasswordHash::new(stored_hash)
        .map(|hash| Argon2::default().verify_password(password.as_bytes(), &hash).is_ok())
        .unwrap_or(false)
}

/// Whether a config.yaml `admin_password` is a hash (set from the UI) rather than the
/// password itself.
fn is_password_hash(value: &str) -> bool {
    value.starts_with("$argon2") && PasswordHash::new(value).is_ok_and(|hash| hash.salt.is_some() && hash.hash.is_some())
}

/// Makes the built-in `admin` account match config.yaml's `admin_password`, which is
/// either the password or its hash: creates the account if it doesn't exist, and
/// writes only if the stored hash doesn't already match (so a normal startup doesn't
/// rewrite it). Blocking.
pub fn ensure_admin(configured: &str) -> Result<(), String> {
    let stored = crate::database::user_password_hash_blocking(ADMIN_USERNAME)?;
    let hash = if is_password_hash(configured) {
        if stored.as_deref() == Some(configured) {
            return Ok(());
        }
        configured.to_string()
    } else {
        if stored.as_deref().is_some_and(|hash| password_matches(configured, hash)) {
            return Ok(());
        }
        hash_password(configured)?
    };
    crate::database::set_user_password_hash_blocking(ADMIN_USERNAME, &hash)
}

/// Whether `username` and `password` are a valid login. `false` for an unknown user, a
/// wrong password, or if the database can't be read — never let someone in on an error.
/// Blocking.
#[cfg(not(feature = "desktop"))] // logins are only for the server build's web UI
pub fn verify_login(username: &str, password: &str) -> bool {
    match crate::database::user_password_hash_blocking(username) {
        Ok(Some(hash)) => password_matches(password, &hash),
        Ok(None) => {
            // Take as long as a real check, so timing doesn't reveal which usernames exist.
            let _ = password_matches(password, &DUMMY_HASH);
            false
        }
        Err(e) => {
            log::error!("[USERS] status=verify_failed username={} err={}", username, e);
            false
        }
    }
}

/// Usernames are 1–32 letters, digits, '.', '_' or '-'. Any capitalisation of "admin"
/// is reserved for the built-in account.
fn validate_new_username(username: &str) -> Result<(), String> {
    if username.is_empty() || username.len() > MAX_USERNAME_LENGTH {
        return Err(format!("A username is 1 to {} characters.", MAX_USERNAME_LENGTH));
    }
    if !username.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        return Err("A username can only contain letters, digits, '.', '_' and '-'.".into());
    }
    if username.eq_ignore_ascii_case(ADMIN_USERNAME) {
        return Err(format!("'{}' is reserved for the built-in account.", username));
    }
    Ok(())
}

fn validate_password(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD_LENGTH {
        return Err(format!("A password is at least {} characters.", MIN_PASSWORD_LENGTH));
    }
    Ok(())
}

/// Runs blocking user work (database, Argon2) off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

/// Every user account (without password hashes).
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_users() -> Result<Vec<crate::database::UserRecord>, String> {
    blocking(crate::database::users_blocking).await
}

/// Creates a user account.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn add_user(username: String, password: String) -> Result<(), String> {
    let username = username.trim().to_string();
    validate_new_username(&username)?;
    validate_password(&password)?;
    let log_name = username.clone();
    let created = blocking(move || crate::database::create_user_blocking(&username, &hash_password(&password)?)).await?;
    if !created {
        return Err(format!("There's already a user called '{}'.", log_name));
    }
    log::info!("[USERS] username={} action=created", log_name);
    Ok(())
}

/// Changes the logged-in user's own password, after checking their current one. For
/// `admin` the new hash goes into config.yaml first (it's what the database is made to
/// match on startup), replacing the plaintext there.
#[cfg(not(feature = "desktop"))] // only the web UI has logins
pub async fn change_own_password(username: String, current: String, new: String) -> Result<(), String> {
    validate_password(&new)?;
    let log_name = username.clone();
    blocking(move || {
        let stored = crate::database::user_password_hash_blocking(&username)?;
        if !stored.is_some_and(|hash| password_matches(&current, &hash)) {
            return Err("Your current password is wrong.".into());
        }
        let hash = hash_password(&new)?;
        if username == ADMIN_USERNAME {
            crate::config::set_admin_password(hash.clone())?;
        }
        if !crate::database::update_user_password_hash_blocking(&username, &hash)? {
            return Err(format!("There's no user called '{}'.", username));
        }
        Ok(())
    })
    .await?;
    log::info!("[USERS] username={} action=own_password_changed", log_name);
    Ok(())
}

/// Themes a user can pick, as the frontend's `applyTheme` takes them.
const THEMES: [&str; 3] = ["Light", "Auto", "Dark"];

/// A user's UI theme.
pub async fn get_theme(username: String) -> Result<String, String> {
    let theme = blocking(move || crate::database::user_theme_blocking(&username)).await?;
    Ok(theme.unwrap_or_else(|| "Auto".into()))
}

/// Sets a user's UI theme.
pub async fn set_theme(username: String, theme: String) -> Result<(), String> {
    if !THEMES.contains(&theme.as_str()) {
        return Err(format!("Unknown theme '{}'.", theme));
    }
    let updated = blocking(move || crate::database::set_user_theme_blocking(&username, &theme)).await?;
    if !updated {
        return Err("Your account no longer exists.".into());
    }
    Ok(())
}

/// The desktop app's theme. It has no login, so it's admin's.
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn get_my_theme() -> Result<String, String> {
    get_theme(ADMIN_USERNAME.into()).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn set_my_theme(theme: String) -> Result<(), String> {
    set_theme(ADMIN_USERNAME.into(), theme).await
}

/// Changes a user's password. Not for `admin`, who changes it themselves
/// (`change_own_password`) or in config.yaml.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn set_user_password(username: String, password: String) -> Result<(), String> {
    if username == ADMIN_USERNAME {
        return Err("Only admin can change the admin password (account menu → Change password).".into());
    }
    validate_password(&password)?;
    let log_name = username.clone();
    let updated =
        blocking(move || crate::database::update_user_password_hash_blocking(&username, &hash_password(&password)?)).await?;
    if !updated {
        return Err(format!("There's no user called '{}'.", log_name));
    }
    log::info!("[USERS] username={} action=password_changed", log_name);
    Ok(())
}

/// Deletes a user account. `admin` can't be deleted.
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn delete_user(username: String) -> Result<(), String> {
    if username == ADMIN_USERNAME {
        return Err("The built-in admin account can't be deleted.".into());
    }
    let log_name = username.clone();
    let deleted = blocking(move || crate::database::delete_user_blocking(&username)).await?;
    if !deleted {
        return Err(format!("There's no user called '{}'.", log_name));
    }
    log::info!("[USERS] username={} action=deleted", log_name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_verify_and_are_salted() {
        let first = hash_password("correct horse").unwrap();
        let second = hash_password("correct horse").unwrap();
        assert_ne!(first, second, "each hash gets its own salt");
        assert!(first.starts_with("$argon2id$"));
        assert!(password_matches("correct horse", &first));
        assert!(!password_matches("wrong", &first));
        assert!(!password_matches("correct horse", "not a hash"));
    }

    #[test]
    fn new_usernames_are_checked() {
        assert!(validate_new_username("jess.doyle").is_ok());
        assert!(validate_new_username("").is_err());
        assert!(validate_new_username("has space").is_err());
        assert!(validate_new_username("Admin").is_err());
        assert!(validate_new_username(&"a".repeat(MAX_USERNAME_LENGTH + 1)).is_err());
    }

    #[test]
    fn only_real_hashes_count_as_hashes() {
        assert!(is_password_hash(&hash_password("correct horse").unwrap()));
        assert!(!is_password_hash("correct horse"));
        assert!(!is_password_hash("$argon2id$not-really"));
    }

    #[test]
    fn generated_passwords_are_long_random_alphanumerics() {
        let password = generate_password();
        assert_eq!(password.len(), GENERATED_PASSWORD_LENGTH);
        assert!(password.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(password, generate_password());
    }
}
