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

use crate::board::{lease, BoardPort, InProcessBoard, LeaseTerm, RunContext};
use crate::context::{ServiceContext, TeamScope};
use crate::db::MutationSource;
use crate::events::ChangeEvent;
use crate::identity::{ensure_solo, SoloIdentity};
use crate::machine::MachineContext;
use crate::paths::AppPaths;
use crate::runner::RunnerConfig;
use crate::testing::machine::MemoryMachine;
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
    /// The board's solo team, user and runner, from the same
    /// [`ensure_solo`] the shell calls, so a test reads the ids rather than
    /// querying for them.
    pub solo: SoloIdentity,
    /// This machine's own state, over a [`MemoryMachine`]. See
    /// [`machine`](Self::machine).
    machine: MachineContext,
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
        Self::over(test_pool().await, TestClock::new(start)).await
    }

    /// The same, over a board database in the file `db_file`, through the
    /// production [`db::connect`](crate::db::connect) pool: more than one
    /// connection, so two callers really do run at once (task 043's race
    /// cases). The in-memory pool has one connection and would serialise
    /// them, proving nothing.
    pub async fn over_file(db_file: &std::path::Path) -> Self {
        let pool = crate::db::connect(db_file)
            .await
            .expect("open a file-backed board");
        crate::db::migrate(&pool)
            .await
            .expect("migrate a file-backed board");
        Self::over(pool, TestClock::new(test_epoch())).await
    }

    async fn over(pool: sqlx::SqlitePool, clock: TestClock) -> Self {
        // Through the shell's own path rather than rows written here, so every
        // service test runs against the identity a first launch creates (D28
        // part 3).
        let solo = ensure_solo(&pool, &clock)
            .await
            .expect("a fresh board must get a solo identity");
        // `Ui` because a service test stands in for the board unless it says
        // otherwise; a test about the MCP path re-sources with `with_source`,
        // exactly as `mcp::build` does (ADR-0019). Scoped and acting exactly as
        // the shell's context is.
        let context = ServiceContext::new(
            pool,
            Arc::new(clock.clone()),
            MutationSource::Ui,
            TeamScope::one(solo.team_id.clone()),
            solo.user_id.clone(),
        );
        let changes = context.subscribe();
        // The shell's shape (task 041): the board's own sender and the solo
        // team, so a machine write reaches `changes` exactly as it reaches the
        // window, and the same clock the board reads.
        let machine = MachineContext {
            store: Arc::new(MemoryMachine::new()),
            clock: Arc::new(clock.clone()),
            changes: context.changes.clone(),
            event_team: solo.team_id.clone(),
        };

        Self {
            context,
            changes,
            clock,
            solo,
            machine,
        }
    }

    /// The machine context over this test's [`MemoryMachine`], sharing the
    /// test's clock, change channel and solo team (task 041).
    ///
    /// One store per harness, so what a test writes through it is what the
    /// code under test reads back. A test that asserts on `runner.db` itself
    /// lives in `crates/runner/tests/` instead; one here asserts on behaviour,
    /// and says "the machine store".
    pub fn machine(&self) -> &MachineContext {
        &self.machine
    }

    /// Where this machine's store records `task_id`'s worktree, if anywhere:
    /// what a test reads where it read `tasks.worktree_path` before task 066.
    pub async fn worktree_path(&self, task_id: &str) -> Option<String> {
        crate::machine::local::worktree_path(&self.machine, task_id)
            .await
            .expect("the machine store answers")
    }

    /// [`worktree::prepare`](crate::worktree::prepare) for `task_id`, the way
    /// a runner calls it: under the solo lease, recording the path in this
    /// test's machine store and the branch through an in-process board port
    /// over this test's own context (task 066).
    ///
    /// The context `prepare` builds from, base included, is the board's
    /// [`preview`](BoardPort::preview) of the task at the call (task 044). A
    /// test whose subject is the base takes its context from `preview` or a
    /// claim itself and calls [`prepare_worktree_from`](Self::prepare_worktree_from).
    ///
    /// The branch is fenced by a lease (task 043). A task the solo runner
    /// already holds is prepared under its own lease; otherwise the test
    /// holds one with no edge for the call and ends it after, so a test that
    /// only wants a worktree does not have to claim the task, which would move
    /// its run state.
    pub async fn prepare_worktree(
        &self,
        task_id: &str,
    ) -> crate::Result<crate::worktree::Worktree> {
        let context = self.preview_board().preview(task_id).await?;
        self.prepare_worktree_from(&context).await
    }

    /// [`prepare_worktree`](Self::prepare_worktree), from a context the test
    /// already holds: the task, the repository and the base all come from it.
    pub async fn prepare_worktree_from(
        &self,
        context: &RunContext,
    ) -> crate::Result<crate::worktree::Worktree> {
        let board = self.preview_board();
        let task_id = context.task.task.id.as_str();
        let held = lease::state_of(&self.context, task_id).await?.lease;
        if let Some(held) = held {
            let lease =
                crate::board::LeaseRef::new(task_id, held.generation, self.solo.team_id.clone());
            return crate::worktree::prepare(&self.machine, &board, &lease, context).await;
        }

        let lease = lease::grant_for_test(&self.context, task_id, &self.solo.runner_id).await?;
        let prepared = crate::worktree::prepare(&self.machine, &board, &lease, context).await;
        lease::end_for_test(&self.context, &lease).await?;
        prepared
    }

    /// The in-process board [`prepare_worktree`](Self::prepare_worktree)
    /// previews and records the branch through.
    fn preview_board(&self) -> InProcessBoard {
        InProcessBoard::new(
            self.context.clone(),
            AppPaths::new(std::env::temp_dir()),
            RunnerConfig::default().provider,
            self.solo.runner_id.clone(),
            LeaseTerm::Never,
        )
    }

    /// The board port over this test's own context (seam-contract D31 point
    /// 8), so what a test arranges through `context` is what the runner reads
    /// through the port.
    ///
    /// The provider comes from `config` rather than being a parameter of its
    /// own: a board built for another provider than the runner it serves hands
    /// the planner the wrong catalogue, and nothing fails loudly (task 036's
    /// Traps). Taking it from the config is what makes that unwritable here.
    pub fn board(&self, paths: &AppPaths, config: &RunnerConfig) -> Arc<dyn BoardPort> {
        Arc::new(InProcessBoard::new(
            self.context.clone(),
            paths.clone(),
            config.provider.clone(),
            self.solo.runner_id.clone(),
            LeaseTerm::Never,
        ))
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

        harness
            .context
            .publish(ChangeEvent::settings(harness.solo.team_id.clone()));

        assert_eq!(
            harness.changes.try_recv().expect("a waiting publication"),
            ChangeEvent::settings(harness.solo.team_id.clone())
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
