//! The review-and-fix loop, end to end (ADR-0017, task 021).
//!
//! The CLI is `testing::FakeCli` replaying recorded streams; a review or fix
//! that writes back does so over the real run-scoped HTTP route, as the
//! planner's write-back does in `runner_strategy.rs`. Git runs against real
//! repositories in temporary directories, and the clock is the harness's.
//! Nothing sleeps: the `timeout`s are failure bounds, not waits.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use pretty_assertions::assert_eq;
use rimaia_core::board::BoardPort;
use rimaia_core::db::{BoardColumn, Run, RunKind, RunState, RunStatus};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::{run_task, RunRequest, RunTrigger, RunnerConfig};
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::board::claim_run;
use rimaia_core::testing::{FakeCli, TempRepo, TestContext};
use rimaia_core::{AppPaths, ChangeEvent};
use tempfile::TempDir;

/// A failure bound for anything that waits on a child.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Off by default
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_successful_implementation_with_the_loop_off_lands_exactly_as_before() {
    // A golden taken from the code before task 021's first commit: one row,
    // and the publications the run makes from its claim to its close, in
    // order. A loop that is off must not add, drop or reorder one of them.
    let mut fixture = Fixture::new().await;
    let config = fixture.config();
    let board = fixture.board(&config);
    let claim = claim_run(board.as_ref(), &fixture.task_id, RunTrigger::Queued, false)
        .await
        .expect("claim the task");
    drain(&mut fixture.harness);

    let run = fixture.run_claimed(board.as_ref(), &config, claim).await;

    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, RunKind::Implementation);
    assert_eq!(rows[0].status, RunStatus::Succeeded);
    assert_eq!(rows[0].attempt, 1);
    assert_eq!(run.id, rows[0].id);

    let task = fixture.task_id.clone();
    assert_eq!(
        drain(&mut fixture.harness),
        vec![
            // `worktree::prepare` records the branch it created.
            ChangeEvent::tasks([task.clone()]),
            // `start_run`.
            ChangeEvent::runs([run.id.clone()]),
            ChangeEvent::tasks([task.clone()]),
            // `finish_run`: the row, then the task it lands.
            ChangeEvent::runs([run.id.clone()]),
            ChangeEvent::tasks([task.clone()]),
            ChangeEvent::tasks([task.clone()]),
            ChangeEvent::tasks([task.clone()]),
        ],
    );

    let detail = fixture.detail().await;
    assert_eq!(detail.task.column, BoardColumn::InReview);
    assert_eq!(detail.task.run_state, RunState::Idle);
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// Every publication waiting on the harness's receiver, oldest first.
fn drain(harness: &mut TestContext) -> Vec<ChangeEvent> {
    let mut events = Vec::new();
    while let Ok(event) = harness.changes.try_recv() {
        events.push(event);
    }
    events
}

struct Fixture {
    harness: TestContext,
    paths: AppPaths,
    task_id: String,
    cli: FakeCli,
    /// Held for their `Drop`; the paths above point inside them.
    _data: TempDir,
    _repository: TempRepo,
}

impl Fixture {
    async fn new() -> Self {
        let harness = TestContext::new().await;
        let data = tempfile::Builder::new()
            .prefix("rimaia-data-")
            .tempdir()
            .expect("temp dir for the app data directory");
        let paths = AppPaths::new(data.path());
        paths.create_all().expect("the app data directories");

        let repository = TempRepo::init();
        let registered = repo::register(
            &harness.context,
            &paths.worktrees_dir(),
            NewRepository {
                path: repository.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register a test repository");
        repo::set_allow_unattended_runs(&harness.context, &registered.id, true)
            .await
            .expect("ADR-0012's per-repository opt-in");

        let task_id = tasks::create_task(
            &harness.context,
            NewTask {
                repository_id: registered.id,
                title: "Alpha".to_string(),
                plan: Some("1. Implement Alpha\n2. Test it".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a ready task")
        .id;

        Self {
            harness,
            paths,
            task_id,
            cli: FakeCli::new(),
            _data: data,
            _repository: repository,
        }
    }

    fn config(&self) -> RunnerConfig {
        RunnerConfig {
            program: self.cli.program(),
            ..RunnerConfig::default()
        }
    }

    fn board(&self, config: &RunnerConfig) -> Arc<dyn BoardPort> {
        self.harness.board(&self.paths, config)
    }

    async fn run_claimed(
        &self,
        board: &dyn BoardPort,
        config: &RunnerConfig,
        claim: rimaia_core::board::Claim,
    ) -> Run {
        tokio::time::timeout(
            TEST_TIMEOUT,
            run_task(
                board,
                &self.harness.context,
                &self.paths,
                config,
                claim,
                RunRequest::default(),
            ),
        )
        .await
        .expect("a run must finish inside the test timeout")
        .expect("the run completes")
    }

    async fn rows(&self) -> Vec<Run> {
        rimaia_core::runs::list_runs_for_task(&self.harness.context, &self.task_id)
            .await
            .expect("read the runs")
    }

    async fn detail(&self) -> tasks::TaskDetail {
        tasks::get_task(&self.harness.context, &self.task_id)
            .await
            .expect("read the task")
    }
}
