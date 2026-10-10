//! The runner judges its strategy ceiling again before every spawn (ADR-0032
//! point 3's last paragraph, task 072).
//!
//! The claim carries the ceiling read when it was made, and the board refuses
//! with it (task 045, `tests/strategy_ceiling.rs`). Neither is the decision:
//! the board is not trusted to have honoured it, and the owner can lower it
//! between a claim and a spawn. So at the last point before the agent process
//! starts, beside the consent re-check, the runner reads its ceiling again
//! and spawns with `judge`'s answer. Each refusal case here has the board
//! claim the task with no ceiling, lowers the stored one afterwards, and
//! asserts the fixture CLI is never started for the refused phase and that
//! the lease is gone. The fill case asserts what the agent was spawned with
//! and what the run's transcript records about it.
//!
//! Real git in a `TempDir`, the recorded `success` stream, and a real bound
//! MCP server, because the planner and the review are composed against one.

// The fake CLI is a shell script, which Windows will not execute.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use pretty_assertions::assert_eq;
use rimaia_core::board::{
    BoardFuture, BoardPort, Claim, ClaimTarget, FinishReceipt, FinishRun, FreeCapacity, Heartbeat,
    LeaseRef, NextStep, OwnerPresence, PreviewOf, RunContext, StartRun, TranscriptAck,
    TranscriptChunk,
};
use rimaia_core::consent::ceiling::{StrategyCeiling, STRATEGY_CEILING};
use rimaia_core::db::{BoardColumn, ExitClass, RunKind, RunState, StrategyMode};
use rimaia_core::machine::MachineContext;
use rimaia_core::mcp::{self, McpHandle, RunHandles};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::review::findings::NewReviewFinding;
use rimaia_core::review_loop::{config as review_config, UnreviewedReason, Verdict};
use rimaia_core::runner::events::{stderr_path, RunTail};
use rimaia_core::runner::provider::ClaudeProvider;
use rimaia_core::runner::strategy::{claim_for_planning, plan_claimed, PlanOutcome};
use rimaia_core::runner::{run_task, CancelSignal, RunRequest, RunTrigger, RunnerConfig};
use rimaia_core::scheduler::{InFlight, SlotOwner};
use rimaia_core::tasks::strategy::StrategyPlan;
use rimaia_core::tasks::{self, NewTask, Patch, TaskPatch};
use rimaia_core::testing::{self, FakeCli, TempRepo, TestContext};
use rimaia_core::{AppPaths, ServiceContext};
use serde_json::json;
use tempfile::TempDir;

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn a_ceiling_lowered_after_the_claim_refuses_the_spawn() {
    the_queue_refuses().await;
    run_now_refuses().await;
    plan_now_refuses().await;
    a_phase_started_by_continue_refuses().await;
}

/// A `Next` claim granted with no ceiling, for a task that names opus.
async fn the_queue_refuses() {
    let f = Fixture::new().await;
    let task = f.task("From the queue").await;
    f.choose_model(&task, "opus").await;
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
            ceiling: StrategyCeiling::default(),
        })
        .await
        .expect("claim")
        .expect("the board claims the task under no ceiling");
    assert_eq!(claim.lease.task_id, task);
    f.lower_the_ceiling().await;

    let error = f.run(board.as_ref(), &config, claim).await;
    f.assert_refused(&task, &error.to_string(), "opus").await;
}

/// Run now, claimed straight through the port with no ceiling, which the
/// starter would have sent had the ceiling not been lowered after it.
async fn run_now_refuses() {
    let f = Fixture::new().await;
    let task = f.task("Run now").await;
    f.choose_model(&task, "opus").await;
    let config = f.config();
    let board = f.harness.board(&f.paths, &config);

    let claim = board
        .claim(ClaimTarget::Run {
            task_id: task.clone(),
            trigger: RunTrigger::Manual,
            continue_session: false,
            ceiling: StrategyCeiling::default(),
        })
        .await
        .expect("claim")
        .expect("the board claims it");
    f.lower_the_ceiling().await;

    let error = f.run(board.as_ref(), &config, claim).await;
    f.assert_refused(&task, &error.to_string(), "opus").await;
}

/// Plan now, through its real door, which read no ceiling when it claimed:
/// the planner's own budget (Claude's catalogue gives it haiku) is what the
/// lowered ceiling refuses.
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
    f.lower_the_ceiling().await;

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
    assert_eq!(reason, refusal("haiku"));
    assert_eq!(f.cli.started(), Vec::<String>::new(), "nothing spawned");
    assert_eq!(f.leases().await, 0, "the planner's claim went back");
    assert_eq!(
        f.detail(&task).await.task.run_state,
        RunState::Idle,
        "a planner moves no run state"
    );
}

/// The implementation runs under no ceiling; the board's `Continue` is
/// granted on the no-ceiling the finish carried, and the owner lowers it
/// before the review spawns, so the review that names opus never starts.
async fn a_phase_started_by_continue_refuses() {
    let f = Fixture::new().await;
    let task = f.task("Continue").await;
    review_config::set_review_settings(
        f.ctx(),
        &ClaudeProvider,
        "Run /review.",
        json!({ "enabled": "on_cost_acknowledged", "review_model": "opus" }),
    )
    .await
    .expect("turn the loop on with a review model");

    let config = f.config();
    let board = LowerAfterContinue {
        inner: f.harness.board(&f.paths, &config),
        machine: f.harness.machine().clone(),
    };
    let claim = board
        .claim(ClaimTarget::Run {
            task_id: task.clone(),
            trigger: RunTrigger::Queued,
            continue_session: false,
            ceiling: StrategyCeiling::default(),
        })
        .await
        .expect("claim")
        .expect("unclaimed");

    tokio::time::timeout(
        TEST_TIMEOUT,
        run_task(
            &board,
            f.harness.machine(),
            &f.paths,
            &config,
            claim,
            RunRequest {
                cancel: CancelSignal::new(),
                in_flight: None,
            },
        ),
    )
    .await
    .expect("the run must finish inside the test timeout")
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
        Some(refusal("opus").as_str()),
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

#[tokio::test]
async fn an_absent_model_is_spawned_with_the_ceilings_first() {
    let f = Fixture::new().await;
    let task = f.task("Names nothing").await;
    f.store_ceiling(&StrategyCeiling {
        models: Some(vec!["sonnet".to_string(), "haiku".to_string()]),
        max_effort: Some("medium".to_string()),
    })
    .await;
    let config = f.config();
    let board = f.harness.board(&f.paths, &config);
    let claim = board
        .claim(ClaimTarget::Run {
            task_id: task.clone(),
            trigger: RunTrigger::Queued,
            continue_session: false,
            ceiling: rimaia_core::consent::ceiling::strategy_ceiling(f.harness.machine())
                .await
                .expect("read the ceiling"),
        })
        .await
        .expect("a ceiling refuses nothing nobody named")
        .expect("unclaimed");

    let run = tokio::time::timeout(
        TEST_TIMEOUT,
        run_task(
            board.as_ref(),
            f.harness.machine(),
            &f.paths,
            &config,
            claim,
            RunRequest::default(),
        ),
    )
    .await
    .expect("the run must finish inside the test timeout")
    .expect("the run is recorded");

    let argv = f.cli.argv(&task, 1);
    assert_eq!(value_after(&argv, "--model"), "sonnet", "{argv:?}");
    assert_eq!(value_after(&argv, "--effort"), "medium", "{argv:?}");
    assert_eq!(run.effort.as_deref(), Some("medium"), "the row's effort");
    let stderr = std::fs::read_to_string(stderr_path(&f.paths, &task, &run.id))
        .expect("the run's stderr capture");
    let notes: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("strategy ceiling"))
        .collect();
    assert_eq!(
        notes,
        vec![
            "rimaia: no model was named, so this run spawned with \"sonnet\", the first model \
             in this runner's strategy ceiling (origin: runner_ceiling)",
            "rimaia: no effort was named, so this run spawned with \"medium\", the highest \
             effort this runner's strategy ceiling allows (origin: runner_ceiling)",
        ],
        "the run records that its strategy came from the ceiling"
    );
    // The board's own resolution never carries the runner's fill.
    let detail = f.detail(&task).await;
    assert_eq!(detail.effective_model, None);
    assert_eq!(detail.effective_effort, None);
}

/// 045's strategy-ceiling sentence, for a runner the board sent no label for:
/// the solo runner, in a personal team.
fn refusal(model: &str) -> String {
    format!(
        "this task asks for the model \"{model}\", which this runner's strategy ceiling does \
         not allow. Change the task's model, or run it on another runner."
    )
}

fn value_after(argv: &[String], flag: &str) -> String {
    argv.iter()
        .position(|arg| arg == flag)
        .and_then(|at| argv.get(at + 1))
        .unwrap_or_else(|| panic!("{flag} is not in {argv:?}"))
        .clone()
}

/// A solo board with one registered repository this runner consents to, and
/// a bound MCP server. Only the ceiling can refuse.
struct Fixture {
    harness: TestContext,
    paths: AppPaths,
    repository_id: String,
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
            .prefix("rimaia-ceiling-at-spawn-")
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
        repo::set_allow_unattended_runs(&harness.context, harness.machine(), &registered.id, true)
            .await
            .expect("this runner's consent");

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

    async fn choose_model(&self, task: &str, model: &str) {
        tasks::update_task(
            self.ctx(),
            task,
            TaskPatch {
                model: Patch::Set(model.to_string()),
                ..TaskPatch::default()
            },
        )
        .await
        .expect("choose the task's model");
    }

    async fn store_ceiling(&self, ceiling: &StrategyCeiling) {
        store_ceiling(self.harness.machine(), ceiling).await;
    }

    /// Sonnet alone: below opus, and not the planner's haiku.
    async fn lower_the_ceiling(&self) {
        self.store_ceiling(&sonnet_only()).await;
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

    /// Refused with the ceiling sentence, nothing spawned, and the lease gone
    /// with the task where `release` lands it.
    async fn assert_refused(&self, task: &str, error: &str, model: &str) {
        assert_eq!(error, refusal(model));
        assert_eq!(self.cli.started(), Vec::<String>::new(), "nothing spawned");
        assert_eq!(self.leases().await, 0, "the lease is released");
        let detail = self.detail(task).await;
        assert_eq!(detail.task.run_state, RunState::Failed);
        assert_eq!(detail.last_run, None, "no run row was opened");
    }
}

fn sonnet_only() -> StrategyCeiling {
    StrategyCeiling {
        models: Some(vec!["sonnet".to_string()]),
        max_effort: None,
    }
}

async fn store_ceiling(machine: &MachineContext, ceiling: &StrategyCeiling) {
    machine
        .store
        .set_setting(
            STRATEGY_CEILING,
            &serde_json::to_string(ceiling).expect("a ceiling serializes"),
        )
        .await
        .expect("store the ceiling");
}

/// A board port that lowers this runner's ceiling the moment a finish
/// answers `Continue`: after the board judged the next phase on the ceiling
/// the finish carried, and before that phase spawns. Every other call goes
/// through.
struct LowerAfterContinue {
    inner: Arc<dyn BoardPort>,
    machine: MachineContext,
}

impl BoardPort for LowerAfterContinue {
    fn preview<'a>(&'a self, task_id: &'a str, of: PreviewOf) -> BoardFuture<'a, RunContext> {
        self.inner.preview(task_id, of)
    }

    fn claim<'a>(&'a self, target: ClaimTarget) -> BoardFuture<'a, Option<Claim>> {
        self.inner.claim(target)
    }

    fn heartbeat<'a>(&'a self, held: &'a [LeaseRef]) -> BoardFuture<'a, Heartbeat> {
        self.inner.heartbeat(held)
    }

    fn run_context<'a>(&'a self, lease: &'a LeaseRef) -> BoardFuture<'a, RunContext> {
        self.inner.run_context(lease)
    }

    fn record_branch<'a>(&'a self, lease: &'a LeaseRef, branch: &'a str) -> BoardFuture<'a, ()> {
        self.inner.record_branch(lease, branch)
    }

    fn start_run<'a>(&'a self, lease: &'a LeaseRef, run: StartRun) -> BoardFuture<'a, ()> {
        self.inner.start_run(lease, run)
    }

    fn append_transcript<'a>(
        &'a self,
        lease: &'a LeaseRef,
        chunk: TranscriptChunk,
    ) -> BoardFuture<'a, TranscriptAck> {
        self.inner.append_transcript(lease, chunk)
    }

    fn publish_tail(&self, lease: &LeaseRef, tail: RunTail) {
        self.inner.publish_tail(lease, tail);
    }

    fn finish_run<'a>(
        &'a self,
        lease: &'a LeaseRef,
        run_id: &'a str,
        finish: FinishRun,
    ) -> BoardFuture<'a, FinishReceipt> {
        Box::pin(async move {
            let receipt = self.inner.finish_run(lease, run_id, finish).await?;
            if matches!(receipt.next, NextStep::Continue { .. }) {
                store_ceiling(&self.machine, &sonnet_only()).await;
            }
            Ok(receipt)
        })
    }

    fn release<'a>(&'a self, lease: &'a LeaseRef) -> BoardFuture<'a, ()> {
        self.inner.release(lease)
    }

    fn record_strategy<'a>(
        &'a self,
        lease: &'a LeaseRef,
        plan: StrategyPlan,
    ) -> BoardFuture<'a, ()> {
        self.inner.record_strategy(lease, plan)
    }

    fn record_review_findings<'a>(
        &'a self,
        lease: &'a LeaseRef,
        run_id: &'a str,
        findings: Vec<NewReviewFinding>,
    ) -> BoardFuture<'a, ()> {
        self.inner.record_review_findings(lease, run_id, findings)
    }
}
