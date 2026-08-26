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
use crate::db::MutationSource;
use crate::events::{ChangeEvent, CHANGE_BUFFER_CAPACITY};
use crate::runner::events::{RunTail, TAIL_CHANNEL_CAPACITY};

#[derive(Clone)]
pub struct ServiceContext {
    pub pool: SqlitePool,
    pub clock: Arc<dyn Clock>,
    /// Public because ADR-0018 fixes this struct's shape, and the shell needs the
    /// sender itself to hand to the MCP server. Prefer [`publish`](Self::publish):
    /// it is where the two rules on publishing are enforced.
    pub changes: broadcast::Sender<ChangeEvent>,
    /// The live run tail (seam-contract D14).
    ///
    /// A second channel rather than a second [`ChangeEvent`] variant, which
    /// ADR-0018 forbids outright: this one carries a payload, because a tail is
    /// a view and not a fact about stored state. Separate from `changes` because
    /// the two differ in frequency — the tail fires many times per turn, and
    /// sharing one bounded broadcast would let a chatty run lag a subscriber
    /// into dropping change events, where a drop actually costs something.
    ///
    /// Prefer [`publish_tail`](Self::publish_tail).
    pub tail: broadcast::Sender<RunTail>,
    /// Which door every mutation made through this context came from
    /// (ADR-0019).
    ///
    /// Ambient for the same reason `changes` is: it is a property of the
    /// subsystem holding the context, not of the call. The shell builds one
    /// [`MutationSource::Ui`] context and hands it to both the scheduler and
    /// the MCP server, each of which re-sources its own clone with
    /// [`with_source`](Self::with_source) at construction.
    pub source: MutationSource,
}

impl ServiceContext {
    /// Wires a context and, with it, both channels.
    ///
    /// They are created here rather than passed in because every clone of this
    /// context must publish to the *same* senders — a second channel is a second
    /// set of subscribers that never hear each other, which shows up as a board
    /// that refreshes for its own writes and not for anyone else's.
    ///
    /// `source` is a parameter rather than a default because every plausible
    /// default is wrong somewhere — [`MutationSource::Ui`] is wrong for the
    /// scheduler, [`MutationSource::System`] is wrong for the shell — and a
    /// field that is wrong by omission is worse than one the compiler makes
    /// the caller name. There is deliberately no `Default` impl.
    pub fn new(pool: SqlitePool, clock: Arc<dyn Clock>, source: MutationSource) -> Self {
        // The receivers are dropped immediately; the senders stay alive on their
        // own and `subscribe` mints receivers on demand. Nothing sent before the
        // first `subscribe` is buffered, which is why a test subscribes first.
        let (changes, _) = broadcast::channel(CHANGE_BUFFER_CAPACITY);
        let (tail, _) = broadcast::channel(TAIL_CHANNEL_CAPACITY);
        Self {
            pool,
            clock,
            changes,
            tail,
            source,
        }
    }

    /// The same context, attributing its mutations to `source` (ADR-0019).
    ///
    /// Called once per subsystem at construction — `scheduler::build` and
    /// `mcp::build` each do it — so the shell hands one context to both and
    /// never thinks about the field again.
    ///
    /// It clones rather than rebuilding, which is the whole point: the clone
    /// keeps the *same* senders, so an MCP write reaches the board's
    /// subscriber. A `with_source` that minted fresh channels would be a card
    /// that never refreshes for anyone else's writes, which is the failure
    /// ADR-0018 exists to prevent — hence the test below.
    pub fn with_source(&self, source: MutationSource) -> Self {
        Self {
            source,
            ..self.clone()
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

    /// A receiver for every live-run snapshot published from here on
    /// (seam-contract D14).
    ///
    /// The shell calls this once in `setup()` and forwards to a `runs:tail`
    /// Tauri event. A subscriber that reports `RecvError::Lagged` **discards and
    /// counts** — it does not recover. There is nothing to recover: a dropped
    /// tail is a line of scrollback that is already on disk in the run's JSONL
    /// transcript, which is the record. Do not build replay for this channel.
    pub fn subscribe_tail(&self) -> broadcast::Receiver<RunTail> {
        self.tail.subscribe()
    }

    /// Announces what a run is doing right now.
    ///
    /// Infallible for the same reason [`publish`](Self::publish) is, and with a
    /// weaker obligation besides: nothing here is a fact about stored state, so
    /// a snapshot nobody heard has cost nothing at all.
    pub fn publish_tail(&self, tail: RunTail) {
        let _ = self.tail.send(tail);
    }
}

impl fmt::Debug for ServiceContext {
    /// Hand-written because [`Clock`] does not require `Debug` — adding that
    /// bound would constrain every implementation for the sake of a line nobody
    /// reads. The receiver counts are the part worth seeing: zero explains why a
    /// publication went nowhere.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceContext")
            .field("source", &self.source)
            .field("subscribers", &self.changes.receiver_count())
            .field("tail_subscribers", &self.tail.receiver_count())
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
        ServiceContext::new(
            test_pool().await,
            Arc::new(TestClock::new(start)),
            MutationSource::Ui,
        )
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
    async fn a_tail_snapshot_reaches_a_subscriber() {
        let ctx = context().await;
        let mut tail = ctx.subscribe_tail();

        ctx.publish_tail(snapshot(1));

        assert_eq!(
            tail.recv().await.expect("the sender is still alive"),
            snapshot(1)
        );
    }

    #[tokio::test]
    async fn publishing_a_tail_with_nobody_watching_is_not_an_error() {
        let ctx = context().await;

        assert!(ctx.tail.send(snapshot(1)).is_err());
        ctx.publish_tail(snapshot(1));
    }

    #[tokio::test]
    async fn a_tail_subscriber_that_falls_behind_loses_the_snapshots_rather_than_replaying_them() {
        // Seam-contract D14 rule 1. A `ChangeEvent` drop means "re-read"; a tail
        // drop means the user missed a line of scrollback that is already on
        // disk in the transcript. The receiver resumes at the newest snapshot
        // still buffered — which is the current state anyway — and the ones in
        // between are simply gone.
        let ctx = context().await;
        let mut behind = ctx.subscribe_tail();

        for elapsed in 0..=TAIL_CHANNEL_CAPACITY {
            ctx.publish_tail(snapshot(elapsed as i64));
        }

        assert_eq!(
            behind
                .recv()
                .await
                .expect_err("the receiver has fallen behind"),
            RecvError::Lagged(1)
        );
        assert_eq!(
            behind.recv().await.expect("the sender is still alive"),
            snapshot(1)
        );
    }

    #[tokio::test]
    async fn a_chatty_tail_does_not_cost_a_change_subscriber_its_events() {
        // The whole reason D14 puts the tail on its own channel. One shared
        // bounded broadcast would let a run this talkative evict a change event,
        // and a dropped change event *does* have a consequence: a card that
        // stops refreshing until the next mutation.
        let ctx = context().await;
        let mut changes = ctx.subscribe();

        for elapsed in 0..TAIL_CHANNEL_CAPACITY * 4 {
            ctx.publish_tail(snapshot(elapsed as i64));
        }
        ctx.publish(ChangeEvent::runs(["run".to_string()]));

        assert_eq!(
            changes.recv().await.expect("the change event survived"),
            ChangeEvent::runs(["run".to_string()])
        );
    }

    fn snapshot(elapsed_ms: i64) -> RunTail {
        RunTail {
            run_id: "run".to_string(),
            elapsed_ms,
            turns: 0,
            current_tool: None,
            last_assistant_text: None,
        }
    }

    #[tokio::test]
    async fn with_source_publishes_to_the_original_subscribers() {
        // The ADR-0018 guarantee `with_source` must not break. Every subsystem
        // re-sources its own clone at construction, so if this cloned the
        // struct but minted new channels, a task created over MCP would never
        // reach the board — the exact requirement ADR-0006 states.
        let ctx = context().await;
        let mut changes = ctx.subscribe();

        ctx.with_source(MutationSource::Mcp)
            .publish(ChangeEvent::tasks(["written-over-mcp".to_string()]));

        assert_eq!(
            changes.recv().await.expect("the sender is still alive"),
            ChangeEvent::tasks(["written-over-mcp".to_string()])
        );
    }

    #[tokio::test]
    async fn with_source_changes_only_the_source() {
        let ctx = context().await;

        let scheduler = ctx.with_source(MutationSource::System);

        assert_eq!(scheduler.source, MutationSource::System);
        assert_eq!(ctx.source, MutationSource::Ui, "the original is untouched");
        assert_eq!(scheduler.clock.now(), ctx.clock.now());
        assert!(
            scheduler.changes.same_channel(&ctx.changes),
            "the clone must publish on the original's channel"
        );
        assert!(scheduler.tail.same_channel(&ctx.tail));
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
