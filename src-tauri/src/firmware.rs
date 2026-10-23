//! Device firmware versions, and the protocol features each one has.
//!
//! Lets the server change how it talks to devices without updating every deployed
//! device at once: a change goes in `Features`, on from the first firmware that has
//! it, and devices on older firmware carry on as before.
//!
//! A version is an optional beta letter, the release, and an optional VAR:
//! `F7.0.82` and then `G7.0.82` are betas of the `7.0.82` release, and `7.0.82.3` is
//! VAR03 of it. Betas come before their release, in letter order. A VAR is a client
//! variant of the same release, so it doesn't change which features it has.
//!
//! To add a feature: a field in `Features`, set in `Features::for_version` from the
//! first version that has it, and checked wherever the server's handling differs.

use std::cmp::Ordering;

/// A device's firmware version, compared by release and then beta (see the module
/// doc). The VAR is left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirmwareVersion {
    release: (u32, u32, u32),
    /// `None` for the release itself.
    beta: Option<char>,
}

impl FirmwareVersion {
    pub const fn beta(letter: char, major: u32, minor: u32, patch: u32) -> Self {
        Self { release: (major, minor, patch), beta: Some(letter) }
    }

    /// `None` if `version` isn't in the form above.
    pub fn parse(version: &str) -> Option<Self> {
        let (beta, numbers) = match version.chars().next()? {
            letter if letter.is_ascii_uppercase() => (Some(letter), &version[1..]),
            _ => (None, version),
        };
        let parts: Vec<u32> = numbers.split('.').map(|part| part.parse().ok()).collect::<Option<_>>()?;
        match parts[..] {
            [major, minor, patch] | [major, minor, patch, _] => Some(Self { release: (major, minor, patch), beta }),
            _ => None,
        }
    }
}

impl Ord for FirmwareVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.release.cmp(&other.release).then(match (self.beta, other.beta) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
    }
}

impl ToString for FirmwareVersion {
    fn to_string(&self) -> String {
        let (major, minor, patch) = self.release;
        match self.beta {
            Some(letter) => format!("{letter}{major}.{minor}.{patch}"),
            None => format!("{major}.{minor}.{patch}"),
        }
    }
}

impl PartialOrd for FirmwareVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The protocol features a device's firmware has. Before a device announces itself,
/// or if its version can't be parsed, it's taken to have none of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Features {
    /// Its `Abort`s carry the VU stage and UDS status after the reason.
    pub abort_vu_failure: bool,
    /// ...followed by the VU's raw negative response code.
    pub abort_nrc: bool,
}

impl Features {
    pub fn for_version(version: &FirmwareVersion) -> Self {
        Self {
            abort_vu_failure: *version >= FirmwareVersion::beta('G', 7, 0, 82),
            abort_nrc: *version >= FirmwareVersion::beta('H', 7, 0, 82),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> FirmwareVersion {
        FirmwareVersion::parse(text).unwrap_or_else(|| panic!("{text} should parse"))
    }

    #[test]
    fn betas_come_before_their_release_in_letter_order() {
        assert!(version("F7.0.82") < version("G7.0.82"));
        assert!(version("G7.0.82") < version("7.0.82"));
        assert!(version("7.0.82") < version("A7.0.83"));
        assert!(version("7.0.9") < version("7.0.82"));
    }

    #[test]
    fn the_var_is_ignored() {
        assert_eq!(version("7.0.82.3"), version("7.0.82"));
        assert_eq!(version("E7.0.82.3"), FirmwareVersion::beta('E', 7, 0, 82));
    }

    #[test]
    fn malformed_versions_are_not_parsed() {
        for text in ["", "G", "7.0", "7.0.82.3.1", "g7.0.82", "7.x.82", "GG7.0.82"] {
            assert!(FirmwareVersion::parse(text).is_none(), "{text}");
        }
    }

    #[test]
    fn abort_vu_failure_is_on_from_g7_0_82() {
        let has_it = |text| Features::for_version(&version(text)).abort_vu_failure;
        assert!(!has_it("F7.0.82"));
        assert!(!has_it("7.0.81"));
        assert!(has_it("G7.0.82"));
        assert!(has_it("7.0.82.3"));
        assert!(has_it("A7.0.83"));
    }

    #[test]
    fn abort_nrc_is_on_from_h7_0_82() {
        let has_it = |text| Features::for_version(&version(text)).abort_nrc;
        assert!(!has_it("G7.0.82.3"));
        assert!(has_it("H7.0.82"));
        assert!(has_it("7.0.82"));
    }
}
