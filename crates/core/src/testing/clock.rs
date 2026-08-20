//! A [`Clock`] whose hands the test moves.
//!
//! This is the piece that keeps retry tests honest *and* instant: a scheduler
//! waiting on a fifteen-minute backoff is exercised by advancing this clock
//! fifteen minutes, not by sleeping (ADR-0015). Cloning is cheap and shares the
//! same instant, so the test and the code under test can each hold one.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};

use crate::clock::Clock;

#[derive(Debug, Clone)]
pub struct TestClock {
    /// Shared rather than owned so that `advance` on the test's handle is
    /// visible to the copy the scheduler is holding.
    instant: Arc<Mutex<DateTime<Utc>>>,
}

impl TestClock {
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            instant: Arc::new(Mutex::new(start)),
        }
    }

    /// Jumps forward (or back, with a negative duration) by `by`.
    pub fn advance(&self, by: Duration) {
        let mut instant = self.lock();
        *instant += by;
    }

    /// Pins the clock to an absolute instant — for tests driven by a
    /// `rate_limit_event`'s epoch `resetsAt` rather than by an elapsed interval.
    pub fn set(&self, at: DateTime<Utc>) {
        *self.lock() = at;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DateTime<Utc>> {
        // The guard never spans caller code, so the only way to poison this is a
        // panic inside the two statements above.
        self.instant.lock().expect("TestClock mutex poisoned")
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.lock()
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
}
