//! The runner's side of the board port (task 036; ADR-0027 point 5,
//! seam-contract D31): what a starter, the queue and `run_task` do with what
//! the board answers.
//!
//! `board_port_in_process.rs` holds the port's own contract. This file holds
//! the behaviour that moved when the runner started reaching the board only
//! through it: the manual starter's lost races, a resume claimed as the kind
//! that was waiting, a claim the runner refuses after taking it, and the
//! usage-limit pause the runner raises around `finish_run`.
//!
//! The CLI is `testing::FakeCli` replaying recorded streams, git runs against
//! real repositories in temporary directories, and the clock is the harness's.
//! Nothing sleeps: the `timeout`s are failure bounds, not waits.

#![cfg(unix)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::board::{
    BoardFuture, BoardPort, Claim, ClaimTarget, FinishReceipt, FinishRun, Heartbeat, LeasePurpose,
    LeaseRef, RunContext, StartRun, TranscriptAck, TranscriptChunk,
};
use rimaia_core::db::{BoardColumn, ExitClass, Run, RunKind, RunState, RunStatus, ScheduleMode};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::review::findings::NewReviewFinding;
use rimaia_core::runner::events::RunTail;
use rimaia_core::runner::outcome::{start_run, NewRun};
use rimaia_core::runner::provider::ProviderId;
use rimaia_core::runner::{
    claim_manual_start, run_task, ManualStart, RunRequest, RunTrigger, RunnerConfig,
};
use rimaia_core::schedule::window::{self, RunWindow};
use rimaia_core::scheduler::retry::{self, USAGE_LIMIT_MAX_JITTER};
use rimaia_core::scheduler::{self, pause, InFlight, ResumePoint};
use rimaia_core::tasks::strategy::StrategyPlan;
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::board::claim_run;
use rimaia_core::testing::provider::Ledger;
use rimaia_core::testing::{self, FakeCli, TempRepo, TestContext};
use rimaia_core::{AppPaths, Clock, ErrorCode, ServiceContext};
use tempfile::TempDir;

/// A failure bound for anything that waits on a child or on the queue.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// What `usage-limit.jsonl`'s `resetsAt` decodes to: five hours after
/// `test_epoch`, the wall task 014 exists for.
const REPORTED_RESET: &str = "2026-08-20T07:00:00Z";

fn reported_reset() -> DateTime<Utc> {
    REPORTED_RESET.parse().expect("a literal timestamp")
}

// ---------------------------------------------------------------------------
// The manual starter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_lost_manual_start_answers_with_todays_sentence() {
    // The queue got there first: it claimed the task between the button's
    // preview and its own claim. The sentence is the one `start_task_run` has
    // always answered with, and the queue's claim is left exactly as it was.
    let fixture = Fixture::new().await;
    let config = fixture.config();
    let board = fixture.board(&config);
    claim_run(board.as_ref(), &fixture.task_id, RunTrigger::Queued, false)
        .await
        .expect("the queue's claim");
    let before = fixture.detail().await;

    let error = fixture
        .start_by_hand(&config, false)
        .await
        .err()
        .expect("the queue holds the task");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        "the run queue is already working on this task; pause or stop the queue, \
         or wait for it to finish, before starting it by hand",
    );
    assert_eq!(
        fixture.detail().await,
        before,
        "the losing start wrote nothing"
    );
    assert_eq!(fixture.cli.started(), Vec::<String>::new());
}

#[tokio::test]
async fn a_lost_retry_start_answers_with_todays_sentence() {
    // Retry now on a task that is no longer waiting: the queue resumed it, or
    // its retries ran out, a moment before the click.
    let fixture = Fixture::new().await;
    let config = fixture.config();
    let before = fixture.detail().await;

    let error = fixture
        .start_by_hand(&config, true)
        .await
        .err()
        .expect("the task is not waiting to be retried");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        "this task is not waiting to be retried; the queue may have already picked it up, \
         or its retries may have run out",
    );
    assert_eq!(
        fixture.detail().await,
        before,
        "the losing retry wrote nothing"
    );
    assert_eq!(fixture.cli.started(), Vec::<String>::new());
}

#[tokio::test]
async fn a_retry_claim_for_a_waiting_review_claims_it_as_a_review() {
    // Task 035 refused this claim; task 021 resumes a review as a review. The
    // point carries the kind, and the lease is for it (D29 points 1 and 3).
    let fixture = Fixture::new().await;
    let ctx = fixture.ctx();
    for state in [RunState::Queued, RunState::Running] {
        tasks::set_run_state(ctx, &fixture.task_id, state)
            .await
            .expect("walk to running");
    }
    let review = start_run(
        ctx,
        &fixture.paths,
        NewRun {
            task_id: fixture.task_id.clone(),
            kind: RunKind::Review,
            session_id: "review-session".to_string(),
            prompt: "review the work".to_string(),
            base_ref: None,
            base_sha: None,
        },
    )
    .await
    .expect("open the review row")
    .id;
    let due = fixture.harness.clock.now() - TimeDelta::minutes(1);
    testing::runs::close_run(
        ctx,
        &review,
        RunStatus::Failed,
        ExitClass::Transient,
        Some(due),
    )
    .await;
    tasks::set_run_state(ctx, &fixture.task_id, RunState::WaitingRetry)
        .await
        .expect("the review is waiting to be retried");

    let config = fixture.config();
    let claim = fixture
        .board(&config)
        .claim(ClaimTarget::Run {
            task_id: fixture.task_id.clone(),
            trigger: RunTrigger::Queued,
            continue_session: true,
        })
        .await
        .expect("a review is resumed")
        .expect("nobody else holds the task");

    assert_eq!(
        claim.resume,
        Some(ResumePoint {
            kind: RunKind::Review,
            session_id: "review-session".to_string(),
        })
    );
    assert_eq!(claim.purpose, LeasePurpose::Review);
    assert_eq!(fixture.detail().await.task.run_state, RunState::Running);
}

// ---------------------------------------------------------------------------
// A claim the runner refuses after taking it
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_task_releases_a_claim_whose_context_no_longer_negotiates() {
    // D31 point 5: `run_task` negotiates again against the claim's own context,
    // and a refusal there is a refusal after the claim, so it gives the claim
    // back. The queue claims without negotiating, so a provider that cannot
    // deny a tool (ADR-0026 point 4) meets its refusal exactly there.
    let fixture = Fixture::new().await;
    let config = RunnerConfig {
        provider: Arc::new(Ledger),
        program: fixture.cli.program_for(ProviderId::Ledger),
        ..RunnerConfig::default()
    };
    let (queue, task) = scheduler::build(
        fixture.board(&config),
        fixture.machine().clone(),
        fixture.ctx().clone(),
        fixture.paths.clone(),
        config,
        InFlight::new(),
    );
    tokio::spawn(task.run());
    let mut changes = fixture.ctx().subscribe();

    queue.start().await.expect("start the queue");
    tokio::time::timeout(TEST_TIMEOUT, async {
        while fixture.detail().await.task.run_state != RunState::Failed {
            changes.recv().await.ok();
        }
    })
    .await
    .expect("the claimed task was never released");

    let detail = fixture.detail().await;
    assert_eq!(detail.task.run_state, RunState::Failed);
    assert_eq!(detail.last_run, None, "no `runs` row was opened");
    assert_eq!(
        fixture.cli.started(),
        Vec::<String>::new(),
        "nothing was spawned"
    );

    queue.shutdown();
}

// ---------------------------------------------------------------------------
// The usage-limit pause, around the board's decision
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_usage_limit_holds_new_starts_before_the_board_hears_the_run_finished() {
    // `finish_run` publishes, and the publication wakes the queue. The pause
    // has to exist by then, or a free slot starts another task into the window
    // this run just found closed (the D31 amendment of 2026-10-09).
    let fixture = Fixture::new().await;
    fixture.cli.replays(&fixture.task_id, "usage-limit", 143);
    let config = fixture.config();
    let witness = PauseWitness {
        inner: fixture.board(&config),
        machine: fixture.machine().clone(),
        seen: Mutex::new(None),
    };

    let run = fixture
        .run(&witness, &config)
        .await
        .expect("the attempt is recorded");

    assert_eq!(run.exit_class, Some(ExitClass::UsageLimit));
    assert_eq!(
        *witness.seen.lock().expect("the witness lock"),
        Some(Some(reported_reset())),
        "new starts were held until the reported reset when the board heard of it",
    );
}

#[tokio::test]
async fn the_pause_a_usage_limit_leaves_is_the_instant_the_board_chose_to_resume_at() {
    // After `NextStep::Released`, the pause moves to the board's own
    // `resume_after`: the reset plus this run's jitter, so the queue does not
    // wake a minute before the task it is waiting for is due.
    let fixture = Fixture::new().await;
    fixture.cli.replays(&fixture.task_id, "usage-limit", 143);
    let config = fixture.config();

    let run = fixture
        .run(fixture.board(&config).as_ref(), &config)
        .await
        .expect("the attempt is recorded");

    let chosen = reported_reset() + retry::jitter(&run.id, USAGE_LIMIT_MAX_JITTER);
    assert_eq!(run.resume_after, Some(chosen));
    assert_eq!(fixture.paused_until().await, Some(chosen));
    assert_eq!(
        fixture.detail().await.task.run_state,
        RunState::WaitingRetry
    );
}

#[tokio::test]
async fn a_usage_limit_that_outlasts_the_run_window_still_holds_new_starts_until_the_reset() {
    // ADR-0011's cap: a reset after tonight's window closes is not retried
    // tonight. The pause still holds until the reset, because the limit is the
    // account's: a task started before it would hit the same wall.
    let fixture = Fixture::new().await;
    fixture.cli.replays(&fixture.task_id, "usage-limit", 143);
    let now = fixture.harness.clock.now();
    window::open(
        fixture.machine(),
        &RunWindow {
            schedule_id: "tonight".to_string(),
            schedule_name: "Tonight".to_string(),
            opened_at: now,
            closes_at: Some(reported_reset() - TimeDelta::hours(1)),
            mode: ScheduleMode::Sequential,
            max_concurrency: 1,
        },
    )
    .await
    .expect("open a window that closes before the reset");
    let config = fixture.config();

    let run = fixture
        .run(fixture.board(&config).as_ref(), &config)
        .await
        .expect("the attempt is recorded");

    assert_eq!(run.exit_class, Some(ExitClass::UsageLimit));
    assert_eq!(run.resume_after, None, "the reset outlasts the run window");
    assert_eq!(fixture.paused_until().await, Some(reported_reset()));
}

#[tokio::test]
async fn a_usage_limit_pause_is_held_by_the_runner() {
    // Task 041: the pause is this machine's, so a run hitting a wall writes it
    // to the machine store, the board's legacy copy of the key is left exactly
    // as it was, and the queue reads the machine's hold, starting nothing
    // until the clock passes it.
    let fixture = Fixture::new().await;
    sqlx::query("INSERT INTO settings (key, value) VALUES (?1, 'the board copy')")
        .bind(pause::USAGE_LIMIT_PAUSE_UNTIL)
        .execute(&fixture.ctx().pool)
        .await
        .expect("plant the board's legacy row");
    fixture
        .cli
        .replays_on_attempt(&fixture.task_id, 1, "usage-limit", 143);
    let config = fixture.config();

    let run = fixture
        .run(fixture.board(&config).as_ref(), &config)
        .await
        .expect("the attempt is recorded");

    assert_eq!(run.exit_class, Some(ExitClass::UsageLimit));
    let held_until = run.resume_after.expect("a usage limit is retried");
    assert_eq!(fixture.paused_until().await, Some(held_until));
    assert_eq!(
        fixture
            .machine()
            .store
            .get_setting(pause::USAGE_LIMIT_PAUSE_UNTIL)
            .await
            .expect("read the machine store"),
        Some(held_until.to_rfc3339()),
    );
    assert_eq!(
        board_row(fixture.ctx(), pause::USAGE_LIMIT_PAUSE_UNTIL).await,
        Some("the board copy".to_string()),
        "the board's row is unchanged"
    );

    // A second ready task, which a free slot would start at once if nothing
    // held it.
    let held = fixture.add_task("Would burn a start").await;
    let (queue, loop_task) = scheduler::build(
        fixture.board(&config),
        fixture.machine().clone(),
        fixture.ctx().clone(),
        fixture.paths.clone(),
        config,
        InFlight::new(),
    );
    tokio::spawn(loop_task.run());
    let mut changes = fixture.ctx().subscribe();
    queue.start().await.expect("start the queue");
    converge().await;

    assert_eq!(
        fixture.cli.started(),
        vec![fixture.task_id.clone()],
        "nothing starts while the machine holds new starts"
    );
    assert_eq!(
        queue
            .status()
            .await
            .expect("read the status")
            .usage_limit_pause_until,
        Some(held_until)
    );

    fixture
        .harness
        .clock
        .set(held_until + TimeDelta::minutes(1));
    tokio::time::timeout(TEST_TIMEOUT, async {
        while !fixture.cli.started().contains(&held) {
            changes.recv().await.ok();
        }
    })
    .await
    .expect("the queue starts once the clock passes the hold");

    queue.shutdown();
}

/// The board's legacy `settings` row for `key`, read past every accessor.
async fn board_row(ctx: &ServiceContext, key: &str) -> Option<String> {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = ?1")
        .bind(key)
        .fetch_optional(&ctx.pool)
        .await
        .expect("read the board's legacy row")
}

/// Gives the queue's loop every chance to act on what it has already been
/// told, without waiting on a clock: a start it was going to make has been
/// made once this returns.
async fn converge() {
    for _ in 0..2_000 {
        tokio::task::yield_now().await;
    }
}

/// A board that reads the usage-limit pause the moment `finish_run` is
/// entered, and otherwise passes every call through.
struct PauseWitness {
    inner: Arc<dyn BoardPort>,
    /// The runner's machine, where the pause is held (task 041).
    machine: rimaia_core::machine::MachineContext,
    /// `Some(pause)` once `finish_run` has been entered.
    seen: Mutex<Option<Option<DateTime<Utc>>>>,
}

impl BoardPort for PauseWitness {
    fn preview<'a>(&'a self, task_id: &'a str) -> BoardFuture<'a, RunContext> {
        self.inner.preview(task_id)
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
            let paused = pause::active_until(&self.machine, self.machine.clock.now()).await?;
            *self.seen.lock().expect("the witness lock") = Some(paused);
            self.inner.finish_run(lease, run_id, finish).await
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

// ---------------------------------------------------------------------------
// One opted-in repository with one ready task
// ---------------------------------------------------------------------------

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
            harness.machine(),
            &paths.worktrees_dir(),
            NewRepository {
                path: repository.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register a test repository");
        repo::set_allow_unattended_runs(&harness.context, harness.machine(), &registered.id, true)
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

    fn ctx(&self) -> &ServiceContext {
        &self.harness.context
    }

    /// This machine's own state, over the harness's machine store (task 041).
    fn machine(&self) -> &rimaia_core::machine::MachineContext {
        self.harness.machine()
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

    /// Run now or Retry now, through the starter both buttons call.
    async fn start_by_hand(
        &self,
        config: &RunnerConfig,
        continue_session: bool,
    ) -> rimaia_core::Result<rimaia_core::runner::Started> {
        claim_manual_start(
            self.board(config).as_ref(),
            self.machine(),
            &self.paths,
            config,
            &InFlight::new(),
            ManualStart {
                task_id: self.task_id.clone(),
                trigger: RunTrigger::Manual,
                continue_session,
            },
        )
        .await
    }

    /// A queued run claimed and run through `board`, the trigger every
    /// recording was captured under.
    async fn run(&self, board: &dyn BoardPort, config: &RunnerConfig) -> rimaia_core::Result<Run> {
        let claim = claim_run(board, &self.task_id, RunTrigger::Queued, false).await?;
        tokio::time::timeout(
            TEST_TIMEOUT,
            run_task(
                board,
                self.machine(),
                self.ctx(),
                &self.paths,
                config,
                claim,
                RunRequest::default(),
            ),
        )
        .await
        .expect("a run must finish inside the test timeout")
    }

    /// Another ready task in the fixture's repository.
    async fn add_task(&self, title: &str) -> String {
        let repository_id = self.detail().await.task.repository_id;
        tasks::create_task(
            self.ctx(),
            NewTask {
                repository_id,
                title: title.to_string(),
                plan: Some("1. Implement it".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a ready task")
        .id
    }

    async fn detail(&self) -> tasks::TaskDetail {
        tasks::get_task(self.ctx(), &self.task_id)
            .await
            .expect("read the task")
    }

    async fn paused_until(&self) -> Option<DateTime<Utc>> {
        pause::active_until(self.machine(), self.harness.clock.now())
            .await
            .expect("read the usage-limit pause")
    }
}
