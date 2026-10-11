//! What the binary reads from its environment, as a function over a lookup so
//! it is tested without touching the process's.
//!
//! Two variables. `RIMAIA_DATA_DIR` is required: a server has no platform
//! default, and the value follows the rules the desktop applies to its
//! override (ADR-0023: absolute, no unexpanded `~`). `RIMAIA_LISTEN` is the
//! socket address to bind. The container, the public URL and logging are task
//! 062's.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;

use rimaia_core::paths::DATA_DIR_ENV;
use rimaia_core::{AppPaths, Error, Result};

/// The variable naming the socket address to bind.
pub const LISTEN_ENV: &str = "RIMAIA_LISTEN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Where the server keeps `rimaia.db`.
    pub paths: AppPaths,
    pub listen: SocketAddr,
}

/// The configuration `lookup` describes, or an `Invalid` naming the variable
/// that is missing or wrong. An empty value is a missing one.
pub fn config_from_env(lookup: impl Fn(&str) -> Option<OsString>) -> Result<Config> {
    let present = |name: &str| lookup(name).filter(|value| !value.is_empty());

    let data_dir = present(DATA_DIR_ENV).ok_or_else(|| {
        Error::invalid(format!(
            "{DATA_DIR_ENV} is not set. The server keeps its board there and has no default: \
             give an absolute path, and never a desktop app's data directory."
        ))
    })?;
    // `resolve`'s fallback is never taken: the value is present and not empty.
    let paths = AppPaths::resolve(Some(&data_dir), PathBuf::new())?;

    let listen = present(LISTEN_ENV).ok_or_else(|| {
        Error::invalid(format!(
            "{LISTEN_ENV} is not set. Give the socket address to listen on, such as \
             127.0.0.1:8787."
        ))
    })?;
    let listen = listen
        .to_str()
        .and_then(|text| text.parse::<SocketAddr>().ok())
        .ok_or_else(|| {
            Error::invalid(format!(
                "{LISTEN_ENV} is {}, which is not a socket address. Give one such as \
                 127.0.0.1:8787.",
                listen.to_string_lossy()
            ))
        })?;

    Ok(Config { paths, listen })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// An absolute path on whichever platform the tests run on.
    fn absolute() -> PathBuf {
        std::env::temp_dir().join("rimaia-server-config")
    }

    fn lookup<'a>(pairs: &'a [(&'a str, OsString)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn both_variables_make_a_config() {
        let pairs = [
            (DATA_DIR_ENV, absolute().into_os_string()),
            (LISTEN_ENV, OsString::from("127.0.0.1:8787")),
        ];

        let config = config_from_env(lookup(&pairs)).expect("a valid environment");

        assert_eq!(config.paths.data_dir(), absolute());
        assert_eq!(config.listen, "127.0.0.1:8787".parse().expect("an address"));
    }

    #[test]
    fn a_missing_data_dir_is_refused_by_name() {
        for value in [None, Some(OsString::new())] {
            let mut pairs = vec![(LISTEN_ENV, OsString::from("127.0.0.1:8787"))];
            if let Some(value) = value {
                pairs.push((DATA_DIR_ENV, value));
            }

            let refused = config_from_env(lookup(&pairs)).expect_err("no data directory");

            assert!(
                refused
                    .to_string()
                    .starts_with("RIMAIA_DATA_DIR is not set"),
                "{refused}"
            );
        }
    }

    #[test]
    fn a_relative_data_dir_is_refused_by_name() {
        let pairs = [
            (DATA_DIR_ENV, OsString::from("relative/board")),
            (LISTEN_ENV, OsString::from("127.0.0.1:8787")),
        ];

        let refused = config_from_env(lookup(&pairs)).expect_err("a relative path");

        assert!(refused.to_string().contains("RIMAIA_DATA_DIR"), "{refused}");
        assert!(refused.to_string().contains("absolute"), "{refused}");
    }

    #[test]
    fn a_tilde_data_dir_is_refused_by_name() {
        let pairs = [
            (DATA_DIR_ENV, OsString::from("~/board")),
            (LISTEN_ENV, OsString::from("127.0.0.1:8787")),
        ];

        let refused = config_from_env(lookup(&pairs)).expect_err("an unexpanded tilde");

        assert!(refused.to_string().contains("RIMAIA_DATA_DIR"), "{refused}");
    }

    #[test]
    fn a_missing_listen_address_is_refused_by_name() {
        let pairs = [(DATA_DIR_ENV, absolute().into_os_string())];

        let refused = config_from_env(lookup(&pairs)).expect_err("no address");

        assert!(
            refused.to_string().starts_with("RIMAIA_LISTEN is not set"),
            "{refused}"
        );
    }

    #[test]
    fn an_unparseable_listen_address_is_refused_by_name() {
        let pairs = [
            (DATA_DIR_ENV, absolute().into_os_string()),
            (LISTEN_ENV, OsString::from("localhost")),
        ];

        let refused = config_from_env(lookup(&pairs)).expect_err("not a socket address");

        assert_eq!(
            refused.to_string(),
            "RIMAIA_LISTEN is localhost, which is not a socket address. Give one such as \
             127.0.0.1:8787."
        );
    }
}
