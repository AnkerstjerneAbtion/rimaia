//! Sign-in state that lives for seconds, held in server memory and never in
//! the database (seam-contract D28, "Sign-in state stays in memory").
//!
//! Litestream replicates every write to object storage (ADR-0037 point 2), so
//! a table for the OAuth `state`, the PKCE verifier or the desktop's one-time
//! code would put seconds-lived secrets into every backup. A restart in the
//! middle of a sign-in empties both maps, which costs the person one click.
//!
//! Both maps are keyed by the [`hash`](secret::hash) of their secret, hold an
//! entry for at most ten minutes by the injected [`Clock`], give each entry out
//! once, and prune expired entries whenever they are written, so neither grows
//! without bound.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Duration, Utc};

use super::secret::{self, Secret};
use crate::clock::Clock;
use crate::events::UserId;

/// How long a pending sign-in or a desktop code can be used.
pub fn pending_lifetime() -> Duration {
    Duration::minutes(10)
}

/// Which sign-in a pending entry belongs to.
#[derive(Clone, PartialEq, Eq)]
pub enum SignInFlow {
    /// A person signing in to the web app: the callback creates a session.
    Browser,
    /// The desktop app signing in through the browser (ADR-0030 point 4): the
    /// callback mints a one-time code and redirects to the app's loopback
    /// listener instead.
    Desktop {
        /// `http://127.0.0.1:<port>/callback`.
        redirect_uri: String,
        /// The desktop's own `state`, echoed back to its listener.
        desktop_state: String,
        /// The desktop's own PKCE S256 challenge, checked when it trades the
        /// code for a token.
        desktop_challenge: String,
        /// What the desktop token will be called.
        label: String,
    },
}

impl fmt::Debug for SignInFlow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignInFlow::Browser => f.write_str("Browser"),
            SignInFlow::Desktop {
                redirect_uri,
                label,
                ..
            } => f
                .debug_struct("Desktop")
                .field("redirect_uri", redirect_uri)
                .field("desktop_state", &"…")
                .field("desktop_challenge", &"…")
                .field("label", label)
                .finish(),
        }
    }
}

/// A sign-in between the redirect to the provider and its callback.
#[derive(Debug, Clone)]
pub struct PendingSignIn {
    /// The PKCE verifier whose challenge went to the provider.
    pub verifier: Secret,
    pub flow: SignInFlow,
    pub created_at: DateTime<Utc>,
}

/// The pending sign-ins, by the hash of their OAuth `state`.
pub struct PendingSignIns {
    clock: Arc<dyn Clock>,
    entries: Mutex<HashMap<String, PendingSignIn>>,
}

impl PendingSignIns {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Records a sign-in started with `state`.
    pub fn insert(&self, state: &str, verifier: Secret, flow: SignInFlow) {
        let now = self.clock.now();
        let mut entries = lock(&self.entries);
        entries.retain(|_, entry| is_live(entry.created_at, now));
        entries.insert(
            secret::hash(state),
            PendingSignIn {
                verifier,
                flow,
                created_at: now,
            },
        );
    }

    /// The sign-in started with `state`, removed so it is used once, or `None`
    /// when there is none or it is ten minutes old.
    pub fn take(&self, state: &str) -> Option<PendingSignIn> {
        let now = self.clock.now();
        lock(&self.entries)
            .remove(&secret::hash(state))
            .filter(|entry| is_live(entry.created_at, now))
    }

    /// How many entries are held, expired ones included until the next write.
    pub fn len(&self) -> usize {
        lock(&self.entries).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl fmt::Debug for PendingSignIns {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingSignIns")
            .field("entries", &self.len())
            .finish()
    }
}

/// A desktop sign-in that has reached the callback, waiting for the app to
/// trade its code for a token.
#[derive(Clone)]
pub struct DesktopCode {
    pub user_id: UserId,
    pub desktop_challenge: String,
    pub redirect_uri: String,
    pub label: String,
    pub created_at: DateTime<Utc>,
}

impl fmt::Debug for DesktopCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DesktopCode")
            .field("user_id", &self.user_id)
            .field("desktop_challenge", &"…")
            .field("redirect_uri", &self.redirect_uri)
            .field("label", &self.label)
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// The desktop codes, by the hash of the code.
pub struct DesktopCodes {
    clock: Arc<dyn Clock>,
    entries: Mutex<HashMap<String, DesktopCode>>,
}

impl DesktopCodes {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// A new one-time code for `user_id`'s desktop sign-in, which the callback
    /// hands to the app's loopback listener.
    pub fn mint(
        &self,
        user_id: &str,
        desktop_challenge: &str,
        redirect_uri: &str,
        label: &str,
    ) -> Secret {
        let now = self.clock.now();
        let code = secret::mint_session_secret();
        let mut entries = lock(&self.entries);
        entries.retain(|_, entry| is_live(entry.created_at, now));
        entries.insert(
            secret::hash(code.expose()),
            DesktopCode {
                user_id: user_id.to_string(),
                desktop_challenge: desktop_challenge.to_string(),
                redirect_uri: redirect_uri.to_string(),
                label: label.to_string(),
                created_at: now,
            },
        );
        code
    }

    /// The sign-in `code` was minted for, removed so it is used once, or
    /// `None` when there is none or it is ten minutes old.
    pub fn take(&self, code: &str) -> Option<DesktopCode> {
        let now = self.clock.now();
        lock(&self.entries)
            .remove(&secret::hash(code))
            .filter(|entry| is_live(entry.created_at, now))
    }

    pub fn len(&self) -> usize {
        lock(&self.entries).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl fmt::Debug for DesktopCodes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DesktopCodes")
            .field("entries", &self.len())
            .finish()
    }
}

fn is_live(created_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now - created_at < pending_lifetime()
}

/// The map, even if a panic elsewhere poisoned its lock: a map of short-lived
/// entries is never left half-written by `insert` or `remove`.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_epoch, TestClock};
    use pretty_assertions::assert_eq;

    fn desktop_flow() -> SignInFlow {
        SignInFlow::Desktop {
            redirect_uri: "http://127.0.0.1:5555/callback".to_string(),
            desktop_state: "desktop-state-secret".to_string(),
            desktop_challenge: "desktop-challenge-secret".to_string(),
            label: "My Mac".to_string(),
        }
    }

    #[test]
    fn a_pending_sign_in_is_single_use_and_expires_after_ten_minutes() {
        let clock = TestClock::new(test_epoch());

        // The pending sign-ins.
        let pending = PendingSignIns::new(Arc::new(clock.clone()));
        pending.insert("state-1", secret::mint_verifier(), SignInFlow::Browser);
        let taken = pending.take("state-1").expect("the first take");
        assert_eq!(taken.flow, SignInFlow::Browser);
        assert!(pending.take("state-1").is_none(), "single use");

        pending.insert("state-2", secret::mint_verifier(), desktop_flow());
        clock.advance(pending_lifetime() - Duration::seconds(1));
        assert_eq!(
            pending.take("state-2").map(|entry| entry.flow),
            Some(desktop_flow()),
            "ten minutes less a second is still live"
        );
        pending.insert("state-3", secret::mint_verifier(), SignInFlow::Browser);
        clock.advance(pending_lifetime());
        assert!(pending.take("state-3").is_none(), "ten minutes old");
        assert!(pending.take("never-issued").is_none());

        // The desktop codes.
        let codes = DesktopCodes::new(Arc::new(clock.clone()));
        let code = codes.mint(
            "user-1",
            "challenge",
            "http://127.0.0.1:5555/callback",
            "Mac",
        );
        let taken = codes.take(code.expose()).expect("the first take");
        assert_eq!(taken.user_id, "user-1");
        assert!(codes.take(code.expose()).is_none(), "single use");

        let late = codes.mint(
            "user-1",
            "challenge",
            "http://127.0.0.1:5555/callback",
            "Mac",
        );
        clock.advance(pending_lifetime() - Duration::seconds(1));
        let live = codes.mint(
            "user-1",
            "challenge",
            "http://127.0.0.1:5555/callback",
            "Mac",
        );
        clock.advance(Duration::seconds(1));
        assert!(codes.take(late.expose()).is_none(), "ten minutes old");
        assert!(codes.take(live.expose()).is_some(), "a second old");
    }

    #[test]
    fn writing_prunes_the_expired_entries() {
        let clock = TestClock::new(test_epoch());
        let pending = PendingSignIns::new(Arc::new(clock.clone()));
        let codes = DesktopCodes::new(Arc::new(clock.clone()));
        for n in 0..5 {
            pending.insert(
                &format!("state-{n}"),
                secret::mint_verifier(),
                SignInFlow::Browser,
            );
            codes.mint("user-1", "challenge", "http://127.0.0.1:1/callback", "Mac");
        }

        clock.advance(pending_lifetime());
        pending.insert("fresh", secret::mint_verifier(), SignInFlow::Browser);
        codes.mint("user-1", "challenge", "http://127.0.0.1:1/callback", "Mac");

        assert_eq!((pending.len(), codes.len()), (1, 1));
    }

    #[test]
    fn a_pending_entry_prints_no_secret() {
        let clock = TestClock::new(test_epoch());
        let entry = PendingSignIn {
            verifier: Secret::new("verifier-secret"),
            flow: desktop_flow(),
            created_at: clock.now(),
        };
        let codes = DesktopCodes::new(Arc::new(clock.clone()));
        let code = codes.mint(
            "user-1",
            "desktop-challenge-secret",
            "http://127.0.0.1:1/callback",
            "Mac",
        );
        let taken = codes.take(code.expose()).expect("the code");

        let printed = format!("{entry:?} {taken:?} {codes:?}");
        for secret in [
            "verifier-secret",
            "desktop-state-secret",
            "desktop-challenge-secret",
            code.expose(),
        ] {
            assert!(!printed.contains(secret), "{printed}");
        }
    }
}
