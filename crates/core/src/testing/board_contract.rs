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
//! 039: `a_lease_on_another_teams_task_is_not_found_in_either_spelling`),
//! `ClaimTarget::Next` (042: the `a_next_claim_…` cases and its race),
//! fencing and generations (043: the `…generation…`, heartbeat and week-long
//! cases, and the race, which is the lease form of 042's),
//! the model rule and who may start a runner (067: the `…model…` cases,
//! `run_now_is_not_bound_by_capacity` and the owner case),
//! expiry (053), `run_tool` (055), resends (056).

use std::future::Future;
use std::sync::Arc;

use std::time::Duration as StdDuration;

use chrono::Duration;
use tempfile::TempDir;

use crate::board::lease;
use crate::board::service::{authorize_start, OwnerPresence};
use crate::board::{
    BoardMethod, BoardPort, Claim, ClaimTarget, FinishRun, FreeCapacity, Heartbeat, LeasePurpose,
    LeaseRef, LeaseTerm, NextStep, StartRun, TranscriptChunk, TranscriptEnd, LEASE_LIFETIME,
};
use crate::clock::Clock;
use crate::context::ServiceContext;
use crate::db::{
    new_id, BoardColumn, ExitClass, RunKind, RunState, RunStatus, StrategyMode, StrategySource,
};
use crate::error::{ErrorCode, Result};
use crate::machine::MachineContext;
use crate::repo::{self, NewRepository};
use crate::review::findings::{self, FindingSeverity, NewReviewFinding};
use crate::review_loop::{config as review_config, Verdict};
use crate::runner::events::{RunTail, TokenUsage};
use crate::runner::outcome::{self, NewRun, RunOutcome, SpawnedAs};
use crate::runner::provider::{ClaudeProvider, ProviderId};
use crate::runner::RunTrigger;
use crate::runs::{self, bundle::RunCapture};
use crate::scheduler::selection::{self, RunnerView};
use crate::scheduler::ResumePoint;
use crate::tasks::strategy::StrategyPlan;
use crate::tasks::Patch;
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
    /// A fresh board with two runners attached, whose leases never expire:
    /// solo's term.
    fn start() -> impl Future<Output = Self> {
        Self::start_with(LeaseTerm::Never)
    }
    /// The same, with the lease term this board grants (task 043). The board
    /// must be served over more than one database connection, so a race
    /// between the two runners is a real one.
    fn start_with(term: LeaseTerm) -> impl Future<Output = Self>;
    /// Runner `A` or `B`'s port onto the one board.
    fn runner(&self, which: Which) -> Arc<dyn BoardPort>;
    /// Runner `A` or `B`'s port onto the one board, as a runner whose claims
    /// carry `provider` (task 067's model rule). The in-process harness builds
    /// the adapter with that provider; task 052's builds its `HttpBoard` with
    /// that `ProviderId`.
    fn runner_on(&self, which: Which, provider: ProviderId) -> Arc<dyn BoardPort>;
    /// Adds a second user as a member of the board's team, with a runner of
    /// their own, and returns both ids: `(user_id, runner_id)`.
    fn add_member_with_runner(&self) -> impl Future<Output = (String, String)>;
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
            record_branch_sets_the_branch,
            review_findings_recorded_through_the_port_land_on_their_review_run,
            a_finish_for_another_tasks_run_is_not_found,
            a_solo_heartbeat_fences_nothing_and_cancels_nothing,
            every_lease_method_answers_not_found_for_a_task_that_does_not_exist,
            a_lease_on_another_teams_task_is_not_found_in_either_spelling,
            finish_run_continues_to_a_review_when_the_loop_is_on,
            finish_run_releases_an_implementation_when_the_loop_is_off,
            finish_run_releases_a_clean_review_and_lands_the_task,
            a_next_claim_takes_the_top_startable_task_in_board_order,
            a_next_claim_passes_over_a_repository_the_runner_did_not_list,
            a_next_claim_takes_a_listed_repository_whatever_its_ceiling_column_says,
            a_next_claim_honours_each_repositorys_free_slots,
            a_next_claim_with_no_free_capacity_claims_nothing_and_writes_nothing,
            a_next_claim_resumes_a_due_retry_with_the_session_it_continues,
            two_runners_claiming_next_for_one_task_get_exactly_one_claim,
            a_waiting_next_claim_returns_as_soon_as_a_task_becomes_startable,
            a_waiting_next_claim_returns_none_once_its_wait_has_passed,
            two_runners_racing_for_one_task_get_exactly_one_claim,
            every_lease_method_refuses_a_stale_generation,
            a_lease_naming_a_missing_task_is_not_found_and_a_released_lease_is_conflict,
            generation_increases_across_a_release_and_a_reclaim,
            the_heartbeat_renews_current_leases_and_fences_stale_ones_per_lease,
            a_solo_lease_survives_a_week_of_clock_time,
            a_runner_is_not_offered_a_task_whose_model_belongs_to_another_provider,
            a_continue_into_a_review_whose_model_this_provider_cannot_run_is_released,
            a_plan_claim_and_an_inline_planner_claim_are_not_subject_to_the_model_rule,
            run_now_is_not_bound_by_capacity,
            only_a_runners_owner_is_authorized_to_start_it,
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
        // A machine of its own, used only to register through the service: the
        // board port never reads a clone, so nothing in a case touches it.
        let machine = MachineContext {
            store: std::sync::Arc::new(crate::testing::machine::MemoryMachine::new()),
            clock: board.clock.clone(),
            changes: board.changes.clone(),
            event_team: board.scope.sole().expect("a solo board").clone(),
        };
        let registered = repo::register(
            board,
            &machine,
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
        self.task_in(board, title, BoardColumn::Ready).await
    }

    /// A task appended to `column`, so creation order is board order.
    async fn task_in(&self, board: &ServiceContext, title: &str, column: BoardColumn) -> String {
        tasks::create_task(
            board,
            NewTask {
                repository_id: self.repository_id.clone(),
                title: title.to_string(),
                plan: Some("1. Do the work".to_string()),
                extra_instructions: None,
                column: Some(column),
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
        ceiling: Default::default(),
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
        ceiling: Default::default(),
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

/// Sets `task_id`'s model through the board's own service, which makes the
/// task `manual` (D17.6) unless `mode` says otherwise.
async fn ask_for_model(
    board: &ServiceContext,
    task_id: &str,
    model: &str,
    mode: Option<StrategyMode>,
) {
    tasks::update_task(
        board,
        task_id,
        TaskPatch {
            strategy_mode: mode,
            model: Patch::Set(model.to_string()),
            ..TaskPatch::default()
        },
    )
    .await
    .expect("set the task's model");
}

/// The sentence a named claim of a task set to `model` gets from a runner
/// whose provider is `provider` and does not offer it.
fn model_refusal(model: &str, provider: ProviderId) -> String {
    format!(
        "this task asks for the model \"{model}\", which {} cannot run. Change the task's \
         model, or run it on a runner whose provider offers it.",
        provider.display_name()
    )
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

/// A `Next` claim over `repositories`, with `total` free slots overall and
/// `per_repository` free in each, that does not wait.
fn next_target(
    repositories: &[&str],
    total: usize,
    per_repository: &[(&str, usize)],
) -> ClaimTarget {
    next_target_waiting(repositories, total, per_repository, StdDuration::ZERO)
}

fn next_target_waiting(
    repositories: &[&str],
    total: usize,
    per_repository: &[(&str, usize)],
    wait: StdDuration,
) -> ClaimTarget {
    ClaimTarget::Next {
        capacity: FreeCapacity {
            total,
            per_repository: per_repository
                .iter()
                .map(|(id, slots)| ((*id).to_string(), *slots))
                .collect(),
        },
        repositories: repositories.iter().map(|id| (*id).to_string()).collect(),
        wait,
        ceiling: Default::default(),
    }
}

/// Walks `task_id` through ADR-0007's machine with the one writer of
/// `run_state`, never a hand-written `UPDATE`.
async fn walk(board: &ServiceContext, task_id: &str, route: &[RunState]) {
    for state in route {
        tasks::set_run_state(board, task_id, *state)
            .await
            .unwrap_or_else(|error| panic!("walk {task_id} to {state:?}: {error}"));
    }
}

/// Yields long enough for a spawned claim to have reached its wait. Not a
/// sleep: it costs no wall-clock time and guesses no duration.
async fn converge() {
    for _ in 0..2_000 {
        tokio::task::yield_now().await;
    }
}

/// A failure bound on a claim a case expects to return, so a claim that never
/// wakes fails with a sentence rather than hanging the job.
async fn joined(
    handle: tokio::task::JoinHandle<Result<Option<Claim>>>,
    what: &str,
) -> Option<Claim> {
    tokio::time::timeout(StdDuration::from_secs(30), handle)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .expect("the claim task does not panic")
        .expect("a waiting claim is not an error")
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

/// The live lease the board records for `task_id`, if any, read through the
/// board's own service.
async fn lease_of(board: &ServiceContext, task_id: &str) -> Option<lease::Lease> {
    lease::state_of(board, task_id)
        .await
        .expect("read the task's lease")
        .lease
}

/// Every row of the three tables a lease method could write, as text, so a
/// case can assert that a refused call wrote nothing at all.
async fn rows_a_lease_method_could_write(board: &ServiceContext) -> Vec<String> {
    let mut rows = Vec::new();
    for table in ["tasks", "runs", "runner_leases"] {
        let columns: Vec<String> =
            sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .fetch_all(&board.pool)
                .await
                .expect("read a table's columns");
        let quoted = columns
            .iter()
            .map(|column| format!("quote({column})"))
            .collect::<Vec<_>>()
            .join(" || '|' || ");
        let table_rows: Vec<String> =
            sqlx::query_scalar(&format!("SELECT {quoted} FROM {table} ORDER BY rowid"))
                .fetch_all(&board.pool)
                .await
                .expect("read a table's rows");
        rows.extend(table_rows.into_iter().map(|row| format!("{table}: {row}")));
    }
    rows
}

/// `lease`, one generation behind.
fn stale(lease: &LeaseRef) -> LeaseRef {
    LeaseRef {
        generation: lease.generation - 1,
        ..lease.clone()
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
                ceiling: Default::default(),
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
                ceiling: Default::default(),
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
                ceiling: Default::default(),
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

    pub async fn record_branch_sets_the_branch<H: Harness>() {
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
        // The task row carries no worktree path at all since task 066; that
        // the column stays NULL is `a_new_worktree_is_recorded_on_the_runner_
        // and_not_the_board`'s to assert, over real git.
        assert_same(task.branch, Some("rimaia/branched".to_string()), "branch");
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
        let team = harness
            .board()
            .scope
            .sole()
            .expect("one board team")
            .clone();
        let nowhere = LeaseRef::new("no-such-task", 1, team);

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

    /// D31 point 13's 038/039 case. A lease naming another team's task is
    /// `NotFound` in both spellings — with that team's id, and with the
    /// runner's own team on the other team's task — exactly as a never-issued
    /// task is, because the adapter narrows to `LeaseRef.team_id` only after
    /// checking its own scope holds it, and the scoped query refuses a
    /// mismatched pair.
    pub async fn a_lease_on_another_teams_task_is_not_found_in_either_spelling<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let own_team = board.scope.sole().expect("one board team").clone();
        // Another team on the same board, made the way a sign-up makes one.
        let other = {
            let mut tx = board.pool.begin().await.expect("a transaction");
            let team = crate::identity::create_personal_team(&mut tx, harness.clock(), "grace")
                .await
                .expect("a second team");
            tx.commit().await.expect("commit the second team");
            team
        };
        let other_board = board.with_scope(crate::TeamScope::one(other.team_id.clone()));
        let arranged = Arranged::new(&other_board).await;
        let theirs = arranged.task(&other_board, "Theirs").await;
        let runner = harness.runner(Which::A);

        for method in BoardMethod::ALL {
            // The lease-less methods are scoped by the runner, not by a lease,
            // and `publish_tail` cannot fail.
            if !method.takes_a_lease() || method == BoardMethod::PublishTail {
                continue;
            }
            let never_issued = new_id();
            let missing = call(
                runner.as_ref(),
                method,
                &LeaseRef::new(never_issued.clone(), 1, own_team.clone()),
            )
            .await
            .expect_err(method.as_str());

            for lease in [
                LeaseRef::new(theirs.clone(), 1, other.team_id.clone()),
                LeaseRef::new(theirs.clone(), 1, own_team.clone()),
            ] {
                let error = call(runner.as_ref(), method, &lease)
                    .await
                    .expect_err(method.as_str());
                assert_same(error.code(), ErrorCode::NotFound, method.as_str());
                assert_same(
                    error.to_string(),
                    missing.to_string().replace(&never_issued, &theirs),
                    method.as_str(),
                );
            }
        }

        assert_same(
            run_state(&other_board, &theirs).await,
            RunState::Idle,
            "their task is where it was",
        );
        assert_same(run_count(&other_board, &theirs).await, 0, "and has no run");
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

    // -----------------------------------------------------------------------
    // `ClaimTarget::Next` (task 042): the board chooses
    // -----------------------------------------------------------------------

    pub async fn a_next_claim_takes_the_top_startable_task_in_board_order<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let failed = arranged.task(board, "Failed last night").await;
        let top = arranged.task(board, "Top").await;
        let below = arranged.task(board, "Below").await;
        walk(
            board,
            &failed,
            &[RunState::Queued, RunState::Running, RunState::Failed],
        )
        .await;
        let repository = arranged.repository_id.as_str();

        let claim = claimed(
            harness.runner(Which::A).as_ref(),
            next_target(&[repository], 2, &[(repository, 1)]),
        )
        .await;

        assert_same(claim.lease.task_id.clone(), top.clone(), "the task chosen");
        assert_same(claim.purpose, LeasePurpose::Implementation, "purpose");
        assert_same(claim.trigger, RunTrigger::Queued, "trigger");
        assert_same(claim.resume, None, "a fresh start resumes nothing");
        assert_same(
            claim.context.task.task.id,
            top.clone(),
            "the context's task",
        );
        assert_same(
            run_state(board, &top).await,
            RunState::Running,
            "a Next claim takes both edges",
        );
        assert_same(
            run_state(board, &below).await,
            RunState::Idle,
            "one claim, one task",
        );
        assert_same(run_state(board, &failed).await, RunState::Failed, "skipped");
    }

    pub async fn a_next_claim_passes_over_a_repository_the_runner_did_not_list<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let unlisted = Arranged::new(board).await;
        let listed = Arranged::new(board).await;
        let elsewhere = unlisted
            .task(board, "In a repository with no consent")
            .await;
        let mine = listed.task(board, "In a listed repository").await;

        let claim = claimed(
            harness.runner(Which::A).as_ref(),
            next_target(
                &[&listed.repository_id],
                2,
                &[(&unlisted.repository_id, 1), (&listed.repository_id, 1)],
            ),
        )
        .await;

        assert_same(
            claim.lease.task_id.clone(),
            mine,
            "the listed repository's task",
        );
        assert_same(
            run_state(board, &elsewhere).await,
            RunState::Idle,
            "an unlisted repository is not opted in",
        );
    }

    pub async fn a_next_claim_takes_a_listed_repository_whatever_its_ceiling_column_says<
        H: Harness,
    >() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged.task(board, "Registered after 066").await;
        assert_same(
            repo::get(board, &arranged.repository_id)
                .await
                .expect("read the repository")
                .allow_unattended_runs,
            false,
            "registering through core services leaves the team ceiling at 0",
        );
        let repository = arranged.repository_id.as_str();

        let claim = harness
            .runner(Which::A)
            .claim(next_target(&[repository], 1, &[(repository, 1)]))
            .await
            .expect("claim");

        assert_same(
            claim.map(|claim| claim.lease.task_id),
            Some(task_id),
            "042 reads no ceiling; 045 adds it with its personal-team exemption",
        );
    }

    pub async fn a_next_claim_honours_each_repositorys_free_slots<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let full = Arranged::new(board).await;
        let unnamed = Arranged::new(board).await;
        let free = Arranged::new(board).await;
        let in_full = full.task(board, "In a full repository").await;
        let in_unnamed = unnamed.task(board, "In a repository with no entry").await;
        let in_free = free.task(board, "In a repository with room").await;
        let runner = harness.runner(Which::A);

        // `full` reports no free slot and `unnamed` reports none at all, which
        // is the same answer: the capacity is net, and a missing key is no slot.
        let repositories = [
            full.repository_id.as_str(),
            unnamed.repository_id.as_str(),
            free.repository_id.as_str(),
        ];
        let claim = claimed(
            runner.as_ref(),
            next_target(
                &repositories,
                3,
                &[(&full.repository_id, 0), (&free.repository_id, 1)],
            ),
        )
        .await;
        assert_same(claim.lease.task_id, in_free, "the one repository with room");

        // One free slot means one more run, never a cap the board subtracts
        // the running task from again.
        let again = claimed(
            runner.as_ref(),
            next_target(&repositories, 1, &[(&full.repository_id, 1)]),
        )
        .await;
        assert_same(again.lease.task_id, in_full, "a free slot in `full` now");
        assert_same(
            run_state(board, &in_unnamed).await,
            RunState::Idle,
            "never offered",
        );
    }

    pub async fn a_next_claim_with_no_free_capacity_claims_nothing_and_writes_nothing<
        H: Harness,
    >() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged.task(board, "Ready, with nowhere to run").await;
        let before = tasks::get_task(board, &task_id)
            .await
            .expect("read the task");
        harness.clock().advance(Duration::seconds(1));
        let repository = arranged.repository_id.as_str();

        let claim = harness
            .runner(Which::A)
            .claim(next_target(&[repository], 0, &[(repository, 2)]))
            .await
            .expect("no capacity is not an error");

        assert!(claim.is_none(), "a full runner got a claim: {claim:?}");
        assert_same(
            tasks::get_task(board, &task_id)
                .await
                .expect("read the task"),
            before,
            "nothing was written",
        );
        assert_same(run_count(board, &task_id).await, 0, "runs");
    }

    pub async fn a_next_claim_resumes_a_due_retry_with_the_session_it_continues<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged.task(board, "Hit a wall").await;
        walk(board, &task_id, &[RunState::Queued, RunState::Running]).await;
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
        let repository = arranged.repository_id.as_str();
        let runner = harness.runner(Which::A);

        let early = runner
            .claim(next_target(&[repository], 1, &[(repository, 1)]))
            .await
            .expect("claim");
        assert!(early.is_none(), "a retry that is not due yet: {early:?}");

        harness.clock().advance(Duration::minutes(6));
        let claim = claimed(
            runner.as_ref(),
            next_target(&[repository], 1, &[(repository, 1)]),
        )
        .await;

        assert_same(
            claim.lease.task_id.clone(),
            task_id.clone(),
            "the due retry",
        );
        assert_same(
            claim.resume,
            Some(ResumePoint {
                kind: RunKind::Implementation,
                session_id: "the-first-session".to_string(),
            }),
            "the point the retry resumes",
        );
        assert_same(claim.trigger, RunTrigger::Queued, "trigger");
        assert_same(
            run_state(board, &task_id).await,
            RunState::Running,
            "waiting_retry -> running",
        );
    }

    pub async fn two_runners_claiming_next_for_one_task_get_exactly_one_claim<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged.task(board, "Wanted by both").await;
        let repository = arranged.repository_id.as_str();
        let a = harness.runner(Which::A);
        let b = harness.runner(Which::B);

        let (first, second) = tokio::join!(
            a.claim(next_target(&[repository], 1, &[(repository, 1)])),
            b.claim(next_target(&[repository], 1, &[(repository, 1)])),
        );
        let claims: Vec<Claim> = [first, second]
            .into_iter()
            .filter_map(|claim| claim.expect("a lost race is not an error"))
            .collect();

        assert_same(claims.len(), 1, "exactly one runner holds the task");
        assert_same(claims[0].lease.task_id.clone(), task_id.clone(), "the task");
        assert_same(
            run_state(board, &task_id).await,
            RunState::Running,
            "running, once",
        );
        assert_same(run_count(board, &task_id).await, 0, "runs");
    }

    pub async fn a_waiting_next_claim_returns_as_soon_as_a_task_becomes_startable<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged
            .task_in(board, "Not ready yet", BoardColumn::NotReady)
            .await;
        let repository = arranged.repository_id.clone();
        let runner = harness.runner(Which::A);

        let waiting = tokio::spawn(async move {
            runner
                .claim(next_target_waiting(
                    &[&repository],
                    1,
                    &[(&repository, 1)],
                    StdDuration::from_secs(60),
                ))
                .await
        });
        converge().await;
        assert!(
            !waiting.is_finished(),
            "a claim with nothing startable and a minute to wait returned early"
        );

        tasks::move_task(board, None, &task_id, BoardColumn::Ready, None, None)
            .await
            .expect("move the task to ready");

        let claim = joined(waiting, "the claim to wake on the move").await;
        assert_same(
            claim.map(|claim| claim.lease.task_id),
            Some(task_id),
            "the task that became startable, with the clock not advanced",
        );
    }

    pub async fn a_waiting_next_claim_returns_none_once_its_wait_has_passed<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged
            .task_in(board, "Never made ready", BoardColumn::NotReady)
            .await;
        let before = tasks::get_task(board, &task_id)
            .await
            .expect("read the task");
        let repository = arranged.repository_id.clone();
        let runner = harness.runner(Which::A);

        let waiting = tokio::spawn(async move {
            runner
                .claim(next_target_waiting(
                    &[&repository],
                    1,
                    &[(&repository, 1)],
                    StdDuration::from_secs(30),
                ))
                .await
        });
        converge().await;
        assert!(!waiting.is_finished(), "returned before its wait passed");

        harness.clock().advance(Duration::seconds(31));

        let claim = joined(waiting, "the claim to give up at its deadline").await;
        assert!(claim.is_none(), "nothing was startable: {claim:?}");
        assert_same(
            tasks::get_task(board, &task_id)
                .await
                .expect("read the task"),
            before,
            "nothing was written",
        );
    }

    // -----------------------------------------------------------------------
    // Leases, fencing and generations (task 043)
    // -----------------------------------------------------------------------

    pub async fn two_runners_racing_for_one_task_get_exactly_one_claim<H: Harness>() {
        // The lease form of the `Next` race: two runners name one task at
        // once, fifty times over, on a board served over several connections.
        // The claim is one transaction with the lease's primary key behind it,
        // so every time one wins and the other is told so — never an error,
        // which is what a refused upgrade of a deferred transaction would be.
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let a = harness.runner(Which::A);
        let b = harness.runner(Which::B);

        for round in 0..50 {
            let task_id = arranged
                .task(board, &format!("Wanted by both, {round}"))
                .await;

            let (first, second) = tokio::join!(
                a.claim(run_target(&task_id, false)),
                b.claim(run_target(&task_id, false)),
            );
            let first = first.unwrap_or_else(|error| panic!("round {round}, A: {error}"));
            let second = second.unwrap_or_else(|error| panic!("round {round}, B: {error}"));
            let claims: Vec<Claim> = [first, second].into_iter().flatten().collect();

            assert_same(claims.len(), 1, &format!("round {round}: one claim"));
            assert_same(claims[0].lease.generation, 1, "the first generation");
            let held = lease_of(board, &task_id).await.expect("one lease row");
            assert_same(held.generation, 1, "the lease row's generation");
            assert_same(
                run_state(board, &task_id).await,
                RunState::Running,
                "running, once",
            );
        }
    }

    pub async fn every_lease_method_refuses_a_stale_generation<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged.task(board, "Fenced").await;
        let runner = harness.runner(Which::A);
        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        runner
            .start_run(&claim.lease, starting(&new_id(), RunKind::Implementation))
            .await
            .expect("start the run");
        let behind = stale(&claim.lease);
        let before = rows_a_lease_method_could_write(board).await;
        harness.clock().advance(Duration::seconds(1));

        for method in BoardMethod::ALL {
            match method {
                // Unfenced on purpose: the first three act before any lease
                // exists, and a tail is synchronous and worth nothing stale
                // (D14), so task 052's handler drops it instead.
                BoardMethod::Preview
                | BoardMethod::Claim
                | BoardMethod::Heartbeat
                | BoardMethod::PublishTail => continue,
                BoardMethod::RunContext
                | BoardMethod::RecordBranch
                | BoardMethod::StartRun
                | BoardMethod::AppendTranscript
                | BoardMethod::FinishRun
                | BoardMethod::Release
                | BoardMethod::RecordStrategy
                | BoardMethod::RecordReviewFindings => {
                    let error = call(runner.as_ref(), method, &behind)
                        .await
                        .expect_err(method.as_str());
                    assert_same(error.code(), ErrorCode::Conflict, method.as_str());
                }
            }
        }

        assert_same(
            rows_a_lease_method_could_write(board).await,
            before,
            "a fenced report writes nothing",
        );
    }

    pub async fn a_lease_naming_a_missing_task_is_not_found_and_a_released_lease_is_conflict<
        H: Harness,
    >() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged.task(board, "Released").await;
        let runner = harness.runner(Which::A);
        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        runner.release(&claim.lease).await.expect("release");
        let missing = LeaseRef {
            task_id: new_id(),
            ..claim.lease.clone()
        };

        for method in BoardMethod::ALL {
            if !method.takes_a_lease() || method == BoardMethod::PublishTail {
                continue;
            }
            let error = call(runner.as_ref(), method, &missing)
                .await
                .expect_err(method.as_str());
            assert_same(error.code(), ErrorCode::NotFound, method.as_str());

            let error = call(runner.as_ref(), method, &claim.lease)
                .await
                .expect_err(method.as_str());
            assert_same(error.code(), ErrorCode::Conflict, method.as_str());
        }
    }

    pub async fn generation_increases_across_a_release_and_a_reclaim<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged.task(board, "Claimed twice").await;
        let runner = harness.runner(Which::A);

        let first = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        assert_same(first.lease.generation, 1, "the first claim");
        runner.release(&first.lease).await.expect("release");
        let second = claimed(runner.as_ref(), run_target(&task_id, false)).await;

        assert_same(second.lease.generation, 2, "never repeated after a release");
        let error = runner
            .run_context(&first.lease)
            .await
            .expect_err("the first holder is fenced");
        assert_same(error.code(), ErrorCode::Conflict, "the error code");
        runner
            .run_context(&second.lease)
            .await
            .expect("the current holder reads");
    }

    pub async fn the_heartbeat_renews_current_leases_and_fences_stale_ones_per_lease<H: Harness>() {
        let harness = H::start_with(LeaseTerm::Renewable(LEASE_LIFETIME)).await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let runner = harness.runner(Which::A);
        let kept = arranged.task(board, "Kept").await;
        let moved_on = arranged.task(board, "Moved on").await;
        let current = claimed(runner.as_ref(), run_target(&kept, false)).await;
        let lifetime = Duration::from_std(LEASE_LIFETIME).expect("three minutes");
        assert_same(
            lease_of(board, &kept)
                .await
                .and_then(|lease| lease.expires_at),
            Some(harness.clock().now() + lifetime),
            "a renewable lease expires a lifetime after its claim",
        );
        let old = claimed(runner.as_ref(), run_target(&moved_on, false)).await;
        runner.release(&old.lease).await.expect("release");
        claimed(runner.as_ref(), run_target(&moved_on, false)).await;

        harness.clock().advance(Duration::minutes(2));
        let heartbeat = runner
            .heartbeat(&[current.lease.clone(), old.lease.clone()])
            .await
            .expect("heartbeat");

        assert_same(heartbeat.fenced, vec![old.lease.clone()], "fenced");
        assert_same(heartbeat.cancel, Vec::<String>::new(), "cancel");
        assert_same(
            lease_of(board, &kept)
                .await
                .and_then(|lease| lease.expires_at),
            Some(harness.clock().now() + lifetime),
            "the current lease was renewed beside the stale one",
        );
    }

    pub async fn a_solo_lease_survives_a_week_of_clock_time<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let task_id = arranged.task(board, "A long week").await;
        let runner = harness.runner(Which::A);
        let claim = claimed(runner.as_ref(), run_target(&task_id, false)).await;
        let expires = || async {
            lease_of(board, &task_id)
                .await
                .expect("the lease is still there")
                .expires_at
        };
        assert_same(expires().await, None, "a solo lease never expires");

        harness.clock().advance(Duration::days(7));
        runner
            .run_context(&claim.lease)
            .await
            .expect("still the holder");
        let run_id = new_id();
        runner
            .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
            .await
            .expect("start the run");
        assert_same(expires().await, None, "still no expiry");
        let receipt = runner
            .finish_run(&claim.lease, &run_id, finishing(succeeded()))
            .await
            .expect("finish the run");

        assert_same(
            receipt.next,
            NextStep::Released { resume_after: None },
            "the finish landed",
        );
    }

    // -----------------------------------------------------------------------
    // Task 067: the model rule, and who may start a runner
    // -----------------------------------------------------------------------

    pub async fn a_runner_is_not_offered_a_task_whose_model_belongs_to_another_provider<
        H: Harness,
    >() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let repository = arranged.repository_id.as_str();
        let task_id = arranged.task(board, "Asks for opus").await;
        ask_for_model(board, &task_id, "opus", None).await;
        harness.clock().advance(Duration::seconds(1));
        let before = rows_a_lease_method_could_write(board).await;
        let ledger = harness.runner_on(Which::B, ProviderId::Ledger);

        let passed_over = ledger
            .claim(next_target(&[repository], 1, &[(repository, 1)]))
            .await
            .expect("passing a task over is not an error");
        assert!(
            passed_over.is_none(),
            "a Ledger runner was offered a task set to opus: {passed_over:?}"
        );

        // The plan a card shows agrees with the claim: Ledger's leaves the task
        // out, Claude's has it next. Neither is pinned, so the runner a view
        // names does not matter here.
        let plan_for = |provider| {
            let view = RunnerView::new("a-plan-view", provider, [repository.to_string()]);
            async move { selection::plan(board, &view).await.expect("draw a plan") }
        };
        assert!(
            !plan_for(ProviderId::Ledger)
                .await
                .iter()
                .any(|entry| entry.task_id == task_id),
            "Ledger's plan lists a task it cannot run",
        );
        let claude_plan = plan_for(ProviderId::ClaudeCode).await;
        let next = claude_plan
            .iter()
            .find(|entry| entry.task_id == task_id)
            .expect("Claude's plan lists the task");
        assert_same(next.queue_position, Some(1), "next for Claude");

        let refused = ledger
            .claim(run_target(&task_id, false))
            .await
            .expect_err("a named claim is refused, in a sentence");
        assert_same(refused.code(), ErrorCode::Invalid, "the code");
        assert_same(
            refused.to_string(),
            model_refusal("opus", ProviderId::Ledger),
            "the sentence",
        );
        assert_same(
            rows_a_lease_method_could_write(board).await,
            before,
            "nothing was written",
        );

        let claim = harness
            .runner(Which::A)
            .claim(next_target(&[repository], 1, &[(repository, 1)]))
            .await
            .expect("claim")
            .expect("a Claude runner takes the task");
        assert_same(claim.lease.task_id, task_id, "A's claim");
    }

    pub async fn a_continue_into_a_review_whose_model_this_provider_cannot_run_is_released<
        H: Harness,
    >() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        // No model of its own, so Ledger can implement it; the review loop
        // names Claude's.
        let task_id = arranged.task(board, "Reviewed by opus").await;
        review_config::set_review_settings(
            board,
            &ClaudeProvider,
            "",
            serde_json::json!({ "enabled": "on_cost_acknowledged", "review_model": "opus" }),
        )
        .await
        .expect("turn the loop on with a review model");
        let ledger = harness.runner_on(Which::B, ProviderId::Ledger);
        let claim = claimed(ledger.as_ref(), run_target(&task_id, false)).await;
        let run_id = new_id();
        ledger
            .start_run(&claim.lease, starting(&run_id, RunKind::Implementation))
            .await
            .expect("start the implementation");

        let receipt = ledger
            .finish_run(&claim.lease, &run_id, finishing_at(succeeded(), "a1"))
            .await
            .expect("finish it");

        assert_same(
            receipt.next,
            NextStep::Released { resume_after: None },
            "the loop ends here rather than continuing into a review Ledger cannot run",
        );
        assert_same(lease_of(board, &task_id).await, None, "the lease is gone");
        let runs = runs::list_runs_for_task(board, &task_id)
            .await
            .expect("read the runs");
        assert_same(
            runs.iter().map(|run| run.kind).collect::<Vec<_>>(),
            vec![RunKind::Implementation],
            "no review run was started",
        );
        let detail = tasks::get_task(board, &task_id)
            .await
            .expect("read the task");
        assert_same(detail.task.column, BoardColumn::InReview, "the column");
        assert_same(detail.task.run_state, RunState::Idle, "the run state");
    }

    pub async fn a_plan_claim_and_an_inline_planner_claim_are_not_subject_to_the_model_rule<
        H: Harness,
    >() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let ledger = harness.runner_on(Which::B, ProviderId::Ledger);

        // Plan now on a card set to opus: the planner chooses from Ledger's
        // own catalogue, so the card's model is not the planner's.
        let by_hand = arranged.task(board, "Planned by hand").await;
        ask_for_model(board, &by_hand, "opus", None).await;
        let plan = claimed(
            ledger.as_ref(),
            ClaimTarget::Plan {
                task_id: by_hand.clone(),
                ceiling: Default::default(),
            },
        )
        .await;
        assert_same(plan.purpose, LeasePurpose::Strategy, "Plan now's purpose");
        assert_same(
            plan.context.strategy.model.as_deref(),
            Some("opus"),
            "the model the rule would have refused",
        );

        // A fresh start that needs planning is leased as `strategy`, ADR-0016's
        // inline planner, and is exempt for the same reason.
        let inline = arranged.task(board, "Planned inline").await;
        ask_for_model(board, &inline, "opus", Some(StrategyMode::Planned)).await;
        let started = claimed(ledger.as_ref(), run_target(&inline, false)).await;
        assert_same(
            started.purpose,
            LeasePurpose::Strategy,
            "the inline planner's purpose",
        );
        assert_same(
            started.context.strategy.model.as_deref(),
            Some("opus"),
            "the model the rule would have refused",
        );
    }

    pub async fn run_now_is_not_bound_by_capacity<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let arranged = Arranged::new(board).await;
        let repository = arranged.repository_id.as_str();
        let task_id = arranged
            .task(board, "Started by hand on a full runner")
            .await;
        let runner = harness.runner(Which::A);

        let queued = runner
            .claim(next_target(&[repository], 0, &[(repository, 0)]))
            .await
            .expect("no capacity is not an error");
        assert!(
            queued.is_none(),
            "a full runner's queue claimed: {queued:?}"
        );

        // D19 point 5: a named start carries no capacity, and the board applies
        // none.
        let claim = claimed(
            runner.as_ref(),
            ClaimTarget::Run {
                task_id: task_id.clone(),
                trigger: RunTrigger::Manual,
                continue_session: false,
                ceiling: Default::default(),
            },
        )
        .await;
        assert_same(&claim.lease.task_id, &task_id, "Run now claims");
        assert_same(claim.trigger, RunTrigger::Manual, "as a manual run");
        assert_same(
            run_state(board, &task_id).await,
            RunState::Running,
            "running",
        );
    }

    pub async fn only_a_runners_owner_is_authorized_to_start_it<H: Harness>() {
        let harness = H::start().await;
        let board = harness.board();
        let (member, theirs) = harness.add_member_with_runner().await;

        let refused = authorize_start(board, &theirs, OwnerPresence::AtRunner)
            .await
            .expect_err("a teammate's runner is not the caller's to start");
        assert_same(refused.code(), ErrorCode::Invalid, "the code");
        assert_same(
            refused.to_string(),
            "only the owner of this runner can start a run on it; assign the task to them, or \
             leave it ready for their queue"
                .to_string(),
            "the sentence",
        );

        let as_them = ServiceContext {
            actor: member,
            ..board.clone()
        };
        assert_same(
            authorize_start(&as_them, &theirs, OwnerPresence::AtRunner)
                .await
                .expect("its owner may start it"),
            RunTrigger::Manual,
            "at the runner",
        );
        assert_same(
            authorize_start(&as_them, &theirs, OwnerPresence::Remote)
                .await
                .expect("its owner may start it from elsewhere"),
            RunTrigger::Queued,
            "away from it",
        );
    }
}
