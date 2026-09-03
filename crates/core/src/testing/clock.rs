//! A [`Clock`] whose hands the test moves.
//!
//! This is the piece that keeps retry tests honest *and* instant: a scheduler
//! waiting on a fifteen-minute backoff is exercised by advancing this clock
//! fifteen minutes, not by sleeping (ADR-0015). Cloning is cheap and shares the
//! same instant, so the test and the code under test can each hold one.
//!
//! # Why the instant lives in a `watch` channel rather than a `Mutex`
//!
//! [`Clock::sleep_until`] has to *resolve* when the test moves time, or task
//! 014's queue would sit on a real timer inside a test that thinks it advanced
//! four hours. A `Mutex` can be read but not awaited; a `watch` can be both, so
//! [`advance`](TestClock::advance) and [`set`](TestClock::set) wake every
//! pending waiter as a consequence of writing rather than through a second
//! notification anyone could forget to send.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use tokio::sync::watch;

use crate::clock::Clock;

#[derive(Debug, Clone)]
pub struct TestClock {
    /// Shared rather than owned so that `advance` on the test's handle is
    /// visible to — and awaited by — the copy the scheduler is holding. The
    /// `Arc` is what makes a clone drive the same channel; the sender is kept
    /// (rather than a receiver) because this value outlives every waiter it
    /// hands out.
    instant: Arc<watch::Sender<DateTime<Utc>>>,
}

impl TestClock {
    pub fn new(start: DateTime<Utc>) -> Self {
        let (instant, _) = watch::channel(start);
        Self {
            instant: Arc::new(instant),
        }
    }

    /// Jumps forward (or back, with a negative duration) by `by`, and resolves
    /// every [`sleep_until`](Clock::sleep_until) the jump reached.
    pub fn advance(&self, by: Duration) {
        let now = self.now();
        self.set(now + by);
    }

    /// Pins the clock to an absolute instant — for tests driven by a
    /// `rate_limit_event`'s epoch `resetsAt` rather than by an elapsed interval.
    ///
    /// `send_replace` rather than `send`: the latter reports an error when no
    /// receiver exists, which is the ordinary state of a clock nobody is
    /// currently waiting on.
    pub fn set(&self, at: DateTime<Utc>) {
        self.instant.send_replace(at);
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.instant.borrow()
    }

    /// Resolves once the test has moved the clock to `at` or past it.
    ///
    /// A loop rather than a single `changed()`, because a test may advance in
    /// several steps and only the last one reaches the deadline. `borrow_and_update`
    /// before the first await is what makes an already-elapsed deadline resolve
    /// immediately instead of waiting for an advance nobody is going to make.
    fn sleep_until(&self, at: DateTime<Utc>) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        let mut receiver = self.instant.subscribe();
        Box::pin(async move {
            loop {
                if *receiver.borrow_and_update() >= at {
                    return;
                }
                if receiver.changed().await.is_err() {
                    // Unreachable while the `TestClock` is alive, and waiting
                    // forever is the safe reading if it is not: a wait that
                    // resolved because the clock was dropped would look to the
                    // caller like time having passed.
                    std::future::pending::<()>().await;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .expect("test timestamp must be valid RFC 3339")
            .with_timezone(&Utc)
    }

    #[test]
    fn a_fifteen_minute_backoff_elapses_without_sleeping() {
        let clock = TestClock::new(at("2026-08-20T02:00:00Z"));
        clock.advance(Duration::minutes(15));
        assert_eq!(clock.now(), at("2026-08-20T02:15:00Z"));
    }

    #[test]
    fn a_clone_observes_time_moved_by_the_original() {
        let clock = TestClock::new(at("2026-08-20T02:00:00Z"));
        let held_by_the_code_under_test = clock.clone();

        clock.advance(Duration::hours(5));

        assert_eq!(
            held_by_the_code_under_test.now(),
            at("2026-08-20T07:00:00Z")
        );
    }

    #[test]
    fn set_jumps_to_an_absolute_reset_time() {
        let clock = TestClock::new(at("2026-08-20T02:00:00Z"));
        clock.set(at("2026-08-20T06:30:00Z"));
        assert_eq!(clock.now(), at("2026-08-20T06:30:00Z"));
    }

    #[test]
    fn it_is_usable_behind_the_trait_object_the_scheduler_holds() {
        let clock = TestClock::new(at("2026-08-20T02:00:00Z"));
        let injected: Arc<dyn Clock> = Arc::new(clock.clone());

        clock.advance(Duration::seconds(30));

        assert_eq!(injected.now(), at("2026-08-20T02:00:30Z"));
    }

    #[tokio::test]
    async fn advancing_the_clock_resolves_a_wait_the_advance_reached() {
        // The property the whole channel exists for. Without it a queue parked
        // on a fifteen-minute backoff inside a test would sit through fifteen
        // real minutes, and CLAUDE.md's "no sleep in tests, ever" would hold
        // for the policy function and quietly fail for the loop.
        let clock = TestClock::new(at("2026-08-20T02:00:00Z"));
        let waiter = clock.clone();
        let waiting = tokio::spawn(async move {
            waiter.sleep_until(at("2026-08-20T02:15:00Z")).await;
        });

        // In two steps, so the loop rather than a single `changed()` is what is
        // being exercised: the first advance moves the clock without reaching
        // the deadline.
        clock.advance(Duration::minutes(5));
        clock.advance(Duration::minutes(10));

        waiting.await.expect("the wait must resolve");
    }

    #[tokio::test]
    async fn a_deadline_already_past_resolves_without_any_advance() {
        let clock = TestClock::new(at("2026-08-20T02:00:00Z"));
        clock.sleep_until(at("2026-08-20T01:00:00Z")).await;
    }
}
