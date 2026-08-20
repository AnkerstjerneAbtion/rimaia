//! A [`ServiceContext`] wired for a test, listening to itself.
//!
//! A service test asserts two things about every mutation: what it wrote, and
//! that it published (ADR-0018). The second half has a trap — broadcast delivers
//! only to receivers that existed when the value was sent, so a test that calls
//! the service and *then* subscribes sees nothing and cannot tell that apart from
//! a service that forgot to publish. This assembles the pieces in the order that
//! makes the assertion possible.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::broadcast::Receiver;

use crate::context::ServiceContext;
use crate::events::ChangeEvent;
use crate::testing::{test_pool, TestClock};

/// Where a [`TestContext`]'s clock starts unless the test says otherwise.
///
/// A fixed instant rather than `Utc::now()`, so a stamped `updated_at` is an
/// exact value a test can assert and a failure message reads the same in
/// December as it does today.
pub fn test_epoch() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-08-20T02:00:00Z")
        .expect("the test epoch must be valid RFC 3339")
        .with_timezone(&Utc)
}

pub struct TestContext {
    /// What the code under test takes.
    pub context: ServiceContext,
    /// Subscribed before the test can call anything, so a publication made by
    /// the call under test is waiting here rather than lost.
    pub changes: Receiver<ChangeEvent>,
    /// The same instant the context's `Arc<dyn Clock>` reads — advance this and
    /// the code under test sees the new time.
    pub clock: TestClock,
}

impl TestContext {
    /// A migrated in-memory database, a clock stopped at [`test_epoch`], and a
    /// live subscriber.
    pub async fn new() -> Self {
        Self::starting_at(test_epoch()).await
    }

    /// The same, with the clock pinned somewhere else — for a test whose subject
    /// is an absolute time, such as a usage limit's epoch `resetsAt`.
    pub async fn starting_at(start: DateTime<Utc>) -> Self {
        let clock = TestClock::new(start);
        let context = ServiceContext::new(test_pool().await, Arc::new(clock.clone()));
        let changes = context.subscribe();

        Self {
            context,
            changes,
            clock,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn the_receiver_is_listening_before_the_test_calls_anything() {
        let mut harness = TestContext::new().await;

        harness.context.publish(ChangeEvent::Settings);

        assert_eq!(
            harness.changes.try_recv().expect("a waiting publication"),
            ChangeEvent::Settings
        );
    }

    #[tokio::test]
    async fn moving_the_handle_moves_the_clock_the_service_reads() {
        let harness = TestContext::new().await;

        harness.clock.advance(Duration::minutes(15));

        assert_eq!(
            harness.context.clock.now(),
            test_epoch() + Duration::minutes(15)
        );
    }

    #[tokio::test]
    async fn the_pool_is_private_to_one_harness() {
        let first = TestContext::new().await;
        let second = TestContext::new().await;

        sqlx::query("CREATE TABLE only_in_the_first (id INTEGER PRIMARY KEY)")
            .execute(&first.context.pool)
            .await
            .expect("create a table");

        let leaked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE name = 'only_in_the_first'",
        )
        .fetch_one(&second.context.pool)
        .await
        .expect("query the schema");

        assert_eq!(leaked, 0);
    }
}
