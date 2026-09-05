//! A repository's own forge token: where it is kept, and what may touch it
//! (task 022, ADR-0020).
//!
//! ADR-0012 opted a repository into `bypassPermissions` and left the credential
//! question open on the grounds that the operator knows what is in their own
//! environment. That holds while every queued repository is theirs. It stops
//! holding the first night a queue runs a client repository and a side project
//! on the same machine.
//!
//! # Cross-platform is this task's contract, not a follow-up
//!
//! ADR-0002 targets macOS first and keeps Windows and Linux viable — *"no
//! macOS-only APIs in the core"* — and this is the module most likely to break
//! that quietly, because secret storage, git authentication and credential
//! prompts each have a different native mechanism per platform. Every decision
//! here is chosen because it behaves the same on all three, and where it cannot,
//! the difference is surfaced rather than papered over.
//!
//! # The secret never reaches the database, and the trait is why
//!
//! [`CredentialStore`] exists so no test ever touches a real keychain: CI has no
//! unlocked keychain and no D-Bus, and a test that needs one is a test that
//! cannot run. [`KeyringStore`] is the real implementation;
//! [`MemoryStore`](crate::testing::credentials::MemoryStore) is the one the
//! suite uses, behind the `testing` feature.
//!
//! One item per repository, no composite keys to parse: the service name is the
//! app's bundle identifier and the **account is the repository id**.
//!
//! # What this bounds, and what it does not
//!
//! ADR-0020 is explicit and it is worth repeating to whoever reads this next: a
//! `bypassPermissions` run can read its own environment, so this bounds what a
//! stolen token is *worth*, not whether it can be stolen. The UI's guidance
//! toward a fine-grained, single-repository token is doing as much work as the
//! keychain is.

pub mod inject;
pub mod provision;
pub mod redact;

use std::fmt;

use crate::error::{Error, Result};

/// The service name every item is stored under — the app's bundle identifier,
/// as `tauri.conf.json` declares it.
///
/// A constant in core rather than a value the shell passes in, because the two
/// halves that must agree are a *save* and a *read at spawn time*, and those
/// happen in different processes' lifetimes. A shell that passed a different
/// string on the next launch would lose every credential silently.
pub const KEYCHAIN_SERVICE: &str = "com.rimaia.app";

/// Windows Credential Manager caps a credential blob at 2560 bytes; macOS and
/// Linux impose no comparable limit.
///
/// Validated on **every** platform so the failure is a message at paste time
/// everywhere rather than a truncated token on one. A GitHub fine-grained token
/// is ~93 bytes, so nothing legitimate is near this.
pub const MAX_SECRET_BYTES: usize = 2560;

/// A forge token, and the type system's whole contribution to keeping it out of
/// a log line.
///
/// No `Serialize`, no `Display`, and a hand-written [`fmt::Debug`] that prints
/// `Secret(***)`. That last one is the load-bearing part: every struct in this
/// crate derives `Debug`, `tracing` interpolates with `?` all over, and the
/// realistic leak is not somebody printing a token on purpose — it is a
/// `#[derive(Debug)]` on something that happens to hold one.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a pasted token, refusing one nothing could store.
    ///
    /// The two refusals are the two ways a paste goes wrong: an empty field,
    /// and a value past the smallest platform's ceiling. Both are told at the
    /// field rather than discovered by the run that needed it.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(Error::invalid("a token cannot be blank"));
        }
        if trimmed.len() > MAX_SECRET_BYTES {
            return Err(Error::invalid(format!(
                "that token is {} bytes, and Windows Credential Manager stores at most \
                 {MAX_SECRET_BYTES} — check that a whole file was not pasted in",
                trimmed.len(),
            )));
        }
        // Trimmed on the way in: a token pasted from a terminal carries a
        // trailing newline often enough that not trimming it would produce a
        // "bad credentials" nobody could explain.
        Ok(Self(trimmed.to_string()))
    }

    /// The bytes, for the two callers that legitimately need them: the
    /// verification subprocess and the child environment at spawn.
    ///
    /// Named `expose` rather than `as_str` on purpose — a call site reads as a
    /// deliberate act, and `grep expose` is the audit.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// Whether this machine can hold a secret at all, and whether this repository's
/// is there.
///
/// Three states rather than two, and the third is the point: `keyring` compiles
/// on a headless Linux box and *fails at runtime* without a running secret
/// service. That is a real user state — a server, a fresh container, a locked
/// login keyring — and reporting it as a failed save would tell the user to try
/// again at something that cannot work.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum StoreStatus {
    /// A secret is stored for this repository.
    Stored,
    /// The keychain works and holds nothing for this repository.
    Absent,
    /// There is no usable secret store on this machine.
    Unavailable { reason: String },
}

/// Where a repository's token lives.
///
/// `set`/`get`/`delete`/`status`, and nothing else — rotation, expiry
/// scheduling and vault integration are ADR-0020's explicit non-goals.
///
/// **Blocking**, deliberately: every backend is a synchronous platform API, and
/// wrapping four calls in `async` would either lie about it or drag a second
/// runtime concern into a crate that already has one. Callers reach it through
/// [`spawn_blocking`](tokio::task::spawn_blocking) — see [`get_for_spawn`].
pub trait CredentialStore: Send + Sync + 'static {
    fn set(&self, repository_id: &str, secret: &Secret) -> Result<()>;
    /// `None` when the store works and holds nothing for this repository.
    fn get(&self, repository_id: &str) -> Result<Option<Secret>>;
    /// Deleting what is not there is not an error: the caller's intent is "make
    /// sure there is none", and reporting a failure would make removing a
    /// half-configured repository impossible.
    fn delete(&self, repository_id: &str) -> Result<()>;
    fn status(&self, repository_id: &str) -> StoreStatus;
}

/// The real one: the OS keychain, through `keyring`.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyringStore;

impl KeyringStore {
    fn entry(repository_id: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(KEYCHAIN_SERVICE, repository_id).map_err(|error| {
            Error::internal(format!(
                "this machine's keychain could not be reached: {error}"
            ))
        })
    }
}

impl CredentialStore for KeyringStore {
    fn set(&self, repository_id: &str, secret: &Secret) -> Result<()> {
        Self::entry(repository_id)?
            .set_password(secret.expose())
            .map_err(|error| {
                // The error is `keyring`'s and names the platform's own failure
                // — a locked keyring, a denied prompt — which is more useful
                // than anything this layer could say instead.
                Error::internal(format!("the token could not be stored: {error}"))
            })
    }

    fn get(&self, repository_id: &str) -> Result<Option<Secret>> {
        match Self::entry(repository_id)?.get_password() {
            Ok(password) => Ok(Some(Secret(password))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(Error::internal(format!(
                "the token could not be read from this machine's keychain: {error}"
            ))),
        }
    }

    fn delete(&self, repository_id: &str) -> Result<()> {
        match Self::entry(repository_id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(Error::internal(format!(
                "the token could not be removed: {error}"
            ))),
        }
    }

    fn status(&self, repository_id: &str) -> StoreStatus {
        match self.get(repository_id) {
            Ok(Some(_)) => StoreStatus::Stored,
            Ok(None) => StoreStatus::Absent,
            Err(error) => StoreStatus::Unavailable {
                reason: error.to_string(),
            },
        }
    }
}

/// The store every spawn reads through, in a form `RunnerConfig` can hold.
///
/// An `Arc<dyn CredentialStore>` with a hand-written [`fmt::Debug`], because
/// `RunnerConfig` derives `Debug` and is printed in a log line at every spawn —
/// the same reason [`RunHandles`](crate::mcp::RunHandles) has one. It prints
/// nothing about what is stored, which is the only correct amount.
#[derive(Clone)]
pub struct CredentialAccess(std::sync::Arc<dyn CredentialStore>);

impl CredentialAccess {
    pub fn new(store: impl CredentialStore) -> Self {
        Self(std::sync::Arc::new(store))
    }

    /// This repository's token, read off the blocking platform API without
    /// blocking the runtime.
    ///
    /// A keychain that prompts takes as long as the user takes to answer, which
    /// is precisely the wait a Tokio worker must not be holding.
    pub async fn get(&self, repository_id: &str) -> Result<Option<Secret>> {
        let store = self.0.clone();
        let repository_id = repository_id.to_string();
        tokio::task::spawn_blocking(move || store.get(&repository_id))
            .await
            .map_err(|error| {
                Error::internal(format!("the keychain read did not complete: {error}"))
            })?
    }

    pub async fn set(&self, repository_id: &str, secret: Secret) -> Result<()> {
        let store = self.0.clone();
        let repository_id = repository_id.to_string();
        tokio::task::spawn_blocking(move || store.set(&repository_id, &secret))
            .await
            .map_err(|error| {
                Error::internal(format!("the keychain write did not complete: {error}"))
            })?
    }

    pub async fn delete(&self, repository_id: &str) -> Result<()> {
        let store = self.0.clone();
        let repository_id = repository_id.to_string();
        tokio::task::spawn_blocking(move || store.delete(&repository_id))
            .await
            .map_err(|error| {
                Error::internal(format!("the keychain delete did not complete: {error}"))
            })?
    }

    pub async fn status(&self, repository_id: &str) -> StoreStatus {
        let store = self.0.clone();
        let repository_id = repository_id.to_string();
        tokio::task::spawn_blocking(move || store.status(&repository_id))
            .await
            .unwrap_or_else(|error| StoreStatus::Unavailable {
                reason: format!("the keychain could not be asked: {error}"),
            })
    }
}

impl Default for CredentialAccess {
    fn default() -> Self {
        Self::new(KeyringStore)
    }
}

impl fmt::Debug for CredentialAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CredentialAccess")
    }
}

/// Reads a repository's token from a blocking store without blocking the
/// runtime.
///
/// The one place the `spawn_blocking` wrapping lives, so every caller — the
/// spawn path, the settings pane, the doctor — reaches a synchronous platform
/// API the same way. A keychain that prompts can take as long as the user takes
/// to answer, which is precisely the kind of wait a Tokio worker must not be
/// holding.
pub async fn get_for_spawn(
    store: &(impl CredentialStore + Clone),
    repository_id: &str,
) -> Result<Option<Secret>> {
    let store = store.clone();
    let repository_id = repository_id.to_string();
    tokio::task::spawn_blocking(move || store.get(&repository_id))
        .await
        .map_err(|error| Error::internal(format!("the keychain read did not complete: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_secrets_debug_output_is_three_asterisks_and_nothing_else() {
        // The one assertion this newtype exists for. Every struct in this crate
        // derives `Debug` and `tracing` interpolates with `?`, so the realistic
        // leak is a derive that happens to hold one of these.
        let secret = Secret::new("ghp_averyrealtoken").expect("a token");

        assert_eq!(format!("{secret:?}"), "Secret(***)");
        assert_eq!(format!("{:?}", Some(secret)), "Some(Secret(***))");
    }

    #[test]
    fn a_pasted_token_loses_the_whitespace_a_terminal_added() {
        assert_eq!(
            Secret::new("  ghp_token\n").expect("a token").expose(),
            "ghp_token"
        );
    }

    #[test]
    fn a_blank_token_is_refused_at_the_field() {
        assert!(Secret::new("   ").is_err());
        assert!(Secret::new("").is_err());
    }

    #[test]
    fn a_token_past_the_windows_ceiling_is_refused_on_every_platform() {
        // Validated everywhere so the failure is a message at paste time on all
        // three rather than a truncated token on one.
        let refusal = Secret::new("x".repeat(MAX_SECRET_BYTES + 1))
            .expect_err("a value past the ceiling is not storable");

        assert!(refusal.to_string().contains("2560"), "{refusal}");
        assert!(Secret::new("x".repeat(MAX_SECRET_BYTES)).is_ok());
    }
}
