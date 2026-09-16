//! What a second provider proves, without spawning one (ADR-0026).
//!
//! # Why this file is not `#![cfg(unix)]`
//!
//! Everything here is argv, negotiation, parsing and classification — pure
//! values, no pipes. CI runs `cargo test -p rimaia-core` on Windows too, and
//! **these are the tests that catch the leaks**: a Claude flag hardcoded in
//! shared code, a refusal that does not refuse, a relative window resolved
//! against the wrong instant. The real children live next door in
//! `provider_process.rs`, which is `#![cfg(unix)]` for the reason
//! `runner_process.rs` already gives.
//!
//! # The provider it uses does not exist
//!
//! `testing::provider::Ledger` is fictional on purpose — see its own header, and
//! `tests/fixtures/ledger/README.md`. Nothing here is evidence about any agent
//! CLI except Claude Code; all of it is evidence about **Rimaia's seam**.
//!
//! # Classification is driven from files, never from hand-built payloads
//!
//! CLAUDE.md's rule is that the CLI is faked by replaying recorded streams, and
//! the one place a second provider is genuinely uncomfortable is classification —
//! where a hand-built event could assert whatever the author expected. So every
//! classification test below drives its input from a fixture on disk through a
//! real [`EventStream`], exactly as `runner_outcome.rs` does for the recordings.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use chrono::Duration;
use pretty_assertions::assert_eq;
use rimaia_core::db::settings::RunEnvironment;
use rimaia_core::db::{BoardColumn, ExitClass, RunState};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::events::{EventStream, RunEvent, UsageState};
use rimaia_core::runner::outcome::{classify, RunOutcome, Termination};
use rimaia_core::runner::process::inherited_identity_vars;
use rimaia_core::runner::provider::{
    negotiate, AgentProvider, ClaudeProvider, ForbiddenOperation, PermissionMode, PromptStyle,
    ProviderId, RimaiaHandle, RunIntent, SessionIntent, SpawnPlan,
};
use rimaia_core::runner::{run_task, RunRequest, RunTrigger, RunnerConfig};
use rimaia_core::scheduler::retry::{self, RetryDecision, RetryKind, USAGE_LIMIT_FALLBACK_POLL};
use rimaia_core::scheduler::AttemptHistory;
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::fixtures::lines_for;
use rimaia_core::testing::provider::{Ledger, LEDGER_HOME, LEDGER_INHERIT, TOOLS_FILE};
use rimaia_core::testing::{TempRepo, TestContext};
use rimaia_core::AppPaths;
use tempfile::TempDir;

const CONVERSATION: &str = "7f0c9f2e-4c27-4a8a-8f1c-2d1e4a6b8c31";

// ---------------------------------------------------------------------------
// The argument vectors
// ---------------------------------------------------------------------------

/// One maximally-populated intent, so neither provider is flattered by a field
/// the other happens not to use.
fn intent<'a>(workspace: &'a Path, home: &'a Path) -> RunIntent<'a> {
    RunIntent {
        session: SessionIntent::Open {
            conversation: CONVERSATION,
            home,
        },
        permission_mode: PermissionMode::BypassPermissions,
        run_environment: RunEnvironment::StrictLocal,
        system_append: "You are running unattended, started by Rimaia.".to_string(),
        prompt: "do the thing",
        model: Some("a-large-model".to_string()),
        effort: Some("high".to_string()),
        max_turns: Some(40),
        workspace,
        forbidden: vec![
            ForbiddenOperation::RemoteHistoryRewrite,
            ForbiddenOperation::RimaiaToolSurface,
        ],
        required_tools: vec!["set_task_strategy"],
        rimaia_handle: Some(RimaiaHandle {
            url: "http://127.0.0.1:4517/mcp/run/tok".to_string(),
            server: "rimaia",
        }),
    }
}

fn plan_of(provider: &dyn AgentProvider, intent: &RunIntent<'_>, scratch: &Path) -> SpawnPlan {
    provider
        .plan_spawn(intent, scratch)
        .expect("a plan this provider can express")
}

#[test]
fn the_two_providers_argv_share_no_token_either_of_them_invented() {
    // **The highest-value test in this file.** Any Claude flag hardcoded in
    // shared code shows up here as a token in the wrong vector, whatever module
    // it leaked from.
    //
    // The two vectors are not disjoint and must not be asserted so: an intent
    // carries values — a model name, an effort, a turn budget — and both
    // providers pass those through verbatim, because they are the operator's
    // data and not either provider's vocabulary. So the assertion is that the
    // intersection is *exactly* that data, which is the strictest form that is
    // also true.
    let dir = TempDir::new().expect("temp dir");
    let scratch = dir.path().join("scratch");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&scratch).expect("a scratch directory");
    let intent = intent(dir.path(), &home);

    let claude: BTreeSet<String> = plan_of(&ClaudeProvider, &intent, &scratch)
        .args
        .into_iter()
        .collect();
    let ledger: BTreeSet<String> = plan_of(&Ledger, &intent, &scratch)
        .args
        .into_iter()
        .collect();

    let shared: Vec<&String> = claude.intersection(&ledger).collect();
    assert_eq!(
        shared,
        vec!["40", "a-large-model", "high"],
        "the only tokens two providers may share are the intent's own values",
    );

    // And the flags themselves, named, because an intersection of zero is also
    // what a vector that was never built would give.
    assert!(claude.contains("--permission-mode"));
    assert!(claude.contains("--max-turns"));
    assert!(ledger.contains("--trust"));
    assert!(ledger.contains("--step-budget"));
}

#[test]
fn the_scoped_rimaia_handle_reaches_a_provider_that_has_no_flag_for_it() {
    // ADR-0026 point 2. A trait whose spawn method returned `Vec<String>` could
    // not express this at all, which is the whole reason it returns a plan.
    let dir = TempDir::new().expect("temp dir");
    let scratch = dir.path().join("scratch");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&scratch).expect("a scratch directory");

    let plan = plan_of(&Ledger, &intent(dir.path(), &home), &scratch);

    assert!(
        !plan.args.iter().any(|arg| arg == "--mcp-config"),
        "this provider has no flag for a handle: {:?}",
        plan.args,
    );
    assert_eq!(
        plan.env_set,
        vec![(LEDGER_HOME.to_string(), home.display().to_string())],
        "the handle arrives as a configuration home",
    );
    let tools = std::fs::read_to_string(home.join(TOOLS_FILE)).expect("a written tools file");
    assert!(
        tools.contains("http://127.0.0.1:4517/mcp/run/tok"),
        "{tools}"
    );
    // Rimaia's own name for the tool, spelled the way *this* provider spells it
    // — which is the point of `required_tools` holding neither spelling.
    assert!(tools.contains("\"set_task_strategy\""), "{tools}");

    // Without a handle the home is not taken over, and this provider says so.
    let no_handle = RunIntent {
        rimaia_handle: None,
        run_environment: RunEnvironment::Inherit,
        ..intent(dir.path(), &home)
    };
    let plan = plan_of(&Ledger, &no_handle, &scratch);
    assert!(plan.env_set.iter().any(|(name, _)| name == LEDGER_INHERIT));
}

#[test]
fn a_self_minting_provider_is_never_handed_a_session_id() {
    // ADR-0026 point 5: Rimaia's conversation id is Rimaia's, and a provider that
    // mints its own must not be told to adopt it.
    let dir = TempDir::new().expect("temp dir");
    let scratch = dir.path().join("scratch");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&scratch).expect("a scratch directory");

    let opening = plan_of(&Ledger, &intent(dir.path(), &home), &scratch);
    let continuing = plan_of(
        &Ledger,
        &RunIntent {
            session: SessionIntent::Continue {
                conversation: CONVERSATION,
                home: &home,
                last_announced: Some("lgr_9d44be03"),
            },
            ..intent(dir.path(), &home)
        },
        &scratch,
    );

    for args in [&opening.args, &continuing.args] {
        assert!(
            !args
                .iter()
                .any(|arg| arg == CONVERSATION || arg == "lgr_9d44be03"),
            "no id reaches a provider that mints its own: {args:?}",
        );
    }
    assert!(continuing.args.iter().any(|arg| arg == "--continue"));
    assert!(!opening.args.iter().any(|arg| arg == "--continue"));
}

#[test]
fn the_default_provider_is_still_claude_code() {
    // Catches the refactor shipping the test provider, which is a live risk now
    // that a second implementation lives in the same crate.
    let config = RunnerConfig::default();

    assert_eq!(config.provider.id(), ProviderId::ClaudeCode);
    assert_eq!(config.program, Path::new("claude"));
}

// ---------------------------------------------------------------------------
// Negotiation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_provider_that_cannot_deny_a_tool_refuses_an_unattended_run() {
    // **The test that catches ADR-0012 evaporating.** The mitigations are part of
    // why `bypassPermissions` was granted at all, so a provider that cannot
    // express them does not get an unattended run — and the refusal lands before
    // anything is written, so there is nothing to clean up afterwards.
    let fixture = Fixture::new().await;

    let error = run_task(
        &fixture.harness.context,
        &fixture.paths,
        &fixture.ledger_config(),
        RunRequest {
            trigger: RunTrigger::Queued,
            ..RunRequest::manual(&fixture.task_id)
        },
    )
    .await
    .expect_err("an unattended run on a provider with no blocklist");

    assert!(
        error.to_string().contains("rewriting history on a remote"),
        "the refusal must name the operation: {error}",
    );

    // No worktree, no claim, no `runs` row — task 008's rule that a missing
    // prerequisite must not leave a half-open run, applied to a missing
    // capability.
    let detail = fixture.detail().await;
    assert_eq!(
        detail.task.run_state,
        RunState::Idle,
        "the task was claimed"
    );
    assert_eq!(detail.last_run, None, "a `runs` row was opened");
    assert_eq!(detail.task.branch, None, "a worktree was prepared");
}

#[test]
fn the_same_provider_records_what_it_could_not_enforce_when_a_human_started_the_run() {
    // The `AskBeforeCommands` arm. Somebody is present, so the run proceeds —
    // and what it could not enforce is a value on the plan rather than a log line
    // nobody looked for. `provider_process.rs` asserts the other half: that it
    // reaches the run's own record on disk.
    let dir = TempDir::new().expect("temp dir");
    let home = dir.path().join("home");
    let manual = RunIntent {
        permission_mode: PermissionMode::AcceptEdits,
        rimaia_handle: None,
        run_environment: RunEnvironment::Inherit,
        ..intent(dir.path(), &home)
    };

    let plan = negotiate(Ledger.capabilities(), &manual).expect("a manual run proceeds");

    assert_eq!(
        plan.unenforced,
        vec![
            ForbiddenOperation::RemoteHistoryRewrite,
            ForbiddenOperation::RimaiaToolSurface,
        ],
    );
    assert!(
        !plan.verify_posture,
        "this provider states no posture, so there is nothing to verify",
    );
}

#[test]
fn a_provider_that_cannot_continue_is_planned_for_the_composed_prompt() {
    // ADR-0026 point 6, as the decision. A continuation delivered into a fresh
    // session produces an agent with no plan, no context and an empty diff — a
    // seam bug that would read as a bad model. `provider_process.rs` asserts the
    // prompt that actually arrives on stdin.
    use rimaia_core::testing::provider::LedgerWithoutResume;

    let dir = TempDir::new().expect("temp dir");
    let home = dir.path().join("home");
    let resuming = RunIntent {
        session: SessionIntent::Continue {
            conversation: CONVERSATION,
            home: &home,
            last_announced: None,
        },
        permission_mode: PermissionMode::AcceptEdits,
        rimaia_handle: None,
        run_environment: RunEnvironment::Inherit,
        ..intent(dir.path(), &home)
    };

    assert_eq!(
        negotiate(Ledger.capabilities(), &resuming)
            .expect("a provider that can continue")
            .prompt_style,
        PromptStyle::Continuation,
    );
    assert_eq!(
        negotiate(LedgerWithoutResume.capabilities(), &resuming)
            .expect("a provider that cannot continue still runs")
            .prompt_style,
        PromptStyle::Composed,
    );
}

#[test]
fn every_providers_identity_variables_are_stripped_whichever_one_spawned() {
    // Seam-contract D27.5: the **union**, not the active provider's. Rimaia is
    // developed from inside a Claude Code session *and* may be spawning something
    // else, and the converse arrives the moment anyone drives Rimaia from another
    // agent. `LGR_` is what fails a fix that assumed one prefix.
    let parent = [
        "CLAUDECODE",
        "CLAUDE_CODE_SESSION_ID",
        "LEDGER_THREAD",
        "LGR_PARENT",
        "PATH",
        "HOME",
        "MY_LEDGER_NOTES",
    ];

    assert_eq!(
        inherited_identity_vars(parent),
        vec![
            "CLAUDECODE".to_string(),
            "CLAUDE_CODE_SESSION_ID".to_string(),
            "LEDGER_THREAD".to_string(),
            "LGR_PARENT".to_string(),
        ],
    );
}

// ---------------------------------------------------------------------------
// The stream, and what an ending is worth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_relative_reset_window_is_resolved_against_the_instant_it_was_observed() {
    // ADR-0026 point 7. The window says "3600 seconds from now" at 02:00 and the
    // run then takes another fifteen fake minutes to die. Resolving at the end
    // would say 03:15 — wrong by exactly how long the run took, in the direction
    // that wastes a night.
    let replay = Replay::of("window-closed").await;

    assert_eq!(replay.class(), ExitClass::UsageLimit);
    assert_eq!(
        replay.outcome().usage_limit_resets_at,
        Some(rimaia_core::testing::test_epoch() + Duration::hours(1)),
        "the window was read at 02:00 and reopens an hour after that",
    );
}

#[tokio::test]
async fn a_window_that_reports_only_a_percentage_is_not_a_wall() {
    // ADR-0026 point 8. A provider that reports usage on every turn must not have
    // "99% used" read as a limit: that raises ADR-0011's *global* pause and stops
    // the whole queue on a healthy run.
    let replay = Replay::of("finished").await;

    assert_eq!(replay.class(), ExitClass::Success);
    assert_eq!(replay.outcome().usage_limit_resets_at, None);
    assert_eq!(
        replay.usage_state(),
        Some(UsageState::Allowed),
        "an open window is reported, and reporting it is not hitting it",
    );
}

#[tokio::test]
async fn an_exhausted_window_is_not_cleared_by_a_later_heartbeat() {
    // The latch. `window-closed-no-reopen.jsonl` reports the wall mid-run and
    // then an ending carrying the window **open** again; without latching, a
    // provider that reports every turn would retract its own refusal, the class
    // would be `transient`, and the task would back off 1m/5m/15m into a window
    // that is still closed — abandoned by morning.
    let replay = Replay::of("window-closed-no-reopen").await;

    assert_eq!(
        replay.usage_state(),
        Some(UsageState::Exhausted),
        "a heartbeat arriving after a refusal must not clear the refusal",
    );
    assert_eq!(replay.class(), ExitClass::UsageLimit);
}

#[tokio::test]
async fn a_usage_limit_without_reset_time_falls_back_to_fixed_poll_for_either_provider() {
    // The repo's own canonical test name, extended across both providers: the
    // fallback is ADR-0011's policy and belongs to neither wire format.
    for (name, provider) in [
        ("usage-limit-no-reset", ProviderId::ClaudeCode),
        ("window-closed-no-reopen", ProviderId::Ledger),
    ] {
        let replay = Replay::for_provider(provider, name).await;
        assert_eq!(replay.class(), ExitClass::UsageLimit, "{name}");
        assert_eq!(
            replay.outcome().usage_limit_resets_at,
            None,
            "{name}: a window that names no reopen names none"
        );

        let history = AttemptHistory {
            exit_class: ExitClass::UsageLimit,
            session_id: CONVERSATION.to_string(),
            attempts_in_session: 1,
            transient_attempts: 0,
            interrupted_attempts: 0,
            usage_limit_resets_at: None,
        };
        let now = rimaia_core::testing::test_epoch();

        let RetryDecision::ResumeAt { at, kind } = retry::decide(&history, now, "run-1", None)
        else {
            panic!("{name}: a usage limit is always retried");
        };
        assert_eq!(kind, RetryKind::UsageLimit);
        assert!(
            at >= now + Duration::seconds(USAGE_LIMIT_FALLBACK_POLL),
            "{name}: the fixed poll, plus jitter",
        );
    }
}

#[tokio::test]
async fn an_unmodelled_event_from_either_provider_is_kept_whole_and_never_fatal() {
    // ADR-0004's tolerance is shared machinery, not Claude's: an event a provider
    // has not been taught still arrives, still carries its whole document, and
    // still occupies its place in the stream.
    for (provider, name, unfamiliar) in [
        (
            ProviderId::ClaudeCode,
            "unknown-event-type",
            "telemetry_ping",
        ),
        (ProviderId::Ledger, "unknown-kind", "telemetry.beat"),
    ] {
        let replay = Replay::for_provider(provider, name).await;

        let kept: Vec<&RunEvent> = replay
            .events
            .iter()
            .filter(|event| event.event_type() == unfamiliar)
            .collect();
        assert_eq!(kept.len(), 1, "{name}: `{unfamiliar}` must arrive");
        let RunEvent::Other(other) = kept[0] else {
            panic!("{name}: an unfamiliar event must stay opaque");
        };
        assert!(!other.raw.is_null(), "{name}: it keeps its whole document");
        assert_eq!(replay.malformed_lines, 0, "{name}");
    }
}

#[tokio::test]
async fn a_stream_that_never_reaches_a_terminal_event_is_transient_for_either_provider() {
    // The other half of the same rule. "The stream stopped" is not evidence of
    // anything, so it is ADR-0011's retryable default rather than a hard failure.
    for (provider, name) in [
        (ProviderId::ClaudeCode, "truncated-stream"),
        (ProviderId::Ledger, "torn"),
    ] {
        let replay = Replay::for_provider(provider, name).await;

        assert_eq!(replay.class(), ExitClass::Transient, "{name}");
        assert_eq!(replay.malformed_lines, 1, "{name}: only the torn last line");
    }
}

// ---------------------------------------------------------------------------
// Replaying a recorded stream through the real machinery
// ---------------------------------------------------------------------------

/// One stream, replayed through a real [`EventStream`] with a real transcript on
/// disk and the provider that speaks it.
///
/// **Never a hand-built payload.** `runner_outcome.rs` holds that line for the
/// recordings and this holds it for the second corpus — the moment a
/// classification test starts constructing an event by hand, the evidence that
/// the classifier reads what a provider actually emits is gone.
struct Replay {
    _root: TempDir,
    stream: EventStream,
    events: Vec<RunEvent>,
    malformed_lines: u64,
}

impl Replay {
    async fn of(name: &str) -> Self {
        Self::for_provider(ProviderId::Ledger, name).await
    }

    async fn for_provider(provider: ProviderId, name: &str) -> Self {
        Self::of_lines_with(provider, lines_for(provider, name).collect()).await
    }

    async fn of_lines_with(provider: ProviderId, lines: Vec<String>) -> Self {
        let harness = TestContext::new().await;
        let root = TempDir::new().expect("temp dir for the run directory");
        let paths = AppPaths::new(root.path());

        let mut stream = EventStream::create(&harness.context, &paths, "task-1", "run-1")
            .expect("the run directory is creatable")
            .driven_by(match provider {
                ProviderId::ClaudeCode => Arc::new(ClaudeProvider) as Arc<dyn AgentProvider>,
                ProviderId::Ledger => Arc::new(Ledger) as Arc<dyn AgentProvider>,
            });

        let mut events = Vec::new();
        for line in lines {
            // The fifteen fake minutes between the window and the ending, which
            // is what makes "resolved against the instant it was observed"
            // falsifiable rather than a tautology. No `sleep` anywhere.
            if line.contains("\"kind\":\"finished\"") || line.contains("\"type\":\"result\"") {
                harness.clock.advance(Duration::minutes(15));
            }
            if let Some(event) = stream
                .observe(&line)
                .expect("the transcript stays writable")
            {
                events.push(event);
            }
        }
        let malformed_lines = stream.malformed_lines();

        Self {
            _root: root,
            stream,
            events,
            malformed_lines,
        }
    }

    fn termination(&self) -> Termination<'_> {
        Termination::from_stream(&self.stream)
    }

    fn class(&self) -> ExitClass {
        classify(&self.termination())
    }

    fn outcome(&self) -> RunOutcome {
        RunOutcome::of(&self.termination(), None)
    }

    fn usage_state(&self) -> Option<UsageState> {
        self.stream.usage().map(|usage| usage.window.state)
    }
}

// ---------------------------------------------------------------------------
// A task to refuse
// ---------------------------------------------------------------------------

struct Fixture {
    harness: TestContext,
    paths: AppPaths,
    task_id: String,
    _data: TempDir,
    _repo: TempRepo,
}

impl Fixture {
    async fn new() -> Self {
        let harness = TestContext::new().await;
        let repository = TempRepo::init();
        let data = TempDir::new().expect("temp dir for the app data directory");
        let paths = AppPaths::new(data.path());
        paths.create_all().expect("the data directory is creatable");

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
            paths,
            task_id: task.id,
            _data: data,
            _repo: repository,
        }
    }

    /// A config pointed at a program that does not exist.
    ///
    /// Deliberately: the refusal has to land **before** the prerequisite probe,
    /// so a test that reaches the probe fails here rather than somewhere more
    /// forgiving.
    fn ledger_config(&self) -> RunnerConfig {
        RunnerConfig {
            provider: Arc::new(Ledger),
            program: self.paths.data_dir().join("no-such-ledger"),
            ..RunnerConfig::default()
        }
    }

    async fn detail(&self) -> tasks::TaskDetail {
        tasks::get_task(&self.harness.context, &self.task_id)
            .await
            .expect("the task is readable")
    }
}
