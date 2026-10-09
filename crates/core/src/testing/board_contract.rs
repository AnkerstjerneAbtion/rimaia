//! The board port's contract suite (ADR-0027 point 5, seam-contract D31 point
//! 13).
//!
//! One set of cases, run against every adapter: the in-process one from
//! `tests/board_port_in_process.rs` (task 036), and task 052's HTTP adapter
//! from `crates/runner/tests/board_port_http.rs`. A behaviour that holds for
//! one adapter and not the other is the bug this suite exists to find.
//!
//! # The rule every case keeps
//!
//! A case **arranges and inspects** the board only through
//! [`Harness::board`]'s core services, and **acts** only through
//! [`Harness::runner`]. No case names an adapter type. That is what lets task
//! 052 invoke [`board_contract!`](crate::board_contract) over HTTP without
//! editing a case; a case that reaches past the harness is one 052 finds out
//! about the hard way.
//!
//! Each later task adds its own cases here (D31 point 13): the review loop's
//! `NextStep::Continue` (021), team scoping (038,
//! 039), fencing and generations (043), expiry (053), `run_tool` (055),
//! resends (056).

use std::future::Future;
use std::sync::Arc;

use chrono::Duration;
use tempfile::TempDir;

use crate::board::{
    BoardMethod, BoardPort, Claim, ClaimTarget, FinishRun, Heartbeat, LeasePurpose, LeaseRef,
    NextStep, StartRun, TranscriptChunk, TranscriptEnd,
};
use crate::clock::Clock;
use crate::context::ServiceContext;
use crate::db::{
    new_id, BoardColumn, ExitClass, RunKind, RunState, RunStatus, StrategyMode, StrategySource,
};
use crate::error::{ErrorCode, Result};
use crate::repo::{self, NewRepository};
use crate::review::findings::{self, FindingSeverity, NewReviewFinding};
use crate::review_loop::{config as review_config, Verdict};
use crate::runner::events::{RunTail, TokenUsage};
use crate::runner::outcome::{self, NewRun, RunOutcome, SpawnedAs};
use crate::runner::provider::ClaudeProvider;
use crate::runner::RunTrigger;
use crate::runs::{self, bundle::RunCapture};
use crate::scheduler::ResumePoint;
use crate::tasks::strategy::StrategyPlan;
use crate::tasks::{self, NewTask, TaskPatch};
use crate::testing::{TempRepo, TestClock};

/// One of the two runners a harness serves over one board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    A,
    B,
}

/// What an adapter's test crate implements to run the suite.
pub trait Harness: Sized {
    /// A fresh board with two runners attached.
    fn start() -> impl Future<Output = Self>;
    /// Runner `A` or `B`'s port onto the one board.
    fn runner(&self, which: Which) -> Arc<dyn BoardPort>;
    /// The board's own context, to arrange and inspect it through core
    /// services.
    fn board(&self) -> &ServiceContext;
    /// The clock the board reads.
    fn clock(&self) -> &TestClock;
}

/// Expands to one `#[tokio::test]` per case, so a failure names its case.
#[macro_export]
macro_rules! board_contract {
    ($harness:ty) => {
        $crate::board_contract!(@cases $harness;
            a_claimed_run_started_and_finished_lands_the_task_as_it_does_today,
            a_claim_lost_to_another_starter_is_none_and_writes_nothing,
            a_retry_claim_carries_the_session_and_kind_it_resumes,
            a_plan_claim_moves_no_run_state_and_opens_no_run,
            preview_returns_what_a_claim_would_and_writes_nothing,
            releasing_an_implementation_lease_fails_a_running_task,
            releasing_a_strategy_lease_leaves_run_state_alone,
            a_finish_that_chooses_its_own_resume_after_is_invalid,
            a_recorded_strategy_is_always_sourced_as_the_planner,
            a_published_tail_reaches_a_tail_subscriber,
            a_transcript_chunk_is_acknowledged_through_its_end,
            start_run_records_the_run_id_the_runner_minted,
            record_branch_sets_the_branch_and_never_the_worktree_path,
            review_findings_recorded_through_the_port_land_on_their_review_run,
            a_finish_for_another_tasks_run_is_not_found,
            a_solo_heartbeat_fences_nothing_and_cancels_nothing,
            every_lease_method_answers_not_found_for_a_task_that_does_not_exist,
            finish_run_continues_to_a_review_when_the_loop_is_on,
            finish_run_releases_an_implementation_when_the_loop_is_off,
            finish_run_releases_a_clean_review_and_lands_the_task,
        );
    };
    (@cases $harness:ty; $($case:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $case() {
                $crate::testing::board_contract::cases::$case::<$harness>().await;
            }
        )*
    };
}

/// A registered repository with tasks in `ready`, arranged through the
/// board's own services. The directories are held for their `Drop`.
struct Arranged {
    _repository: TempRepo,
    _worktrees: TempDir,
    repository_id: String,
}

impl Arranged {
    async fn new(board: &ServiceContext) -> Self {
        let repository = TempRepo::init();
        let worktrees = tempfile::Builder::new()
            .prefix("rimaia-contract-worktrees-")
            .tempdir()
            .expect("a worktrees directory");
        let registered = repo::register(
            board,
            worktrees.path(),
            NewRepository {
                path: repository.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register a repository");

        Self {
            _repository: repository,
            _worktrees: worktrees,
            repository_id: registered.id,
        }
    }

    async fn task(&self, board: &ServiceContext, title: &str) -> String {
        tasks::create_task(
            board,
            NewTask {
                repository_id: self.repository_id.clone(),
                title: title.to_string(),
                plan: Some("1. Do the work".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a task")
        .id
    }
}

fn run_target(task_id: &str, continue_session: bool) -> ClaimTarget {
    ClaimTarget::Run {
        task_id: task_id.to_string(),
        trigger: RunTrigger::Queued,
        continue_session,
    }
}

fn starting(run_id: &str, kind: RunKind) -> StartRun {
    StartRun {
        run_id: run_id.to_string(),
        kind,
        session_id: "session-1".to_string(),
        prompt: "do the work".to_string(),
        base_ref: Some("main".to_string()),
        base_sha: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
    }
}

fn succeeded() -> RunOutcome {
    RunOutcome {
        exit_class: ExitClass::Success,
        status: RunStatus::Succeeded,
        error_message: None,
        num_turns: Some(4),
        cost_usd: Some(0.25),
        duration_ms: Some(1_000),
        pr_url: None,
        usage_limit_resets_at: None,
        resume_after: None,
        spawned_as: SpawnedAs::default(),
        usage: TokenUsage::default(),
    }
}

fn transient() -> RunOutcome {
    RunOutcome {
        exit_class: ExitClass::Transient,
        status: RunStatus::Failed,
        error_message: Some("the API was overloaded".to_string()),
        ..succeeded()
    }
}

fn finishing(outcome: RunOutcome) -> FinishRun {
    FinishRun {
        outcome,
        head_sha: None,
        bundle: None,
        window_closes_at: None,
        transcript: TranscriptEnd::Complete { length: 0 },
    }
}

/// A finish that recorded `head` as the commit the run ended on.
fn finishing_at(outcome: RunOutcome, head: &str) -> FinishRun {
    FinishRun {
        head_sha: Some(head.to_string()),
        ..finishing(outcome)
    }
}

/// Turns the review loop on for the whole board, through its own service.
async fn turn_the_loop_on(board: &ServiceContext) {
    review_config::set_review_settings(
        board,
        &ClaudeProvider,
        "",
        serde_json::json!({ "enabled": "on_cost_acknowledged" }),
    )
    .await
    .expect("turn the loop on");
}

async fn claimed(runner: &dyn BoardPort, target: ClaimTarget) -> Claim {
    runner
        .claim(target)
        .await
        .expect("claim")
        .expect("nobody else holds this task")
}

async fn run_state(board: &ServiceContext, task_id: &str) -> RunState {
    tasks::get_task(board, task_id)
        .await
        .expect("read the task")
        .task
        .run_state
}

async fn run_count(board: &ServiceContext, task_id: &str) -> usize {
    runs::list_runs_for_task(board, task_id)
        .await
        .expect("read the runs")
        .len()
}

/// One call of `method` under `lease`, for the cases that walk
/// [`BoardMethod::ALL`]. The match is exhaustive, so a method added to the
/// port without a line here does not compile.
async fn call(runner: &dyn BoardPort, method: BoardMethod, lease: &LeaseRef) -> Result<()> {
    match method {
        BoardMethod::Preview => runner.preview(&lease.task_id).await.map(drop),
        BoardMethod::Claim => runner
            .claim(run_target(&lease.task_id, false))
            .await
            .map(drop),
        BoardMethod::Heartbeat => runner
            .heartbeat(std::slice::from_ref(lease))
            .await
            .map(drop),
        BoardMethod::RunContext => runner.run_context(lease).await.map(drop),
        BoardMethod::RecordBranch => runner.record_branch(lease, "rimaia/nowhere").await,
        BoardMethod::StartRun => {
            runner
                .start_run(lease, starting(&new_id(), RunKind::Implementation))
                .await
        }
        BoardMethod::AppendTranscript => runner
            .append_transcript(
                lease,
                TranscriptChunk {
                    run_id: "no-such-run".to_string(),
                    offset: 0,
                    bytes: b"{}\n".to_vec(),
                },
            )
            .await
            .map(drop),
        BoardMethod::PublishTail => {
            runner.publish_tail(lease, tail("no-such-run"));
            Ok(())
        }
        BoardMethod::FinishRun => runner
            .finish_run(lease, "no-such-run", finishing(succeeded()))
            .await
            .map(drop),
        BoardMethod::Release => runner.release(lease).await,
        BoardMethod::RecordStrategy => {
            runner
                .record_strategy(lease, StrategyPlan::failed("nothing to plan"))
                .await
        }
        BoardMethod::RecordReviewFindings => {
            runner
                .record_review_findings(lease, "no-such-run", vec![])
                .await
        }
    }
}

/// `assert_eq!` with a label. `pretty_assertions` is a dev-dependency, which
/// a module compiled into the library cannot reach.
fn assert_same<T: PartialEq + std::fmt::Debug>(left: T, right: T, what: &str) {
    assert!(
        left == right,
        "{what}\n  left: {left:#?}\n right: {right:#?}"
    );
}

fn tail(run_id: &str) -> RunTail {
    RunTail {
        run_id: run_id.to_string(),
        elapsed_ms: 1_500,
        turns: 2,
        current_tool: None,
        last_assistant_text: Some("Reading the plan".to_string()),
    }
}

/// The cases themselves. Generic over the harness, and reached only through
/// [`board_contract!`](crate::board_contract).
pub mod cases {
    use super::*;

    pub async fn a_claimed_run_started_and_finished_lands_the_task_as_it_does_today<H: Harness>() {
        let harness = H::start().await;
        let arranged = Arranged::new(harness.board()).await;
        let task_id = arranged.task(harness.board(), "Land me").await;
        let runner = harness.runner(Which::A);

        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        assert_same(claim.purpose, LeasePurpose::Implementation, "purpose");
        assert_same(
            run_state(harness.board(), &task_id).await,
            RunState::Running,
            "a claim takes both edges",
        );

        let run_id = new_id();
        runner
            .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
            .await
            .expect("start the run");
        let receipt = runner
            .finish_run(&claim.lease, &run_id, finishing(succeeded()))
            .await
            .expect("finish the run");

        assert_same(
            receipt.next,
            NextStep::Released { resume_after: None },
            "a success is not retried",
        );
        assert_same(receipt.run.id, run_id.clone(), "the receipt's run");
        assert_same(receipt.run.status, RunStatus::Succeeded, "the run's status");
        let detail = tasks::get_task(harness.board(), &task_id)
            .await
            .expect("read the task");
        assert_same(detail.task.column, BoardColumn::InReview, "the column");
        assert_same(detail.task.run_state, RunState::Idle, "the run state");
    }

    pub async fn a_claim_lost_to_another_starter_is_none_and_writes_nothing<H: Harness>() {
        let harness = H::start().await;
        let arranged = Arranged::new(harness.board()).await;
        let task_id = arranged.task(harness.board(), "Contended").await;

        claimed(
            harness.runner(Which::A).as_ref(),
            run_target(&task_id, false),
        )
        .await;
        let before = tasks::get_task(harness.board(), &task_id)
            .await
            .expect("read the task");
        harness.clock().advance(Duration::seconds(1));

        let lost = harness
            .runner(Which::B)
            .claim(run_target(&task_id, false))
            .await
            .expect("a lost race is not an error");

        assert!(lost.is_none(), "the second starter got a claim: {lost:?}");
        assert_same(
            tasks::get_task(harness.board(), &task_id)
                .await
                .expect("read the task"),
            before,
            "the losing claim wrote nothing",
        );
        assert_same(run_count(harness.board(), &task_id).await, 0, "runs");
    }

    pub async fn a_retry_claim_carries_the_session_and_kind_it_resumes<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged.task(board, "Hit a wall").await;

        // A task waiting out a transient failure, arranged the way the board
        // itself records one.
        for state in [RunState::Queued, RunState::Running] {
            tasks::set_run_state(board, &task_id, state)
                .await
                .expect("walk to running");
        }
        let run = outcome::start_run(
            board,
            &crate::paths::AppPaths::new(std::env::temp_dir().join("rimaia-contract-unused")),
            NewRun {
                task_id: task_id.clone(),
                kind: RunKind::Implementation,
                session_id: "the-first-session".to_string(),
                prompt: "do the work".to_string(),
                base_ref: None,
                base_sha: None,
            },
        )
        .await
        .expect("open the first attempt");
        outcome::finish_run(
            board,
            &run.id,
            &RunOutcome {
                resume_after: Some(harness.clock().now() + Duration::minutes(5)),
                ..transient()
            },
            &RunCapture::default(),
        )
        .await
        .expect("close it as retryable");
        assert_same(
            run_state(board, &task_id).await,
            RunState::WaitingRetry,
            "arranged",
        );

        let claim = claimed(
            harness.runner(Which::A).as_ref(),
            run_target(&task_id, true),
        )
        .await;

        assert_same(
            claim.resume,
            Some(ResumePoint {
                kind: RunKind::Implementation,
                session_id: "the-first-session".to_string(),
            }),
            "the point a retry resumes",
        );
        assert_same(
            run_state(board, &task_id).await,
            RunState::Running,
            "a retry claim takes waiting_retry -> running",
        );
    }

    pub async fn a_plan_claim_moves_no_run_state_and_opens_no_run<H: Harness>() {
        let harness = H::start().await;
        let arranged = Arranged::new(harness.board()).await;
        let task_id = arranged.task(harness.board(), "Plan me").await;

        let claim = claimed(
            harness.runner(Which::A).as_ref(),
            ClaimTarget::Plan {
                task_id: task_id.clone(),
            },
        )
        .await;

        assert_same(claim.purpose, LeasePurpose::Strategy, "purpose");
        assert_same(claim.resume, None, "a plan resumes nothing");
        assert_same(
            run_state(harness.board(), &task_id).await,
            RunState::Idle,
            "the run state",
        );
        assert_same(run_count(harness.board(), &task_id).await, 0, "runs");
    }

    pub async fn preview_returns_what_a_claim_would_and_writes_nothing<H: Harness>() {
        let harness = H::start().await;
        let arranged = Arranged::new(harness.board()).await;
        let task_id = arranged.task(harness.board(), "Look first").await;
        let runner = harness.runner(Which::A);
        let before = tasks::get_task(harness.board(), &task_id)
            .await
            .expect("read the task");

        let preview = runner.preview(&task_id).await.expect("preview");

        assert_same(
            tasks::get_task(harness.board(), &task_id)
                .await
                .expect("read the task"),
            before,
            "a preview wrote nothing",
        );
        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        assert_same(claim.context, preview, "the claim's context");
    }

    pub async fn releasing_an_implementation_lease_fails_a_running_task<H: Harness>() {
        let harness = H::start().await;
        let arranged = Arranged::new(harness.board()).await;
        let task_id = arranged.task(harness.board(), "Abandoned").await;
        let runner = harness.runner(Which::A);

        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        runner.release(&claim.lease).await.expect("release");

        assert_same(
            run_state(harness.board(), &task_id).await,
            RunState::Failed,
            "the run state",
        );
    }

    pub async fn releasing_a_strategy_lease_leaves_run_state_alone<H: Harness>() {
        let harness = H::start().await;
        let arranged = Arranged::new(harness.board()).await;
        let task_id = arranged.task(harness.board(), "Planned and released").await;
        let runner = harness.runner(Which::A);

        let claim = claimed(
            runner.as_ref(),
            ClaimTarget::Plan {
                task_id: task_id.clone(),
            },
        )
        .await;
        runner.release(&claim.lease).await.expect("release");

        assert_same(
            run_state(harness.board(), &task_id).await,
            RunState::Idle,
            "the run state",
        );
    }

    pub async fn a_finish_that_chooses_its_own_resume_after_is_invalid<H: Harness>() {
        let harness = H::start().await;
        let arranged = Arranged::new(harness.board()).await;
        let task_id = arranged.task(harness.board(), "Overreaching").await;
        let runner = harness.runner(Which::A);
        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        let run_id = new_id();
        runner
            .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
            .await
            .expect("start the run");

        let error = runner
            .finish_run(
                &claim.lease,
                &run_id,
                finishing(RunOutcome {
                    resume_after: Some(harness.clock().now() + Duration::minutes(1)),
                    ..transient()
                }),
            )
            .await
            .expect_err("the runner chose its own retry time");

        assert_same(error.code(), ErrorCode::Invalid, "the error code");
        let run = runs::get_run_row(harness.board(), &run_id)
            .await
            .expect("read the run");
        assert_same(run.ended_at, None, "the run was left open");
        assert_same(
            run_state(harness.board(), &task_id).await,
            RunState::Running,
            "the task was not landed",
        );
    }

    pub async fn a_recorded_strategy_is_always_sourced_as_the_planner<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged.task(board, "Planned").await;
        tasks::update_task(
            board,
            &task_id,
            TaskPatch {
                strategy_mode: Some(StrategyMode::Planned),
                ..TaskPatch::default()
            },
        )
        .await
        .expect("put the task in planned mode");
        let runner = harness.runner(Which::A);
        let claim = claimed(
            runner.as_ref(),
            ClaimTarget::Plan {
                task_id: task_id.clone(),
            },
        )
        .await;

        runner
            .record_strategy(
                &claim.lease,
                StrategyPlan::proposed(Some("claude-sonnet-5".to_string()), None),
            )
            .await
            .expect("record the strategy");

        let task = tasks::get_task(board, &task_id)
            .await
            .expect("read the task")
            .task;
        assert_same(
            task.strategy_source,
            Some(StrategySource::Planner),
            "the source",
        );
        assert_same(task.model, Some("claude-sonnet-5".to_string()), "the model");
    }

    pub async fn a_published_tail_reaches_a_tail_subscriber<H: Harness>() {
        let harness = H::start().await;
        let arranged = Arranged::new(harness.board()).await;
        let task_id = arranged.task(harness.board(), "Watched").await;
        let runner = harness.runner(Which::A);
        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        let mut tails = harness.board().subscribe_tail();

        runner.publish_tail(&claim.lease, tail("run-1"));

        assert_same(
            tails.recv().await.expect("a tail on the board's channel"),
            tail("run-1"),
            "the tail",
        );
    }

    pub async fn a_transcript_chunk_is_acknowledged_through_its_end<H: Harness>() {
        let harness = H::start().await;
        let arranged = Arranged::new(harness.board()).await;
        let task_id = arranged.task(harness.board(), "Transcribed").await;
        let runner = harness.runner(Which::A);
        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        let run_id = new_id();
        runner
            .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
            .await
            .expect("start the run");

        let ack = runner
            .append_transcript(
                &claim.lease,
                TranscriptChunk {
                    run_id,
                    offset: 0,
                    bytes: b"{\"type\":\"system\"}\n".to_vec(),
                },
            )
            .await
            .expect("append");

        assert_same(ack.stored_through, 18, "acknowledged through");
    }

    pub async fn start_run_records_the_run_id_the_runner_minted<H: Harness>() {
        let harness = H::start().await;
        let arranged = Arranged::new(harness.board()).await;
        let task_id = arranged.task(harness.board(), "Minted").await;
        let runner = harness.runner(Which::A);
        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        let run_id = new_id();

        runner
            .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
            .await
            .expect("start the run");

        let run = runs::get_run_row(harness.board(), &run_id)
            .await
            .expect("a row under the runner's id");
        assert_same(run.task_id, task_id, "the task");
        assert_same(run.kind, RunKind::Implementation, "the kind");
        assert_same(run.attempt, 1, "the attempt");
        assert_same(run.status, RunStatus::Running, "the status");
        assert_same(run.session_id, "session-1".to_string(), "the session");
        assert_same(run.prompt, "do the work".to_string(), "the prompt");
        assert_same(run.base_ref, Some("main".to_string()), "the base ref");
        assert_same(
            run.base_sha,
            Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            "the base sha",
        );
    }

    pub async fn record_branch_sets_the_branch_and_never_the_worktree_path<H: Harness>() {
        let harness = H::start().await;
        let arranged = Arranged::new(harness.board()).await;
        let task_id = arranged.task(harness.board(), "Branched").await;
        let runner = harness.runner(Which::A);
        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;

        runner
            .record_branch(&claim.lease, "rimaia/branched")
            .await
            .expect("record the branch");

        let task = tasks::get_task(harness.board(), &task_id)
            .await
            .expect("read the task")
            .task;
        assert_same(task.branch, Some("rimaia/branched".to_string()), "branch");
        assert_same(task.worktree_path, None, "worktree path");
    }

    pub async fn review_findings_recorded_through_the_port_land_on_their_review_run<H: Harness>() {
        let harness = H::start().await;
        let arranged = Arranged::new(harness.board()).await;
        let task_id = arranged.task(harness.board(), "Reviewed").await;
        let runner = harness.runner(Which::A);
        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        let review_id = new_id();
        runner
            .start_run(&claim.lease, starting(&review_id, RunKind::Review))
            .await
            .expect("open the review run");

        runner
            .record_review_findings(
                &claim.lease,
                &review_id,
                vec![NewReviewFinding {
                    severity: FindingSeverity::High,
                    title: "The retry never stops".to_string(),
                    body: "The loop has no budget.".to_string(),
                    file: Some("src/retry.rs".to_string()),
                    line: Some(12),
                }],
            )
            .await
            .expect("record the findings");

        let recorded = findings::list(harness.board(), &task_id, None)
            .await
            .expect("read the findings");
        assert_same(recorded.len(), 1, "one finding");
        assert_same(
            recorded[0].review_run_id.clone(),
            review_id.clone(),
            "on its review run",
        );
        assert_same(
            recorded[0].title.clone(),
            "The retry never stops".to_string(),
            "the title",
        );
        assert!(findings::recorded_at(harness.board(), &review_id)
            .await
            .expect("read the witness")
            .is_some());
    }

    pub async fn a_finish_for_another_tasks_run_is_not_found<H: Harness>() {
        let harness = H::start().await;
        let arranged = Arranged::new(harness.board()).await;
        let mine = arranged.task(harness.board(), "Mine").await;
        let theirs = arranged.task(harness.board(), "Theirs").await;
        let a = harness.runner(Which::A);
        let b = harness.runner(Which::B);
        let my_claim = claimed(a.as_ref(), run_target(&mine, false)).await;
        let their_claim = claimed(b.as_ref(), run_target(&theirs, false)).await;
        let their_run = new_id();
        b.start_run(
            &their_claim.lease,
            starting(&their_run, RunKind::Implementation),
        )
        .await
        .expect("start their run");

        let error = a
            .finish_run(&my_claim.lease, &their_run, finishing(succeeded()))
            .await
            .expect_err("a lease bounds what a call may touch");

        assert_same(error.code(), ErrorCode::NotFound, "the error code");
        let run = runs::get_run_row(harness.board(), &their_run)
            .await
            .expect("read their run");
        assert_same(run.ended_at, None, "their run is still open");
    }

    pub async fn a_solo_heartbeat_fences_nothing_and_cancels_nothing<H: Harness>() {
        let harness = H::start().await;
        let arranged = Arranged::new(harness.board()).await;
        let task_id = arranged.task(harness.board(), "Alive").await;
        let runner = harness.runner(Which::A);
        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;

        let heartbeat = runner
            .heartbeat(std::slice::from_ref(&claim.lease))
            .await
            .expect("heartbeat");

        assert_same(heartbeat, Heartbeat::default(), "the heartbeat");
    }

    pub async fn every_lease_method_answers_not_found_for_a_task_that_does_not_exist<H: Harness>() {
        let harness = H::start().await;
        let runner = harness.runner(Which::A);
        let nowhere = LeaseRef::solo("no-such-task");

        for method in BoardMethod::ALL {
            // The three lease-less methods are scoped by the runner, not by a
            // lease, and `publish_tail` cannot fail.
            if !method.takes_a_lease() || method == BoardMethod::PublishTail {
                continue;
            }

            let error = call(runner.as_ref(), method, &nowhere)
                .await
                .expect_err(method.as_str());
            assert_same(error.code(), ErrorCode::NotFound, method.as_str());
        }
    }

    pub async fn finish_run_continues_to_a_review_when_the_loop_is_on<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged.task(board, "Reviewed overnight").await;
        turn_the_loop_on(board).await;
        let runner = harness.runner(Which::A);
        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        let run_id = new_id();
        runner
            .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
            .await
            .expect("start the implementation");

        let receipt = runner
            .finish_run(&claim.lease, &run_id, finishing_at(succeeded(), "a1"))
            .await
            .expect("finish it");

        assert_same(
            receipt.next,
            NextStep::Continue {
                kind: RunKind::Review,
            },
            "the board starts a review",
        );
        let detail = tasks::get_task(board, &task_id)
            .await
            .expect("read the task");
        assert_same(detail.task.column, BoardColumn::Ready, "the column");
        assert_same(detail.task.run_state, RunState::Running, "still running");
    }

    pub async fn finish_run_releases_an_implementation_when_the_loop_is_off<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged.task(board, "Not reviewed").await;
        let runner = harness.runner(Which::A);
        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        let run_id = new_id();
        runner
            .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
            .await
            .expect("start the implementation");

        let receipt = runner
            .finish_run(&claim.lease, &run_id, finishing_at(succeeded(), "a1"))
            .await
            .expect("finish it");

        assert_same(
            receipt.next,
            NextStep::Released { resume_after: None },
            "nothing follows",
        );
        let detail = tasks::get_task(board, &task_id)
            .await
            .expect("read the task");
        assert_same(detail.task.column, BoardColumn::InReview, "the column");
        assert_same(detail.task.run_state, RunState::Idle, "the run state");
        assert_same(detail.review_loop, None, "the loop never touched it");
    }

    pub async fn finish_run_releases_a_clean_review_and_lands_the_task<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged.task(board, "Reviewed clean").await;
        turn_the_loop_on(board).await;
        let runner = harness.runner(Which::A);
        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        let implementation = new_id();
        runner
            .start_run(
                &claim.lease,
                starting(&implementation, RunKind::Implementation),
            )
            .await
            .expect("start the implementation");
        runner
            .finish_run(
                &claim.lease,
                &implementation,
                finishing_at(succeeded(), "a1"),
            )
            .await
            .expect("finish it");
        let review = new_id();
        runner
            .start_run(&claim.lease, starting(&review, RunKind::Review))
            .await
            .expect("start the review");
        runner
            .record_review_findings(&claim.lease, &review, vec![])
            .await
            .expect("a clean review is an explicit empty call");

        let receipt = runner
            .finish_run(&claim.lease, &review, finishing_at(succeeded(), "a1"))
            .await
            .expect("finish the review");

        assert_same(
            receipt.next,
            NextStep::Released { resume_after: None },
            "the loop ends",
        );
        let detail = tasks::get_task(board, &task_id)
            .await
            .expect("read the task");
        assert_same(detail.task.column, BoardColumn::InReview, "the column");
        assert_same(detail.task.run_state, RunState::Idle, "the run state");
        assert_same(
            detail.review_loop.map(|summary| summary.verdict),
            Some(Verdict::Clean),
            "the verdict",
        );
    }
}
