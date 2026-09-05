//! A credential store no test can lose a real secret in.
//!
//! **CI has no unlocked keychain and no D-Bus**, and a test that needs one is a
//! test that cannot run — which is the whole reason
//! [`CredentialStore`](crate::credentials::CredentialStore) is a trait. This is
//! the implementation the suite uses; `cargo test -p rimaia-core` passes on
//! Linux, macOS and Windows against it with no secret store on any of them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::credentials::{CredentialStore, Secret, StoreStatus};
use crate::error::Result;

/// An in-memory keychain, shared by every clone.
///
/// Cloneable for the same reason the real one is `Copy`: the spawn path takes a
/// clone into `spawn_blocking`, and a store whose clone saw different contents
/// would make that path untestable.
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    items: Arc<Mutex<HashMap<String, String>>>,
    /// When set, every call reports the machine as having no usable store —
    /// the headless-Linux state the real backend fails at runtime with, which
    /// is a state `status` has to be able to report rather than a bug.
    unavailable: Option<String>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A machine with no secret service at all.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            unavailable: Some(reason.into()),
            ..Self::default()
        }
    }

    /// Removes an item behind the app's back — the "configured but the keychain
    /// item is gone" state ADR-0020 makes a refusal rather than a fallback.
    pub fn forget(&self, repository_id: &str) {
        self.lock().remove(repository_id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.items
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn refuse<T>(&self) -> Option<Result<T>> {
        self.unavailable.as_ref().map(|reason| {
            Err(crate::error::Error::internal(format!(
                "no keychain on this machine: {reason}"
            )))
        })
    }
}

impl CredentialStore for MemoryStore {
    fn set(&self, repository_id: &str, secret: &Secret) -> Result<()> {
        if let Some(refusal) = self.refuse() {
            return refusal;
        }
        self.lock()
            .insert(repository_id.to_string(), secret.expose().to_string());
        Ok(())
    }

    fn get(&self, repository_id: &str) -> Result<Option<Secret>> {
        if let Some(refusal) = self.refuse() {
            return refusal;
        }
        self.lock()
            .get(repository_id)
            .map(|stored| Secret::new(stored.clone()))
            .transpose()
    }

    fn delete(&self, repository_id: &str) -> Result<()> {
        if let Some(refusal) = self.refuse() {
            return refusal;
        }
        self.lock().remove(repository_id);
        Ok(())
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
