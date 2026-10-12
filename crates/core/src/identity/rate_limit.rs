//! Rate limits on signing in, pairing and minting (ADR-0030's Consequences,
//! seam-contract D34's "Hand-written").
//!
//! A fixed-window counter keyed by a [`Bucket`] and a key, reading the
//! injected [`Clock`], so a test crosses a window by moving a `TestClock`
//! rather than by waiting. A key's window starts at its first hit and resets
//! once it has passed; expired windows are pruned on every write.
//!
//! **A refusal is `invalid`**, saying how long to wait. D8 admits no new code
//! for it, because the client's action is to wait and the sentence says how
//! long; D32 point 3's table makes that a 400. A dedicated 429 would be an
//! amendment to both.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};

use crate::clock::Clock;
use crate::error::{Error, Result};

/// What is being limited, and by how much.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bucket {
    /// `GET /auth/github`, `GET /auth/desktop` and the callback, per peer
    /// address.
    SignIn,
    /// `POST /api/v1/auth/desktop_token`, per peer address.
    DesktopToken,
    /// `POST /api/v1/auth/pair`, per peer address.
    Pair,
    /// `create_personal_access_token`, `create_pairing_code` and
    /// `pair_own_runner`, per user.
    MintToken,
}

impl Bucket {
    /// How many attempts a key gets in one window.
    pub fn limit(self) -> u32 {
        match self {
            Bucket::SignIn => 30,
            Bucket::DesktopToken | Bucket::Pair => 10,
            Bucket::MintToken => 20,
        }
    }

    /// How long one window lasts.
    pub fn window(self) -> Duration {
        match self {
            Bucket::SignIn | Bucket::DesktopToken | Bucket::Pair => Duration::minutes(10),
            Bucket::MintToken => Duration::hours(1),
        }
    }
}

struct Window {
    started: DateTime<Utc>,
    hits: u32,
}

/// One limiter for a whole server, shared by the auth routes and the board
/// commands that mint (task 047 Scope 9).
pub struct RateLimiter {
    clock: Arc<dyn Clock>,
    windows: Mutex<HashMap<(Bucket, String), Window>>,
}

impl RateLimiter {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Counts one attempt by `key` in `bucket`, or refuses it when the key has
    /// used its window's limit.
    pub fn check(&self, bucket: Bucket, key: &str) -> Result<()> {
        let now = self.clock.now();
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        windows.retain(|(bucket, _), window| now < window.started + bucket.window());

        let window = windows.entry((bucket, key.to_string())).or_insert(Window {
            started: now,
            hits: 0,
        });
        if window.hits >= bucket.limit() {
            let left = window.started + bucket.window() - now;
            return Err(Error::invalid(format!(
                "Too many attempts. Try again in {} seconds.",
                whole_seconds_up(left)
            )));
        }
        window.hits += 1;
        Ok(())
    }
}

impl fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RateLimiter").finish_non_exhaustive()
    }
}

/// `duration` in whole seconds, rounded up, so "try again in n seconds" is
/// never early.
fn whole_seconds_up(duration: Duration) -> i64 {
    let millis = duration.num_milliseconds();
    (millis + 999).div_euclid(1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_epoch, TestClock};
    use crate::ErrorCode;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_rate_limit_refuses_the_attempt_after_its_limit_and_resets_with_the_window() {
        let clock = TestClock::new(test_epoch());
        let limiter = RateLimiter::new(Arc::new(clock.clone()));

        for _ in 0..10 {
            limiter
                .check(Bucket::Pair, "127.0.0.1")
                .expect("within the limit");
        }
        let refused = limiter
            .check(Bucket::Pair, "127.0.0.1")
            .expect_err("the eleventh");
        assert_eq!(refused.code(), ErrorCode::Invalid);
        assert_eq!(
            refused.to_string(),
            "Too many attempts. Try again in 600 seconds."
        );

        // Another key, and another bucket, have windows of their own.
        limiter
            .check(Bucket::Pair, "10.0.0.2")
            .expect("another peer");
        limiter
            .check(Bucket::DesktopToken, "127.0.0.1")
            .expect("another bucket");

        clock.advance(Duration::milliseconds(599_500));
        let refused = limiter
            .check(Bucket::Pair, "127.0.0.1")
            .expect_err("half a second left");
        assert_eq!(
            refused.to_string(),
            "Too many attempts. Try again in 1 seconds."
        );

        clock.advance(Duration::milliseconds(500));
        limiter
            .check(Bucket::Pair, "127.0.0.1")
            .expect("a new window");
    }

    #[test]
    fn each_bucket_has_its_limit_and_window() {
        let table: Vec<(Bucket, u32, i64)> = [
            Bucket::SignIn,
            Bucket::DesktopToken,
            Bucket::Pair,
            Bucket::MintToken,
        ]
        .into_iter()
        .map(|bucket| (bucket, bucket.limit(), bucket.window().num_seconds()))
        .collect();

        assert_eq!(
            table,
            vec![
                (Bucket::SignIn, 30, 600),
                (Bucket::DesktopToken, 10, 600),
                (Bucket::Pair, 10, 600),
                (Bucket::MintToken, 20, 3600),
            ]
        );
    }
}
