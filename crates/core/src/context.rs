//! The ambient capabilities of being a service (ADR-0018).
//!
//! Services take `&ServiceContext` rather than a bare `&SqlitePool`, because the
//! store, the clock and the change sender are all the same kind of thing: not
//! arguments to an operation, but the environment an operation runs in.
//! Publishing in particular is an ambient capability of *being* a service, like
//! knowing the time — not a parameter each caller decides to pass, which would
//! make "did this mutation notify anyone?" a property of the call site instead of
//! the rule.
//!
//! Nothing here is a shell type. Task 004's constraint — no `AppHandle`, no
//! `tauri::State`, nothing the MCP server cannot construct — is intact and still
//! compiler-enforced by the crate split (ADR-0015), and the UI still learns about
//! a task written over MCP without polling. The Tauri shell and the MCP server
//! build the same struct, so both go through one implementation of every rule
//! (ADR-0006).
//!
//! Cloning is cheap by design — the pool and the sender are handles, the clock is
//! an `Arc` — so a context is passed by clone into a spawned run without anyone
//! reaching for a lifetime.

use std::fmt;
use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::broadcast;

use crate::clock::Clock;
use crate::events::{ChangeEvent, CHANGE_BUFFER_CAPACITY};

#[derive(Clone)]
pub struct ServiceContext {
    pub pool: SqlitePool,
    pub clock: Arc<dyn Clock>,
    /// Public because ADR-0018 fixes this struct's shape, and the shell needs the
    /// sender itself to hand to the MCP server. Prefer [`publish`](Self::publish):
    /// it is where the two rules on publishing are enforced.
    pub changes: broadcast::Sender<ChangeEvent>,
}

impl ServiceContext {
    /// Wires a context and, with it, one change channel.
    ///
    /// The channel is created here rather than passed in because every clone of
    /// this context must publish to the *same* sender — a second channel is a
    /// second set of subscribers that never hear each other, which shows up as a
    /// board that refreshes for its own writes and not for anyone else's.
    pub fn new(pool: SqlitePool, clock: Arc<dyn Clock>) -> Self {
        // The receiver is dropped immediately; the sender stays alive on its own
        // and `subscribe` mints receivers on demand. Nothing sent before the
        // first `subscribe` is buffered, which is why a test subscribes first.
        let (changes, _) = broadcast::channel(CHANGE_BUFFER_CAPACITY);
        Self {
            pool,
            clock,
            changes,
        }
    }

    /// A receiver for every event published from here on.
    ///
    /// The shell calls this once in `setup()` and forwards for the life of the
    /// app; the MCP server and the scheduler each call it too. There is no
    /// registration and no ordering between them — a subscriber that does not
    /// care about a variant ignores it.
    pub fn subscribe(&self) -> broadcast::Receiver<ChangeEvent> {
        self.changes.subscribe()
    }

    /// Announces a mutation.
    ///
    /// **Call this after the transaction commits.** A subscriber's reaction is to
    /// re-read, and under WAL an uncommitted write is invisible to the other
    /// connections in the pool — a notification sent from inside the transaction
    /// is a subscriber reading the old row and never being told again.
    ///
    /// Infallible on purpose. `broadcast::Sender::send` reports `Err` when nobody
    /// is subscribed, which is the normal state of a `cargo test -p rimaia-core`
    /// run and of an app shutting down; a mutation that committed must never be
    /// reported as failed because nothing was listening.
    pub fn publish(&self, event: ChangeEvent) {
        // See `ChangeEvent::is_empty`: an empty id list on the wire is the
        // forwarder's "re-read everything" signal, not a service's to send.
        if event.is_empty() {
            return;
        }

        let _ = self.changes.send(event);
    }
}

impl fmt::Debug for ServiceContext {
    /// Hand-written because [`Clock`] does not require `Debug` — adding that
    /// bound would constrain every implementation for the sake of a line nobody
    /// reads. The receiver count is the part worth seeing: zero explains why a
    /// publication went nowhere.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceContext")
            .field("subscribers", &self.changes.receiver_count())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_pool, TestClock};
    use chrono::{DateTime, Utc};
    use pretty_assertions::assert_eq;
    use tokio::sync::broadcast::error::RecvError;

    async fn context() -> ServiceContext {
        let start: DateTime<Utc> = DateTime::parse_from_rfc3339("2026-08-20T02:00:00Z")
            .expect("test timestamp must be valid RFC 3339")
            .with_timezone(&Utc);
        ServiceContext::new(test_pool().await, Arc::new(TestClock::new(start)))
    }

    fn task_ids(event: &ChangeEvent) -> Vec<String> {
        match event {
            ChangeEvent::Tasks(ids) => ids.to_vec(),
            other => panic!("expected a task change, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_publication_reaches_a_subscriber() {
        let ctx = context().await;
        let mut changes = ctx.subscribe();

        ctx.publish(ChangeEvent::tasks(["moved".to_string()]));

        assert_eq!(
            changes.recv().await.expect("the sender is still alive"),
            ChangeEvent::tasks(["moved".to_string()])
        );
    }

    #[tokio::test]
    async fn publishing_with_nobody_listening_is_not_an_error() {
        // The rule that stops a committed mutation being reported as failed. The
        // raw sender's own answer is the control: it *does* report an error, and
        // `publish` is the thing that swallows it.
        let ctx = context().await;

        assert!(ctx.changes.send(ChangeEvent::Settings).is_err());
        ctx.publish(ChangeEvent::Settings);
    }

    #[tokio::test]
    async fn every_subscriber_receives_the_same_publication() {
        let ctx = context().await;
        let mut board = ctx.subscribe();
        let mut mcp = ctx.subscribe();

        ctx.publish(ChangeEvent::runs(["run".to_string()]));

        let published = ChangeEvent::runs(["run".to_string()]);
        assert_eq!(board.recv().await.expect("board"), published);
        assert_eq!(mcp.recv().await.expect("mcp"), published);
    }

    #[tokio::test]
    async fn a_receiver_that_falls_behind_is_told_how_much_it_missed() {
        // One more publication than the buffer holds, so the oldest is evicted
        // before the receiver ever reads. It must hear about the drop rather than
        // silently continue with a hole: the shell's forwarder answers `Lagged`
        // with a wholesale re-read, and that recovery only happens if it is told.
        let ctx = context().await;
        let mut behind = ctx.subscribe();

        for sequence in 0..=CHANGE_BUFFER_CAPACITY {
            ctx.publish(ChangeEvent::tasks([sequence.to_string()]));
        }

        let lag = behind
            .recv()
            .await
            .expect_err("the receiver has fallen behind");
        assert_eq!(lag, RecvError::Lagged(1));

        // And it resumes at the oldest event still buffered, rather than at the
        // one it lost.
        let resumed = behind.recv().await.expect("the sender is still alive");
        assert_eq!(task_ids(&resumed), vec!["1".to_string()]);
    }

    #[tokio::test]
    async fn an_empty_id_list_is_never_published() {
        let ctx = context().await;
        let mut changes = ctx.subscribe();

        ctx.publish(ChangeEvent::tasks([]));
        ctx.publish(ChangeEvent::Settings);

        // The settings event, not the suppressed one, is what is waiting.
        assert_eq!(
            changes.try_recv().expect("the settings event"),
            ChangeEvent::Settings
        );
    }

    #[tokio::test]
    async fn a_clone_publishes_to_the_original_subscribers() {
        // Services are handed clones — one per spawned run. A clone that
        // published somewhere else would be a card that stops refreshing.
        let ctx = context().await;
        let mut changes = ctx.subscribe();

        ctx.clone()
            .publish(ChangeEvent::repositories(["repo".to_string()]));

        assert_eq!(
            changes.recv().await.expect("the sender is still alive"),
            ChangeEvent::repositories(["repo".to_string()])
        );
    }
}
