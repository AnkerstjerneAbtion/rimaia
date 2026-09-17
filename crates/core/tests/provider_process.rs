#![cfg(unix)]
//! A second provider, driving a real child (ADR-0026).
//!
//! # Why this file exists at all
//!
//! `provider_seam.rs` proves the seam against values — argv, negotiation,
//! parsing, classification — and it would pass just as happily if `execute` had
//! never been wired to a provider at all, because `RunnerConfig::default()` and
//! `EventStream`'s builder both mean Claude Code. **That default is what keeps
//! test churn near zero and also what would let a missed wiring pass silently.**
//! The guard is below: a real child, a real pipe, a stream in a vocabulary
//! nothing else in the codebase reads, and a task that has to land in
//! `in_review` at the end of it.
//!
//! # `#![cfg(unix)]`, for `runner_process.rs`'s reason
//!
//! The stand-in is a POSIX shell script and cancellation is a signal to a
//! process group. That is an honest gap rather than a stub pretending to be a
//! port — and it is why the tests that catch the *leaks* live next door, in a
//! file CI also runs on Windows.
//!
//! # Nothing here starts a real agent CLI
//!
//! Same two reasons `runner_process.rs` gives, and one more: the provider below
//! is fictional, so there is nothing to start. What is real is everything Rimaia
//! owns — spawn, argv, environment, cwd, stdin, pipes, exit status, signals,
//! process groups.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pretty_assertions::assert_eq;
use rimaia_core::db::{BoardColumn, ExitClass, RunState, StrategyMode};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::events::RunTail;
use rimaia_core::runner::provider::{ClaudeProvider, ProviderId};
use rimaia_core::runner::{
    run_task, AgentProvider, CancelSignal, RunRequest, RunnerConfig, STRATEGY_TRANSCRIPT_PREFIX,
};
use rimaia_core::strategy::catalogue;
use rimaia_core::tasks::strategy::{StrategyPlan, StrategyPlanStatus};
use rimaia_core::tasks::{self, NewTask, TaskPatch};
use rimaia_core::testing::fixtures::path_for;
use rimaia_core::testing::provider::{Ledger, LedgerWithoutResume};
use rimaia_core::testing::{FakeCli, TempRepo, TestContext};
use rimaia_core::AppPaths;
use tempfile::TempDir;
use tokio::sync::broadcast::Receiver;

/// A ceiling on any single process test. Long enough that a slow machine never
/// trips it, short enough that a supervision bug fails rather than hangs.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// What a killed run of this provider exits with. **Not 143.** Nothing in Rimaia
/// may be written against one provider's exit code, and this is the number that
/// catches it if anything is.
const LEDGER_KILLED: i32 = 137;

// ---------------------------------------------------------------------------
// The test that makes "a second provider just implements the trait" true
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_run_driven_by_the_second_provider_reaches_in_review() {
    // **Not optional.** Every other test in this pair could pass with `execute`
    // still hardcoded to Claude Code; this one cannot. A missed wiring returns
    // the wrong `ExitClass` here, because the fixture is in a vocabulary the
    // other provider reads as five opaque events and no ending at all.
    //
    // It is a *manual* run, and that is not a convenience: this provider cannot
    // express a tool blocklist, so an unattended one is refused outright
    // (ADR-0012, and `provider_seam.rs` asserts that). A human started this one.
    let fixture = Fixture::new().await;
    let cli = FakeCli::new();
    cli.replays_path(
        &fixture.task_id,
        &path_for(ProviderId::Ledger, "finished"),
        0,
    );

    let run = tokio::time::timeout(TEST_TIMEOUT, fixture.run(&cli))
        .await
        .expect("the run finishes")
        .expect("a manual run on a provider that cannot deny tools still runs");

    assert_eq!(run.exit_class, Some(ExitClass::Success));
    assert_eq!(
        fixture.detail().await.task.column,
        BoardColumn::InReview,
        "the whole promise of the board: work that finished is waiting to be reviewed",
    );

    // The transcript is the provider's bytes, unedited — ADR-0013's record of the
    // run, and evidence that nothing on the way through rewrote the stream into
    // some other provider's shape.
    let recorded =
        std::fs::read_to_string(path_for(ProviderId::Ledger, "finished")).expect("the fixture");
    let transcript = std::fs::read_to_string(&run.log_path).expect("a written transcript");
    assert_eq!(transcript, recorded);

    // The prompt arrived whole, on stdin, and was closed — the mechanism
    // `spike/FINDINGS.md` §7 measured as the thing that hangs a run.
    assert_eq!(
        cli.stdin(&fixture.task_id, 1),
        run.prompt,
        "the composed prompt, byte for byte",
    );

    // ADR-0022's capture, under a provider that reports no price at all.
    // Seam-contract D18: absent is NULL, never zero.
    assert_eq!(run.cost_usd, None, "this provider has never named a price");
    assert_eq!(run.num_turns, Some(3));
    assert_eq!(run.input_tokens, Some(14_203));
    assert_eq!(
        run.pr_url.as_deref(),
        Some("https://github.com/abtion/rimaia/pull/61"),
        "the pull-request grammar is forge-shaped, not provider-shaped",
    );
}

#[tokio::test]
async fn the_second_providers_child_is_never_handed_a_flag_it_does_not_implement() {
    // Catches a Claude flag entering through `probe_cli`, the doctor, the
    // strategy resolver, or anything else that reaches for a vector. The
    // stand-in scans its whole argv rather than only `$1`, which is what makes a
    // flag arriving in the middle of one visible at all.
    let fixture = Fixture::new().await;
    let cli = FakeCli::new();
    cli.replays_path(
        &fixture.task_id,
        &path_for(ProviderId::Ledger, "finished"),
        0,
    );

    tokio::time::timeout(TEST_TIMEOUT, fixture.run(&cli))
        .await
        .expect("the run finishes")
        .expect("the run succeeds");

    cli.assert_nothing_fell_through();

    let argv = cli.argv(&fixture.task_id, 1);
    assert_eq!(argv.first().map(String::as_str), Some("run"));
    for claude_only in ["-p", "--permission-mode", "--max-turns", "--mcp-config"] {
        assert!(
            !argv.iter().any(|arg| arg == claude_only),
            "`{claude_only}` reached a provider that has never heard of it: {argv:?}",
        );
    }
}

#[tokio::test]
async fn a_self_minting_provider_still_writes_a_non_null_session_id() {
    // ADR-0026 point 5. `runs.session_id` is `NOT NULL` and
    // `scheduler::attempts` counts a retry budget against it, so it has to be
    // Rimaia's conversation id — **not** the `lgr_…` the stream announces, which
    // is never persisted.
    let fixture = Fixture::new().await;
    let cli = FakeCli::new();
    cli.replays_path(
        &fixture.task_id,
        &path_for(ProviderId::Ledger, "finished"),
        0,
    );

    let run = tokio::time::timeout(TEST_TIMEOUT, fixture.run(&cli))
        .await
        .expect("the run finishes")
        .expect("the run succeeds");

    assert!(!run.session_id.is_empty());
    assert!(
        !run.session_id.starts_with("lgr_"),
        "the provider's own id must not reach the column: {}",
        run.session_id,
    );
    // And it was never handed to the child either.
    assert!(!cli
        .argv(&fixture.task_id, 1)
        .iter()
        .any(|arg| arg == &run.session_id));
}

#[tokio::test]
async fn two_attempts_of_one_task_share_a_conversation_and_spend_one_budget() {
    // The retry-budget boundary, under a provider that never saw a session id.
    // `attempts::history` groups by `runs.session_id`, so a second attempt that
    // minted a fresh one would silently reset the budget — which is a task that
    // retries forever rather than landing in `failed` with a reason.
    let fixture = Fixture::new().await;
    let cli = FakeCli::new();
    cli.replays_path(
        &fixture.task_id,
        &path_for(ProviderId::Ledger, "stopped"),
        LEDGER_KILLED,
    );

    let first = tokio::time::timeout(TEST_TIMEOUT, fixture.run(&cli))
        .await
        .expect("the run finishes")
        .expect("the run is recorded");

    cli.replays_path(
        &fixture.task_id,
        &path_for(ProviderId::Ledger, "continued"),
        0,
    );
    let second = tokio::time::timeout(
        TEST_TIMEOUT,
        fixture.run_request(
            &cli,
            RunRequest::resuming(&fixture.task_id, &first.session_id),
        ),
    )
    .await
    .expect("the run finishes")
    .expect("the resumed run is recorded");

    assert_eq!(
        second.session_id, first.session_id,
        "a resume continues the conversation, so it spends the same budget",
    );
    assert_eq!(second.attempt, 2);
    assert_eq!(second.exit_class, Some(ExitClass::Success));
    // No id crosses to the child; `--continue` is how this provider is told.
    assert!(cli
        .argv(&fixture.task_id, 2)
        .iter()
        .any(|arg| arg == "--continue"));
}

#[tokio::test]
async fn a_requeued_task_gets_a_fresh_budget_under_a_self_minting_provider() {
    // The other half: a run that is *not* a resume opens a new conversation, so
    // pressing "Run now" on a task that gave up last night starts its budget
    // again. Same rule as Claude Code, reached without a session flag.
    let fixture = Fixture::new().await;
    let cli = FakeCli::new();
    cli.replays_path(
        &fixture.task_id,
        &path_for(ProviderId::Ledger, "stopped"),
        LEDGER_KILLED,
    );

    let first = tokio::time::timeout(TEST_TIMEOUT, fixture.run(&cli))
        .await
        .expect("the run finishes")
        .expect("the run is recorded");
    let second = tokio::time::timeout(TEST_TIMEOUT, fixture.run(&cli))
        .await
        .expect("the run finishes")
        .expect("the run is recorded");

    assert_ne!(second.session_id, first.session_id);
}

#[tokio::test]
async fn a_provider_that_cannot_continue_is_sent_the_composed_prompt_not_the_continuation() {
    // ADR-0026 point 6, where it can actually be observed: on the child's stdin.
    // A one-line continuation delivered into a *fresh* session produces an agent
    // with no plan, no context and an empty diff — a seam bug that would read as
    // a bad model rather than as a bug.
    let fixture = Fixture::new().await;
    let cli = FakeCli::new();
    cli.replays_path(
        &fixture.task_id,
        &path_for(ProviderId::Ledger, "stopped"),
        LEDGER_KILLED,
    );

    let first = tokio::time::timeout(
        TEST_TIMEOUT,
        fixture.run_with(
            &cli,
            Arc::new(LedgerWithoutResume),
            RunRequest::manual(&fixture.task_id),
        ),
    )
    .await
    .expect("the run finishes")
    .expect("the run is recorded");

    cli.replays_path(
        &fixture.task_id,
        &path_for(ProviderId::Ledger, "finished"),
        0,
    );
    let second = tokio::time::timeout(
        TEST_TIMEOUT,
        fixture.run_with(
            &cli,
            Arc::new(LedgerWithoutResume),
            RunRequest::resuming(&fixture.task_id, &first.session_id),
        ),
    )
    .await
    .expect("the run finishes")
    .expect("the resumed run is recorded");

    assert_eq!(
        cli.stdin(&fixture.task_id, 2),
        second.prompt,
        "what was stored is what was sent",
    );
    assert_eq!(
        cli.stdin(&fixture.task_id, 2),
        cli.stdin(&fixture.task_id, 1),
        "a provider that cannot continue is sent the whole composed prompt again",
    );
    assert!(!cli
        .argv(&fixture.task_id, 2)
        .iter()
        .any(|arg| arg == "--continue"));
}

#[tokio::test]
async fn a_manual_attempt_records_the_mitigations_its_provider_could_not_enforce() {
    // The `AskBeforeCommands` arm reaching the run's own record rather than a log
    // line nobody looked for. A morning reviewer can read "this attempt ran
    // without ADR-0012's mitigations" beside the transcript it applies to.
    let fixture = Fixture::new().await;
    let cli = FakeCli::new();
    cli.replays_path(
        &fixture.task_id,
        &path_for(ProviderId::Ledger, "finished"),
        0,
    );

    let run = tokio::time::timeout(TEST_TIMEOUT, fixture.run(&cli))
        .await
        .expect("the run finishes")
        .expect("the run succeeds");

    let diagnostics =
        std::fs::read_to_string(PathBuf::from(&run.log_path).with_extension("stderr.log"))
            .expect("a run that could not be fully protected says so beside its transcript");

    assert!(
        diagnostics.contains("rewriting history on a remote"),
        "the operation must be named: {diagnostics}",
    );
    assert!(
        diagnostics.contains("calling Rimaia's own MCP tools"),
        "every unenforced operation, not just the first: {diagnostics}",
    );
}

#[tokio::test]
async fn a_second_provider_run_that_is_cancelled_still_records_the_ending_its_stream_reported() {
    // Catches SIGTERM handling written against one provider's exit code. This
    // child exits **137** rather than 143, emits its ending on the way out, and
    // the run is still `Cancelled` — because cancellation outranks whatever the
    // stream says about how it died (ADR-0011's rule 1).
    //
    // No `sleep`: the cancel fires on the first tail snapshot, which is the run
    // telling us it is in flight (seam-contract D14).
    let fixture = Fixture::new().await;
    let cli = FakeCli::new();
    let gate = cli.resists_sigterm(
        &fixture.task_id,
        &path_for(ProviderId::Ledger, "stopped"),
        3,
        LEDGER_KILLED,
    );
    let cancel = CancelSignal::new();
    let tail = fixture.harness.context.subscribe_tail();

    let (run, ()) = tokio::time::timeout(TEST_TIMEOUT, async {
        tokio::join!(
            fixture.run_request(
                &cli,
                RunRequest {
                    cancel: cancel.clone(),
                    ..RunRequest::manual(&fixture.task_id)
                },
            ),
            async {
                open_the_gate_once_the_run_is_live(tail, cancel.clone(), &gate).await;
            },
        )
    })
    .await
    .expect("the run finishes");
    let run = run.expect("a cancelled run is still recorded");

    assert_eq!(run.exit_class, Some(ExitClass::Cancelled));
    assert_eq!(
        fixture.detail().await.task.run_state,
        RunState::Failed,
        "ADR-0010: cancel-one on a running task lands in failed with the reason on the run",
    );
    // The ending the stream reported is on disk, which is what the grace period
    // buys — and it arrived after the signal, from a child that exits 137.
    let transcript = std::fs::read_to_string(&run.log_path).expect("a written transcript");
    assert!(
        transcript.contains("\"why\":\"stopped\""),
        "the ending its own stream reported: {transcript}",
    );
}

#[tokio::test]
async fn a_planner_falls_back_when_a_provider_cannot_inject_a_handle() {
    // Seam-contract D16.7's route, reached by a capability rather than by a busy
    // port: a planner that cannot be handed Rimaia's scoped handle has no way to
    // answer, so it is never spawned. The card carries a `failed` envelope and the
    // task runs on the `default` chain — ADR-0016's "failure is not fatal".
    let fixture = Fixture::new().await;
    let cli = FakeCli::new();
    cli.replays_path(
        &fixture.task_id,
        &path_for(ProviderId::Ledger, "finished"),
        0,
    );
    fixture.set_planned().await;

    let run = tokio::time::timeout(TEST_TIMEOUT, fixture.run(&cli))
        .await
        .expect("the run finishes")
        .expect("the implementation run still happens");

    assert_eq!(run.exit_class, Some(ExitClass::Success));
    assert_eq!(
        cli.attempts(&fixture.task_id),
        1,
        "exactly one process: the implementation run, and no planner",
    );
    assert!(
        !cli.spawns()
            .iter()
            .any(|line| line.contains(STRATEGY_TRANSCRIPT_PREFIX)),
        "no planner transcript was opened: {:?}",
        cli.spawns(),
    );

    let plan = StrategyPlan::from_stored(fixture.detail().await.task.strategy_plan.as_deref())
        .expect("a failure envelope on the card");
    assert_eq!(plan.status, StrategyPlanStatus::Failed);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Opens a gated stand-in once the run says it is in flight, having cancelled it
/// first — so the child is signalled, *then* allowed to emit its ending.
async fn open_the_gate_once_the_run_is_live(
    mut tail: Receiver<RunTail>,
    cancel: CancelSignal,
    gate: &std::path::Path,
) {
    let _ = tokio::time::timeout(TEST_TIMEOUT, tail.recv()).await;
    cancel.cancel();
    rimaia_core::testing::open_gate(gate);
}

struct Fixture {
    harness: TestContext,
    /// Held for their `Drop`; the paths below point inside them.
    _repository: TempRepo,
    _data: TempDir,
    paths: AppPaths,
    task_id: String,
}

impl Fixture {
    async fn new() -> Self {
        let harness = TestContext::new().await;
        let repository = TempRepo::init();
        let data = tempfile::Builder::new()
            .prefix("rimaia-data-")
            .tempdir()
            .expect("temp dir for the app data directory");
        let paths = AppPaths::new(data.path());
        paths.create_all().expect("the app data directories");

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
        .expect("register the test repository");
        repo::set_allow_unattended_runs(&harness.context, &registered.id, true)
            .await
            .expect("ADR-0012's per-repository opt-in");

        let task = tasks::create_task(
            &harness.context,
            NewTask {
                repository_id: registered.id.clone(),
                title: "Add truncate_slug".to_string(),
                plan: Some("1. Add the function\n2. Test it".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a ready task");

        Self {
            harness,
            _repository: repository,
            _data: data,
            paths,
            task_id: task.id,
        }
    }

    /// A config driving the second provider's stand-in.
    ///
    /// `RunnerConfig::default()` underneath, so the only thing this test changes
    /// about how a run happens is which provider it is — which is the whole
    /// claim under test.
    fn config(&self, cli: &FakeCli, provider: Arc<dyn AgentProvider>) -> RunnerConfig {
        RunnerConfig {
            provider,
            program: cli.program_for(ProviderId::Ledger),
            ..RunnerConfig::default()
        }
    }

    async fn run(&self, cli: &FakeCli) -> rimaia_core::Result<rimaia_core::db::Run> {
        self.run_request(cli, RunRequest::manual(&self.task_id))
            .await
    }

    async fn run_request(
        &self,
        cli: &FakeCli,
        request: RunRequest,
    ) -> rimaia_core::Result<rimaia_core::db::Run> {
        self.run_with(cli, Arc::new(Ledger), request).await
    }

    async fn run_with(
        &self,
        cli: &FakeCli,
        provider: Arc<dyn AgentProvider>,
        request: RunRequest,
    ) -> rimaia_core::Result<rimaia_core::db::Run> {
        run_task(
            &self.harness.context,
            &self.paths,
            &self.config(cli, provider),
            request,
        )
        .await
    }

    /// Puts the task into ADR-0016's `planned` mode, so `run_task` would resolve
    /// a strategy before spawning the implementation run.
    async fn set_planned(&self) {
        catalogue::catalogue(&self.harness.context.pool, &ClaudeProvider)
            .await
            .expect("the seeded catalogue");
        tasks::update_task(
            &self.harness.context,
            &self.task_id,
            TaskPatch {
                strategy_mode: Some(StrategyMode::Planned),
                ..TaskPatch::default()
            },
        )
        .await
        .expect("put the task into planned mode");
    }

    async fn detail(&self) -> tasks::TaskDetail {
        tasks::get_task(&self.harness.context, &self.task_id)
            .await
            .expect("the task is readable")
    }
}
