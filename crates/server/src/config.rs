//! What the binary reads from its environment, as a function over a lookup so
//! it is tested without touching the process's.
//!
//! Five variables, each named in the refusal when it is missing (D11), and one
//! optional:
//!
//! - `RIMAIA_DATA_DIR` is required: a server has no platform default, and the
//!   value follows the rules the desktop applies to its override (ADR-0023:
//!   absolute, no unexpanded `~`).
//! - `RIMAIA_LISTEN` is the socket address to bind.
//! - `RIMAIA_PUBLIC_URL` is the origin people reach the server at, such as
//!   `https://rimaia.example.com`. GitHub returns a sign-in to
//!   `<origin>/auth/github/callback`, which is the callback URL the OAuth app
//!   must be registered with (task 047).
//! - `RIMAIA_GITHUB_CLIENT_ID` and `RIMAIA_GITHUB_CLIENT_SECRET` are that OAuth
//!   app's. The secret is held in a [`Secret`], so neither a refusal nor the
//!   config's `Debug` can print it.
//!
//! - `RIMAIA_WEB_ROOT` is optional: the absolute path of the directory
//!   `npm run build` writes (`dist/`). Unset, the server serves the API only.
//!   Set to anything that is not a directory holding `index.html`, it refuses
//!   to start, because a server that starts and serves a blank page is the
//!   failure this prevents (task 050).
//!
//! The container and logging are task 062's.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;

use reqwest::Url;
use rimaia_core::identity::secret::Secret;
use rimaia_core::paths::DATA_DIR_ENV;
use rimaia_core::{AppPaths, Error, Result};

use crate::web;

/// The variable naming the socket address to bind.
pub const LISTEN_ENV: &str = "RIMAIA_LISTEN";
/// The variable naming the origin the server is reached at.
pub const PUBLIC_URL_ENV: &str = "RIMAIA_PUBLIC_URL";
/// The GitHub OAuth app's client id.
pub const GITHUB_CLIENT_ID_ENV: &str = "RIMAIA_GITHUB_CLIENT_ID";
/// The GitHub OAuth app's client secret.
pub const GITHUB_CLIENT_SECRET_ENV: &str = "RIMAIA_GITHUB_CLIENT_SECRET";
/// The directory holding the built web bundle.
pub const WEB_ROOT_ENV: &str = "RIMAIA_WEB_ROOT";

#[derive(Debug, Clone)]
pub struct Config {
    /// Where the server keeps `rimaia.db`.
    pub paths: AppPaths,
    pub listen: SocketAddr,
    /// The origin, with no trailing slash: `https://rimaia.example.com`.
    pub public_url: String,
    pub github_client_id: String,
    pub github_client_secret: Secret,
    /// Where the built bundle is, or `None` to serve the API only.
    pub web_root: Option<PathBuf>,
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

    let public_url = present(PUBLIC_URL_ENV).ok_or_else(|| {
        Error::invalid(format!(
            "{PUBLIC_URL_ENV} is not set. Give the origin people reach this server at, such \
             as https://rimaia.example.com: GitHub returns sign-ins to \
             <origin>/auth/github/callback."
        ))
    })?;
    let public_url = origin(&public_url.to_string_lossy()).ok_or_else(|| {
        Error::invalid(format!(
            "{PUBLIC_URL_ENV} is {}, which is not an origin. Give a scheme and a host with no \
             path, such as https://rimaia.example.com.",
            public_url.to_string_lossy()
        ))
    })?;

    let github_client_id = present(GITHUB_CLIENT_ID_ENV)
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| {
            Error::invalid(format!(
                "{GITHUB_CLIENT_ID_ENV} is not set. Give the client id of the GitHub OAuth app \
                 people sign in with."
            ))
        })?;
    let github_client_secret = present(GITHUB_CLIENT_SECRET_ENV)
        .and_then(|value| value.into_string().ok())
        .map(Secret::new)
        .ok_or_else(|| {
            Error::invalid(format!(
                "{GITHUB_CLIENT_SECRET_ENV} is not set. Give the client secret of the GitHub \
                 OAuth app people sign in with."
            ))
        })?;

    let web_root = present(WEB_ROOT_ENV)
        .map(|value| {
            let root = PathBuf::from(value);
            if !root.is_absolute() {
                return Err(Error::invalid(format!(
                    "{WEB_ROOT_ENV} is {}, which is not an absolute path. Give the full path of \
                     the directory `npm run build` writes.",
                    root.display()
                )));
            }
            match web::problem_with(&root) {
                None => Ok(root),
                Some(problem) => Err(Error::invalid(format!(
                    "{WEB_ROOT_ENV} is {}, which {problem}. Give the directory `npm run build` \
                     writes, or leave it unset to serve the API only.",
                    root.display()
                ))),
            }
        })
        .transpose()?;

    Ok(Config {
        paths,
        listen,
        public_url,
        github_client_id,
        github_client_secret,
        web_root,
    })
}

/// `text` as an `http` or `https` origin with no trailing slash, or `None`
/// when it has no host, or a path, a query or a fragment beyond the origin.
fn origin(text: &str) -> Option<String> {
    let url = Url::parse(text).ok()?;
    let bare = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none();
    bare.then(|| url.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// An absolute path on whichever platform the tests run on.
    fn absolute() -> PathBuf {
        std::env::temp_dir().join("rimaia-server-config")
    }

    const CLIENT_SECRET: &str = "a-github-client-secret";

    /// Every variable, valid.
    fn full() -> Vec<(&'static str, OsString)> {
        vec![
            (DATA_DIR_ENV, absolute().into_os_string()),
            (LISTEN_ENV, OsString::from("127.0.0.1:8787")),
            (PUBLIC_URL_ENV, OsString::from("https://rimaia.test/")),
            (GITHUB_CLIENT_ID_ENV, OsString::from("Iv1.test")),
            (GITHUB_CLIENT_SECRET_ENV, OsString::from(CLIENT_SECRET)),
        ]
    }

    /// [`full`], with `name` given `value`, or removed for `None`.
    fn with(name: &str, value: Option<&str>) -> Vec<(&'static str, OsString)> {
        let mut pairs: Vec<_> = full().into_iter().filter(|(key, _)| *key != name).collect();
        if let Some(value) = value {
            let key = full()
                .into_iter()
                .map(|(key, _)| key)
                .find(|key| *key == name)
                .expect("a known variable");
            pairs.push((key, OsString::from(value)));
        }
        pairs
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
    fn every_variable_makes_a_config() {
        let pairs = full();

        let config = config_from_env(lookup(&pairs)).expect("a valid environment");

        assert_eq!(config.paths.data_dir(), absolute());
        assert_eq!(config.listen, "127.0.0.1:8787".parse().expect("an address"));
        assert_eq!(config.public_url, "https://rimaia.test");
        assert_eq!(config.github_client_id, "Iv1.test");
        assert_eq!(config.github_client_secret.expose(), CLIENT_SECRET);
    }

    #[test]
    fn a_missing_data_dir_is_refused_by_name() {
        for value in [None, Some("")] {
            let pairs = with(DATA_DIR_ENV, value);

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
        let pairs = with(DATA_DIR_ENV, Some("relative/board"));

        let refused = config_from_env(lookup(&pairs)).expect_err("a relative path");

        assert!(refused.to_string().contains("RIMAIA_DATA_DIR"), "{refused}");
        assert!(refused.to_string().contains("absolute"), "{refused}");
    }

    #[test]
    fn a_tilde_data_dir_is_refused_by_name() {
        let pairs = with(DATA_DIR_ENV, Some("~/board"));

        let refused = config_from_env(lookup(&pairs)).expect_err("an unexpanded tilde");

        assert!(refused.to_string().contains("RIMAIA_DATA_DIR"), "{refused}");
    }

    #[test]
    fn a_missing_listen_address_is_refused_by_name() {
        let pairs = with(LISTEN_ENV, None);

        let refused = config_from_env(lookup(&pairs)).expect_err("no address");

        assert!(
            refused.to_string().starts_with("RIMAIA_LISTEN is not set"),
            "{refused}"
        );
    }

    #[test]
    fn an_unparseable_listen_address_is_refused_by_name() {
        let pairs = with(LISTEN_ENV, Some("localhost"));

        let refused = config_from_env(lookup(&pairs)).expect_err("not a socket address");

        assert_eq!(
            refused.to_string(),
            "RIMAIA_LISTEN is localhost, which is not a socket address. Give one such as \
             127.0.0.1:8787."
        );
    }

    #[test]
    fn each_sign_in_variable_is_refused_by_name_when_missing() {
        for name in [
            PUBLIC_URL_ENV,
            GITHUB_CLIENT_ID_ENV,
            GITHUB_CLIENT_SECRET_ENV,
        ] {
            for value in [None, Some("")] {
                let pairs = with(name, value);

                let refused = config_from_env(lookup(&pairs)).expect_err(name);

                assert!(
                    refused
                        .to_string()
                        .starts_with(&format!("{name} is not set")),
                    "{refused}"
                );
                assert!(!refused.to_string().contains(CLIENT_SECRET), "{refused}");
            }
        }
    }

    #[test]
    fn a_public_url_must_be_an_origin() {
        for (given, origin) in [
            ("https://rimaia.test", "https://rimaia.test"),
            ("http://localhost:8787/", "http://localhost:8787"),
        ] {
            let pairs = with(PUBLIC_URL_ENV, Some(given));
            let config = config_from_env(lookup(&pairs)).expect(given);
            assert_eq!(config.public_url, origin);
        }
        for refused in [
            "rimaia.test",
            "ftp://rimaia.test",
            "https://rimaia.test/board",
            "https://rimaia.test/?x=1",
            "https://user@rimaia.test",
        ] {
            let pairs = with(PUBLIC_URL_ENV, Some(refused));
            let error = config_from_env(lookup(&pairs)).expect_err(refused);
            assert!(
                error.to_string().starts_with("RIMAIA_PUBLIC_URL is "),
                "{error}"
            );
        }
    }

    #[test]
    fn no_secret_type_prints_its_value_in_debug() {
        let pairs = full();
        let config = config_from_env(lookup(&pairs)).expect("a valid environment");

        let printed = format!("{config:?}");

        assert!(!printed.contains(CLIENT_SECRET), "{printed}");
        assert!(printed.contains("Secret(…)"), "{printed}");
    }
}
