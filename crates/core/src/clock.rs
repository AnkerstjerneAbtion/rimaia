//! Time, as an injected dependency.
//!
//! Nothing in Rimaia calls `Utc::now()` directly. Retry backoff, run windows and
//! usage-limit reset times are all decided against a [`Clock`], so their tests
//! run instantly instead of sleeping (ADR-0015). Task 019 adds the controllable
//! test implementation; this module owns the trait and the real one.
//!
//! # Waiting is part of the trait, not something a caller reaches around it for
//!
//! Task 014's queue has to wake when a `waiting_retry` task becomes due, and a
//! bare `tokio::time::sleep` there would be a second clock: the deadline is
//! computed against [`Clock::now`], so the wait has to be too, or a test that
//! advances a [`TestClock`](crate::testing::TestClock) fifteen minutes would
//! still sit through fifteen real ones. [`Clock::sleep_until`] is what makes
//! "a fifteen-minute backoff test finishes in milliseconds" true for the *loop*
//! and not only for the policy function (seam-contract D22).

use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;

    /// Resolves at or after `at`; returns immediately for an instant already
    /// past.
    ///
    /// Boxed rather than an `async fn` so the trait stays object-safe — the
    /// scheduler holds an `Arc<dyn Clock>` — without an `async-trait`
    /// dependency, which seam-contract D6 would forbid anyway.
    fn sleep_until(&self, at: DateTime<Utc>) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
}

/// The wall clock. The only implementation permitted in production wiring.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    /// `tokio::time::sleep` over the remaining interval.
    ///
    /// A saturating conversion: a negative interval — an instant already past,
    /// or a clock the operating system moved backwards under us — becomes zero
    /// rather than an error, because "it is already time" is the honest reading
    /// of both.
    ///
    /// **This is a timer, not a wall-clock alarm**, which is why every caller
    /// caps what it passes here and re-checks [`now`](Clock::now) after waking:
    /// a `tokio` timer measures elapsed *monotonic* time, and a laptop asleep
    /// for four hours between 23:00 and 03:00 has elapsed almost none of it.
    fn sleep_until(&self, at: DateTime<Utc>) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        let delay = (at - Utc::now())
            .to_std()
            .unwrap_or(std::time::Duration::ZERO);
        Box::pin(tokio::time::sleep(delay))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn system_clock_is_usable_behind_the_trait_object() {
        // The scheduler holds an `Arc<dyn Clock>`; object safety is load-bearing,
        // not incidental.
        let clock: std::sync::Arc<dyn Clock> = std::sync::Arc::new(SystemClock);
        let first = clock.now();
        let second = clock.now();
        assert!(second >= first);
    }

    #[tokio::test]
    async fn an_instant_already_past_resolves_without_waiting() {
        // The saturating conversion, exercised: a queue that woke a moment late
        // must not then wait for the difference to come round again.
        let clock = SystemClock;
        clock
            .sleep_until(Utc::now() - chrono::Duration::hours(1))
            .await;
    }
}
