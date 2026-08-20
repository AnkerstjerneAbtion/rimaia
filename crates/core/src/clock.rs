//! Time, as an injected dependency.
//!
//! Nothing in Rimaia calls `Utc::now()` directly. Retry backoff, run windows and
//! usage-limit reset times are all decided against a [`Clock`], so their tests
//! run instantly instead of sleeping (ADR-0015). Task 019 adds the controllable
//! test implementation; this module owns the trait and the real one.

use chrono::{DateTime, Utc};

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

/// The wall clock. The only implementation permitted in production wiring.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_is_usable_behind_the_trait_object() {
        // The scheduler holds an `Arc<dyn Clock>`; object safety is load-bearing,
        // not incidental.
        let clock: std::sync::Arc<dyn Clock> = std::sync::Arc::new(SystemClock);
        let first = clock.now();
        let second = clock.now();
        assert!(second >= first);
    }
}
