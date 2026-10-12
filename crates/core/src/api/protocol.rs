//! Which client versions a server answers (ADR-0037 point 4, seam-contract D32
//! point 7).
//!
//! Every client sends its version in `Rimaia-Protocol`. A version is
//! *supported* when its major is the current major and its minor is the
//! current minor or the one before it; at a current minor of 0, only the
//! current version is. A supported version may read and write. Any other
//! version, older or newer, may still read and is refused every write with
//! `upgrade_required`: a runner cannot be ahead of the board, and a browser
//! left open across a deploy has to reload before it changes anything.
//!
//! A missing or unparseable header counts as unsupported. Every client sends
//! one, and ADR-0037 fails closed, so a client that sends none cannot write.
//!
//! The rule is written against a version it is given ([`check_against`])
//! rather than against [`PROTOCOL_VERSION`], because at `1.0` half of it
//! cannot be exercised: there is no previous minor to accept. The board
//! routes call [`check`]. Solo never does: one process on one version has no
//! skew.

use std::fmt;

use super::registry::Effect;
use super::PROTOCOL_VERSION;
use crate::error::{Error, Result};

/// The request header a client names its version in.
pub const HEADER: &str = "Rimaia-Protocol";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}

impl Version {
    /// `"<major>.<minor>"`, both plain decimal, or `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let (major, minor) = text.trim().split_once('.')?;
        Some(Self {
            major: decimal(major)?,
            minor: decimal(minor)?,
        })
    }

    /// The oldest version this one accepts a write from.
    fn oldest_supported(self) -> Self {
        Self {
            major: self.major,
            minor: self.minor.saturating_sub(1),
        }
    }

    fn supports(self, other: Self) -> bool {
        other.major == self.major
            && (self.oldest_supported().minor..=self.minor).contains(&other.minor)
    }

    /// The window as a person reads it: `1.0`, or `1.2 to 1.3`.
    fn window(self) -> String {
        let oldest = self.oldest_supported();
        if oldest == self {
            self.to_string()
        } else {
            format!("{oldest} to {self}")
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Digits only, so `+1`, ` 1` and `1e0` are not versions.
fn decimal(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The whole rule, against `current`.
pub fn check_against(current: Version, header: Option<&str>, effect: Effect) -> Result<()> {
    let received = header.and_then(Version::parse);
    if effect == Effect::Read || received.is_some_and(|version| current.supports(version)) {
        return Ok(());
    }
    let what = match (header, received) {
        (_, Some(version)) => format!("speaks Rimaia protocol {version}"),
        (Some(_), None) => format!("sent a {HEADER} header that names no version"),
        (None, None) => format!("sent no {HEADER} header"),
    };
    Err(Error::upgrade_required(format!(
        "this client {what}, and this server accepts changes from protocol {window} only; \
         update the client",
        window = current.window(),
    )))
}

/// [`check_against`] this build's [`PROTOCOL_VERSION`]: the only function a
/// board route calls.
pub fn check(header: Option<&str>, effect: Effect) -> Result<()> {
    let current =
        Version::parse(PROTOCOL_VERSION).expect("PROTOCOL_VERSION is a `<major>.<minor>` literal");
    check_against(current, header, effect)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;
    use pretty_assertions::assert_eq;

    const CURRENT: Version = Version { major: 1, minor: 3 };

    /// What the rule answers each header, for a read and for a write, as
    /// `(read allowed, write allowed)`.
    fn answers(header: Option<&str>) -> (bool, bool) {
        (
            check_against(CURRENT, header, Effect::Read).is_ok(),
            check_against(CURRENT, header, Effect::Write).is_ok(),
        )
    }

    #[test]
    fn the_current_and_previous_minor_read_and_write() {
        assert_eq!(answers(Some("1.3")), (true, true));
        assert_eq!(answers(Some("1.2")), (true, true));
    }

    #[test]
    fn an_older_minor_reads_but_cannot_write() {
        assert_eq!(answers(Some("1.1")), (true, false));
    }

    #[test]
    fn a_newer_minor_reads_but_cannot_write() {
        assert_eq!(answers(Some("1.4")), (true, false));
    }

    #[test]
    fn another_major_reads_but_cannot_write() {
        assert_eq!(answers(Some("2.3")), (true, false));
        assert_eq!(answers(Some("0.3")), (true, false));
    }

    #[test]
    fn a_missing_header_reads_but_cannot_write() {
        assert_eq!(answers(None), (true, false));
    }

    #[test]
    fn a_malformed_header_reads_but_cannot_write() {
        for header in [
            "",
            "1",
            "1.",
            ".3",
            "v1.3",
            "1.3.0",
            "+1.3",
            "1.x",
            "one.three",
        ] {
            assert_eq!(answers(Some(header)), (true, false), "{header:?}");
        }
    }

    #[test]
    fn a_refused_write_is_upgrade_required_and_names_what_was_sent_and_the_window() {
        let older = check_against(CURRENT, Some("1.1"), Effect::Write).expect_err("too old");
        assert_eq!(older.code(), ErrorCode::UpgradeRequired);
        assert_eq!(
            older.to_string(),
            "this client speaks Rimaia protocol 1.1, and this server accepts changes from \
             protocol 1.2 to 1.3 only; update the client"
        );

        let missing = check_against(CURRENT, None, Effect::Write).expect_err("no header");
        assert_eq!(
            missing.to_string(),
            "this client sent no Rimaia-Protocol header, and this server accepts changes from \
             protocol 1.2 to 1.3 only; update the client"
        );

        let malformed = check_against(CURRENT, Some("1.x"), Effect::Write).expect_err("malformed");
        assert_eq!(
            malformed.to_string(),
            "this client sent a Rimaia-Protocol header that names no version, and this server \
             accepts changes from protocol 1.2 to 1.3 only; update the client"
        );
    }

    #[test]
    fn surrounding_whitespace_is_not_part_of_the_version() {
        assert_eq!(Version::parse(" 1.3 "), Some(CURRENT));
    }

    #[test]
    fn the_real_protocol_version_writes_and_its_neighbours_do_not() {
        assert_eq!(PROTOCOL_VERSION, "1.0");
        check(Some("1.0"), Effect::Write).expect("the current version writes");

        for neighbour in ["0.9", "1.1", "2.0"] {
            check(Some(neighbour), Effect::Read).expect("every version reads");
            let refused = check(Some(neighbour), Effect::Write).expect_err(neighbour);
            assert_eq!(refused.code(), ErrorCode::UpgradeRequired);
            assert!(
                refused.to_string().contains("from protocol 1.0 only"),
                "names 1.0 as the oldest accepted: {refused}"
            );
        }
    }
}
