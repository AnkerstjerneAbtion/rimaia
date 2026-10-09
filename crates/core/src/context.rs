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
//!
//! ADR-0019 fixed the struct's shape and said a later field is a later record.
//! ADR-0029 is that record for [`scope`](ServiceContext::scope), the teams a
//! request may touch, and ADR-0030 for [`actor`](ServiceContext::actor), the
//! user it acts for. Neither has a default, for `source`'s reason: whoever
//! builds a context is the only one who knows the answer, so the compiler makes
//! them say it.

use std::fmt;
use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::broadcast;

use crate::clock::Clock;
use crate::db::MutationSource;
use crate::error::{Error, Result};
use crate::events::{ChangeEvent, TeamId, UserId, CHANGE_BUFFER_CAPACITY};
use crate::runner::events::{RunTail, TAIL_CHANNEL_CAPACITY};

/// The teams a request may touch (ADR-0029 point 5).
///
/// A set rather than one team, because an entity's id determines its team
/// (ADR-0035 point 2): an MCP token or a signed-in member can reach several
/// teams, and the edge resolves which ones once, before any service runs.
/// Never empty, so "may touch nothing" cannot be mistaken for "unfiltered",
/// and kept sorted and deduplicated, so two scopes naming the same teams
/// compare equal.
///
/// Task 038 carries the scope; task 039 makes every service honour it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TeamScope {
    teams: Arc<[TeamId]>,
}

impl TeamScope {
    /// A scope of exactly one team: solo's, or a request that named one.
    pub fn one(team_id: impl Into<TeamId>) -> Self {
        Self {
            teams: Arc::from([team_id.into()]),
        }
    }

    /// A scope of every team in `teams`, or `Invalid` when there are none.
    pub fn of(teams: impl IntoIterator<Item = TeamId>) -> Result<Self> {
        let mut teams: Vec<TeamId> = teams.into_iter().collect();
        teams.sort();
        teams.dedup();
        if teams.is_empty() {
            return Err(Error::invalid(
                "a request has to be scoped to at least one team",
            ));
        }
        Ok(Self {
            teams: teams.into(),
        })
    }

    pub fn contains(&self, team_id: &str) -> bool {
        self.teams
            .binary_search_by(|team| team.as_str().cmp(team_id))
            .is_ok()
    }

    /// Every team in scope, sorted.
    pub fn teams(&self) -> &[TeamId] {
        &self.teams
    }

    /// The one team in scope, or `Invalid` when there are several.
    ///
    /// For a write that creates a team-owned row with nothing to take the team
    /// from: registering a repository, and a settings write until task 039
    /// gives `settings` a team. Anything with a parent row takes the parent's
    /// team instead, because a scope of several teams cannot say which.
    pub fn sole(&self) -> Result<&TeamId> {
        match self.teams.as_ref() {
            [team] => Ok(team),
            teams => Err(Error::invalid(format!(
                "this request can reach {} teams, so it has to say which one it means",
                teams.len()
            ))),
        }
    }
}

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
    /// The teams this context may touch (ADR-0029 point 5).
    ///
    /// Resolved once, at the edge that built the context: solo's shell from
    /// its [`SoloIdentity`](crate::identity::SoloIdentity), and later a
    /// server's session, an MCP token or a runner's lease. A clone keeps it, so
    /// `mcp::build` and `scheduler::build` inherit the shell's.
    pub scope: TeamScope,
    /// The user every mutation through this context is made for (ADR-0030
    /// point 8).
    ///
    /// A plain user id (seam-contract D10) rather than an enum, because every
    /// writer through task 053 acts for someone: the solo user, a signed-in
    /// caller, a runner's owner. It is recorded on every service span as
    /// `user_id`; task 045's `tasks.created_by` is the first column to store it.
    pub actor: UserId,
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
    /// the caller name. There is deliberately no `Default` impl. `scope` and
    /// `actor` are parameters for the same reason: a context scoped to nobody
    /// in particular is exactly the cross-team read ADR-0029 forbids.
    pub fn new(
        pool: SqlitePool,
        clock: Arc<dyn Clock>,
        source: MutationSource,
        scope: TeamScope,
        actor: UserId,
    ) -> Self {
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
            scope,
            actor,
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

    /// The same context, reaching only the teams in `scope`.
    ///
    /// Clones for [`with_source`](Self::with_source)'s reason: a narrowed
    /// context publishes on the *same* senders, so a write made through it
    /// still reaches every subscriber.
    pub fn with_scope(&self, scope: TeamScope) -> Self {
        Self {
            scope,
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
            .field("scope", &self.scope)
            .field("actor", &self.actor)
            .field("subscribers", &self.changes.receiver_count())
            .field("tail_subscribers", &self.tail.receiver_count())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Change;
    use crate::identity::ensure_solo;
    use crate::testing::{test_pool, TestClock};
    use chrono::{DateTime, Utc};
    use pretty_assertions::assert_eq;
    use tokio::sync::broadcast::error::RecvError;

    async fn context() -> ServiceContext {
        let start: DateTime<Utc> = DateTime::parse_from_rfc3339("2026-08-20T02:00:00Z")
            .expect("test timestamp must be valid RFC 3339")
            .with_timezone(&Utc);
        let pool = test_pool().await;
        let clock = TestClock::new(start);
        let solo = ensure_solo(&pool, &clock)
            .await
            .expect("a fresh board gets a solo identity");
        ServiceContext::new(
            pool,
            Arc::new(clock),
            MutationSource::Ui,
            TeamScope::one(solo.team_id),
            solo.user_id,
        )
    }

    /// The team every event in these tests is published for: the solo one.
    fn team(ctx: &ServiceContext) -> TeamId {
        ctx.scope
            .sole()
            .expect("a solo context has one team")
            .clone()
    }

    fn task_ids(event: &ChangeEvent) -> Vec<String> {
        match &event.change {
            Change::Tasks(ids) => ids.to_vec(),
            other => panic!("expected a task change, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_publication_reaches_a_subscriber() {
        let ctx = context().await;
        let mut changes = ctx.subscribe();

        ctx.publish(ChangeEvent::tasks(team(&ctx), ["moved".to_string()]));

        assert_eq!(
            changes.recv().await.expect("the sender is still alive"),
            ChangeEvent::tasks(team(&ctx), ["moved".to_string()])
        );
    }

    #[tokio::test]
    async fn publishing_with_nobody_listening_is_not_an_error() {
        // The rule that stops a committed mutation being reported as failed. The
        // raw sender's own answer is the control: it *does* report an error, and
        // `publish` is the thing that swallows it.
        let ctx = context().await;

        assert!(ctx.changes.send(ChangeEvent::settings(team(&ctx))).is_err());
        ctx.publish(ChangeEvent::settings(team(&ctx)));
    }

    #[tokio::test]
    async fn every_subscriber_receives_the_same_publication() {
        let ctx = context().await;
        let mut board = ctx.subscribe();
        let mut mcp = ctx.subscribe();

        ctx.publish(ChangeEvent::runs(team(&ctx), ["run".to_string()]));

        let published = ChangeEvent::runs(team(&ctx), ["run".to_string()]);
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
            ctx.publish(ChangeEvent::tasks(team(&ctx), [sequence.to_string()]));
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

        ctx.publish(ChangeEvent::tasks(team(&ctx), []));
        ctx.publish(ChangeEvent::settings(team(&ctx)));

        // The settings event, not the suppressed one, is what is waiting.
        assert_eq!(
            changes.try_recv().expect("the settings event"),
            ChangeEvent::settings(team(&ctx))
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
        ctx.publish(ChangeEvent::runs(team(&ctx), ["run".to_string()]));

        assert_eq!(
            changes.recv().await.expect("the change event survived"),
            ChangeEvent::runs(team(&ctx), ["run".to_string()])
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
            .publish(ChangeEvent::tasks(
                team(&ctx),
                ["written-over-mcp".to_string()],
            ));

        assert_eq!(
            changes.recv().await.expect("the sender is still alive"),
            ChangeEvent::tasks(team(&ctx), ["written-over-mcp".to_string()])
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
            .publish(ChangeEvent::repositories(team(&ctx), ["repo".to_string()]));

        assert_eq!(
            changes.recv().await.expect("the sender is still alive"),
            ChangeEvent::repositories(team(&ctx), ["repo".to_string()])
        );
    }

    const OTHER_TEAM: &str = "3f2b1c00-0000-4000-8000-0000000000b2";

    #[tokio::test]
    async fn with_scope_publishes_to_the_original_subscribers() {
        // `with_source`'s guarantee, for the field 039's doors narrow: a
        // context scoped down to one team still publishes where the board
        // listens.
        let ctx = context().await;
        let mut changes = ctx.subscribe();

        ctx.with_scope(TeamScope::one(OTHER_TEAM))
            .publish(ChangeEvent::tasks(
                OTHER_TEAM.to_string(),
                ["in-another-team".to_string()],
            ));

        assert_eq!(
            changes.recv().await.expect("the sender is still alive"),
            ChangeEvent::tasks(OTHER_TEAM.to_string(), ["in-another-team".to_string()])
        );
    }

    #[tokio::test]
    async fn with_scope_changes_only_the_scope() {
        let ctx = context().await;
        let both = TeamScope::of([team(&ctx), OTHER_TEAM.to_string()]).expect("two teams");

        let narrowed = ctx.with_scope(both.clone());

        assert_eq!(narrowed.scope, both);
        assert_eq!(
            ctx.scope,
            TeamScope::one(team(&ctx)),
            "the original is untouched"
        );
        assert_eq!(narrowed.actor, ctx.actor);
        assert_eq!(narrowed.source, ctx.source);
        assert_eq!(narrowed.clock.now(), ctx.clock.now());
        assert!(narrowed.changes.same_channel(&ctx.changes));
        assert!(narrowed.tail.same_channel(&ctx.tail));
    }

    #[test]
    fn a_scope_names_at_least_one_team() {
        let error = TeamScope::of([]).expect_err("an empty scope");
        assert_eq!(error.code(), crate::ErrorCode::Invalid);

        let scope = TeamScope::of([
            OTHER_TEAM.to_string(),
            "3f2b1c00-0000-4000-8000-0000000000a1".to_string(),
            OTHER_TEAM.to_string(),
        ])
        .expect("two teams, one named twice");
        assert_eq!(
            scope.teams(),
            [
                "3f2b1c00-0000-4000-8000-0000000000a1".to_string(),
                OTHER_TEAM.to_string()
            ],
            "sorted and deduplicated"
        );
        assert!(scope.contains(OTHER_TEAM));
        assert!(!scope.contains("3f2b1c00-0000-4000-8000-0000000000c3"));
    }

    #[test]
    fn a_scope_of_two_teams_has_no_sole_team() {
        let one = TeamScope::one(OTHER_TEAM);
        assert_eq!(one.sole().expect("one team"), OTHER_TEAM);

        let two = TeamScope::of([
            OTHER_TEAM.to_string(),
            "3f2b1c00-0000-4000-8000-0000000000a1".to_string(),
        ])
        .expect("two teams");
        assert_eq!(
            two.sole().expect_err("two teams have no sole one").code(),
            crate::ErrorCode::Invalid
        );
    }

    #[tokio::test]
    async fn the_debug_output_names_the_scope_and_the_actor() {
        // Hand-written, so a field it does not list is silently missing.
        let ctx = context().await;

        let printed = format!("{ctx:?}");

        assert!(
            printed.contains(&format!("scope: {:?}", ctx.scope)),
            "{printed}"
        );
        assert!(
            printed.contains(&format!("actor: {:?}", ctx.actor)),
            "{printed}"
        );
    }
}
