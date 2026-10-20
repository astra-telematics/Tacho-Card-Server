//! Which company a device belongs to.
//!
//! Everything that needs a device's company asks `directory()`, never the config
//! directly, so the source can change — e.g. to a tracking-platform lookup, with a local
//! cache so a platform outage doesn't stop downloads — without touching the rest of the
//! server. See `multi-company design.md`.

pub trait CompanyDirectory: Send + Sync {
    /// The id of the company this device belongs to, or `None` if it's unregistered.
    fn company_for_imei(&self, imei: &str) -> Option<String>;
}

/// Reads the IMEI registry in config.yaml (`companies.<id>.devices`). While the only
/// company is `default`, every IMEI belongs to it (see `config::company_for_imei`).
pub struct ConfigDirectory;

impl CompanyDirectory for ConfigDirectory {
    fn company_for_imei(&self, imei: &str) -> Option<String> {
        crate::config::company_for_imei(imei)
    }
}

/// The directory in use.
pub fn directory() -> &'static dyn CompanyDirectory {
    &ConfigDirectory
}
