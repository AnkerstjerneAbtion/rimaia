//! The runner re-checks its own consent before every spawn (ADR-0032 point 4,
//! task 045 Scope 7).
//!
//! 042 lists only consented checkouts in a `Next` claim's `repositories`, and
//! the starters judge consent on a preview before they claim. Neither is the
//! decision: the board is not trusted to have honoured the list, and a person
//! can withdraw consent between a claim and a spawn. So at the last point
//! before the agent process starts, in the process that spawns it, the runner
//! reads `checkouts.unattended_consent` again. Each case here has the board
//! claim the task anyway, and asserts that the fixture CLI is never started
//! for the refused phase and that the lease is gone.
//!
//! Real git in a `TempDir`, the recorded `success` stream, and a real bound
//! MCP server, because the planner and the review are composed against one.

// The fake CLI is a shell script, which Windows will not execute.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::time::Duration;

use pretty_assertions::assert_eq;
use rimaia_core::board::{BoardPort, Claim, ClaimTarget, FreeCapacity, OwnerPresence};
use rimaia_core::db::{BoardColumn, ExitClass, RunKind, RunState, StrategyMode};
use rimaia_core::mcp::{self, McpHandle, RunHandles};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::review_loop::{config as review_config, UnreviewedReason, Verdict};
use rimaia_core::runner::events::RunTail;
use rimaia_core::runner::provider::ClaudeProvider;
use rimaia_core::runner::strategy::{claim_for_planning, plan_claimed, PlanOutcome};
use rimaia_core::runner::{run_task, CancelSignal, RunRequest, RunTrigger, RunnerConfig};
use rimaia_core::scheduler::{InFlight, SlotOwner};
use rimaia_core::tasks::{self, NewTask, TaskPatch};
use rimaia_core::testing::{self, open_gate, FakeCli, TempRepo, TestContext};
use rimaia_core::{AppPaths, ServiceContext};
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::broadcast::Receiver;

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn a_runner_refuses_to_spawn_where_it_never_consented_even_when_the_board_claims_it() {
    the_queue_refuses().await;
    run_now_refuses().await;
    plan_now_refuses().await;
    a_phase_started_by_continue_refuses().await;
}

/// A `Next` claim that lists a repository this runner never consented to: a
/// board that did not honour `repositories` would grant exactly this.
async fn the_queue_refuses() {
    let f = Fixture::new().await;
    let task = f.task("From the queue").await;
    let config = f.config();
    let board = f.harness.board(&f.paths, &config);

    let claim = board
        .claim(ClaimTarget::Next {
            capacity: FreeCapacity {
                total: 1,
                per_repository: BTreeMap::from([(f.repository_id.clone(), 1)]),
            },
            repositories: vec![f.repository_id.clone()],
            wait: Duration::ZERO,
            ceiling: Default::default(),
        })
        .await
        .expect("claim")
        .expect("the board claims the listed repository's task");
    assert_eq!(claim.lease.task_id, task);

    let error = f.run(board.as_ref(), &config, claim).await;
    f.assert_refused(&task, &error.to_string()).await;
}

/// Run now, claimed straight through the port rather than through the
/// starter, whose preview would have refused before the claim.
async fn run_now_refuses() {
    let f = Fixture::new().await;
    let task = f.task("Run now").await;
    let config = f.config();
    let board = f.harness.board(&f.paths, &config);

    let claim = board
        .claim(ClaimTarget::Run {
            task_id: task.clone(),
            trigger: RunTrigger::Manual,
            continue_session: false,
            ceiling: Default::default(),
        })
        .await
        .expect("claim")
        .expect("the board claims it");

    let error = f.run(board.as_ref(), &config, claim).await;
    f.assert_refused(&task, &error.to_string()).await;
}

/// Plan now, through its real door, with consent withdrawn between the claim
/// and the spawn: the starter judged it on a preview, and the board does not
/// read a runner's consent at all.
async fn plan_now_refuses() {
    let f = Fixture::new().await;
    let task = f.task("Plan now").await;
    tasks::update_task(
        f.ctx(),
        &task,
        TaskPatch {
            strategy_mode: Some(StrategyMode::Planned),
            ..TaskPatch::default()
        },
    )
    .await
    .expect("ADR-0016's planned mode");
    f.consent(true).await;

    let config = f.config();
    let board = f.harness.board(&f.paths, &config);
    let in_flight = InFlight::new();
    let claim = claim_for_planning(
        f.harness.starter(OwnerPresence::AtRunner),
        board.as_ref(),
        f.harness.machine(),
        &in_flight,
        &task,
        SlotOwner::Manual,
    )
    .await
    .expect("Plan now")
    .unwrap_or_else(|skip| panic!("the planner is claimed: {}", skip.message()));
    f.consent(false).await;

    let outcome = tokio::time::timeout(
        TEST_TIMEOUT,
        plan_claimed(
            board.as_ref(),
            f.harness.machine(),
            &f.paths,
            &config,
            claim,
        ),
    )
    .await
    .expect("the planner must finish inside the test timeout")
    .expect("a refused planner is an outcome, not an error");

    let PlanOutcome::Failed(reason) = outcome else {
        panic!("the planner is refused, not {outcome:?}");
    };
    assert_eq!(reason, f.refusal());
    assert_eq!(f.cli.started(), Vec::<String>::new(), "nothing spawned");
    assert_eq!(f.leases().await, 0, "the planner's claim went back");
    assert_eq!(
        f.detail(&task).await.task.run_state,
        RunState::Idle,
        "a planner moves no run state"
    );
}

/// The implementation runs with consent; it is withdrawn while the agent
/// works, and the review the board's `Continue` starts is never spawned.
async fn a_phase_started_by_continue_refuses() {
    let f = Fixture::new().await;
    let task = f.task("Continue").await;
    review_config::set_review_settings(
        f.ctx(),
        &ClaudeProvider,
        "Run /review.",
        json!({ "enabled": "on_cost_acknowledged" }),
    )
    .await
    .expect("turn the loop on");
    f.consent(true).await;
    let gate = f.cli.gates(&task, "success", 5);

    let config = f.config();
    let board = f.harness.board(&f.paths, &config);
    let claim = board
        .claim(ClaimTarget::Run {
            task_id: task.clone(),
            trigger: RunTrigger::Queued,
            continue_session: false,
            ceiling: Default::default(),
        })
        .await
        .expect("claim")
        .expect("unclaimed");

    let tail = f.ctx().subscribe_tail();
    let (run, ()) = tokio::join!(
        tokio::time::timeout(
            TEST_TIMEOUT,
            run_task(
                board.as_ref(),
                f.harness.machine(),
                &f.paths,
                &config,
                claim,
                RunRequest {
                    cancel: CancelSignal::new(),
                    in_flight: None,
                },
            ),
        ),
        async {
            once_the_run_is_live(tail).await;
            f.consent(false).await;
            open_gate(&gate);
        },
    );
    run.expect("the run must finish inside the test timeout")
        .expect("the implementation is recorded");

    assert_eq!(f.cli.attempts(&task), 1, "only the implementation spawned");
    let mut rows = rimaia_core::runs::list_runs_for_task(f.ctx(), &task)
        .await
        .expect("the runs");
    rows.sort_by_key(|row| row.attempt);
    let kinds: Vec<RunKind> = rows.iter().map(|row| row.kind).collect();
    assert_eq!(kinds, vec![RunKind::Implementation, RunKind::Review]);
    assert_eq!(rows[0].exit_class, Some(ExitClass::Success));
    assert_eq!(rows[1].exit_class, Some(ExitClass::Fatal));
    assert_eq!(
        rows[1].error_message.as_deref(),
        Some(f.refusal().as_str()),
        "the refusal is the review's recorded reason"
    );
    assert_eq!(f.leases().await, 0, "the lease is released");
    let detail = f.detail(&task).await;
    assert_eq!(detail.task.column, BoardColumn::InReview);
    assert_eq!(detail.task.run_state, RunState::Idle);
    assert_eq!(
        detail.review_loop.expect("the loop ran").verdict,
        Verdict::Unreviewed {
            reason: UnreviewedReason::ReviewFailed
        }
    );
}

async fn once_the_run_is_live(mut tail: Receiver<RunTail>) {
    tokio::time::timeout(TEST_TIMEOUT, tail.recv())
        .await
        .expect("a run must report itself in flight")
        .expect("the tail sender outlives the run");
}

/// A solo board with one registered repository this runner has not consented
/// to, and a bound MCP server. The team ceiling is not consulted in a
/// personal team, so only the runner's own consent can refuse.
struct Fixture {
    harness: TestContext,
    paths: AppPaths,
    repository_id: String,
    repository_name: String,
    cli: FakeCli,
    handles: RunHandles,
    mcp: McpHandle,
    _data: TempDir,
    _repository: TempRepo,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.mcp.shutdown();
    }
}

impl Fixture {
    async fn new() -> Self {
        let harness = TestContext::new().await;
        let data = tempfile::Builder::new()
            .prefix("rimaia-consent-at-spawn-")
            .tempdir()
            .expect("a data directory");
        let paths = AppPaths::new(data.path());
        paths.create_all().expect("the app data directories");
        let repository = TempRepo::init();
        let registered = repo::register(
            &harness.context,
            harness.machine(),
            &paths.worktrees_dir(),
            NewRepository {
                path: repository.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register the clone");

        let handles = RunHandles::default();
        let (mcp, served) = mcp::build(
            harness.context.clone(),
            0,
            handles.clone(),
            testing::doctor::provider(),
            Some(testing::doctor::local_tools(harness.machine())),
        )
        .await;
        tokio::spawn(served.run());

        Self {
            harness,
            paths,
            repository_id: registered.id,
            repository_name: registered.name,
            cli: FakeCli::new(),
            handles,
            mcp,
            _data: data,
            _repository: repository,
        }
    }

    fn ctx(&self) -> &ServiceContext {
        &self.harness.context
    }

    fn config(&self) -> RunnerConfig {
        RunnerConfig {
            program: self.cli.program(),
            run_handles: self.handles.clone(),
            ..RunnerConfig::default()
        }
    }

    async fn task(&self, title: &str) -> String {
        tasks::create_task(
            self.ctx(),
            NewTask {
                repository_id: self.repository_id.clone(),
                title: title.to_string(),
                plan: Some(format!("1. {title}")),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a task")
        .id
    }

    async fn consent(&self, allow: bool) {
        repo::set_allow_unattended_runs(
            self.ctx(),
            self.harness.machine(),
            &self.repository_id,
            allow,
        )
        .await
        .expect("this runner's consent");
    }

    /// `repo::ensure_unattended_runs_allowed`'s refusal, which every door
    /// shares.
    fn refusal(&self) -> String {
        format!(
            "\"{}\" has not enabled unattended agent runs. Enable it in Settings → Repositories \
             before starting tasks here.",
            self.repository_name
        )
    }

    async fn run(
        &self,
        board: &dyn BoardPort,
        config: &RunnerConfig,
        claim: Claim,
    ) -> rimaia_core::Error {
        tokio::time::timeout(
            TEST_TIMEOUT,
            run_task(
                board,
                self.harness.machine(),
                &self.paths,
                config,
                claim,
                RunRequest {
                    cancel: CancelSignal::new(),
                    in_flight: None,
                },
            ),
        )
        .await
        .expect("a refused run must not hang")
        .expect_err("the runner refuses to spawn")
    }

    async fn leases(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM runner_leases")
            .fetch_one(&self.ctx().pool)
            .await
            .expect("count the leases")
    }

    async fn detail(&self, task: &str) -> tasks::TaskDetail {
        tasks::get_task(self.ctx(), task).await.expect("the task")
    }

    /// Refused with the consent sentence, nothing spawned, and the lease gone
    /// with the task where `release` lands it.
    async fn assert_refused(&self, task: &str, error: &str) {
        assert_eq!(error, self.refusal());
        assert_eq!(self.cli.started(), Vec::<String>::new(), "nothing spawned");
        assert_eq!(self.leases().await, 0, "the lease is released");
        let detail = self.detail(task).await;
        assert_eq!(detail.task.run_state, RunState::Failed);
        assert_eq!(detail.last_run, None, "no run row was opened");
    }
}
