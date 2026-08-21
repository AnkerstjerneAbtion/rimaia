//! Spawning, the environment, cancellation, and the whole run loop — against a
//! stand-in binary, never the real one (ADR-0004, ADR-0012, ADR-0015, task 008).
//!
//! # Nothing here starts a real `claude`
//!
//! Two independent reasons, and either alone would be enough. It costs the
//! operator real money on every run. And `cargo test` is routinely invoked from
//! inside a Claude Code session, so a real child would inherit exactly the
//! `CLAUDE_*` variables this task exists to strip — a test that "passed" while
//! doing the thing it was written to forbid.
//!
//! So the CLI is a shell script in a `tempfile::TempDir` that replays a recorded
//! stream from `tests/fixtures/cli/` and records what it was handed. That gives
//! real process semantics — spawn, argv, environment, cwd, stdin, pipes, exit
//! status, signals, process groups — with no network and no tokens. Everything
//! *above* the process boundary is asserted against the recordings themselves.
//!
//! # Why these tests ask for `bypassPermissions`
//!
//! Every recording in the corpus was captured with `--permission-mode
//! bypassPermissions`, and its `init` event echoes that back. The runner refuses
//! to continue when the applied mode is not the requested one, so a test that
//! replays a recording has to ask for the mode the recording answers with. That
//! is not a workaround — it is the verification being live, and
//! [`a_cli_that_applied_a_permission_mode_nobody_asked_for_stops_the_run`] turns
//! the same fact around and uses it as the mismatch.
//!
//! # No `sleep`, and no unbounded wait
//!
//! Nothing here sleeps to let a process "get going". A test that needs a run to
//! be *in flight* subscribes to the tail channel (seam-contract D14) and acts on
//! the first snapshot, which is the run telling it so. The `timeout` wrappers
//! are failure bounds, not waits: on a passing run they cost nothing, and on a
//! broken one they turn a hung CI job into a failed assertion.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use pretty_assertions::assert_eq;
use rimaia_core::db::settings::{self, RunEnvironment};
use rimaia_core::db::{BoardColumn, ExitClass, Run, RunState, RunStatus, Task};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::events::{parse_line, stderr_path, transcript_path, RunEvent, RunTail};
use rimaia_core::runner::process::{
    disallowed_tools, inherited_identity_vars, is_process_identity, verify_permission_mode,
    DEFAULT_DISALLOWED_TOOLS, DISALLOWED_TOOLS,
};
use rimaia_core::runner::prompt::compose_prompt;
use rimaia_core::runner::{
    run_task, CancelSignal, Invocation, PermissionMode, RunRequest, RunTrigger, RunnerConfig,
};
use rimaia_core::tasks::{self, NewTask, Patch, TaskPatch};
use rimaia_core::testing::fixtures::{fixture_lines, fixture_path};
use rimaia_core::testing::{TempRepo, TestContext};
use rimaia_core::{AppPaths, ErrorCode};
use tempfile::TempDir;
use tokio::sync::broadcast::Receiver;

/// A ceiling on any single process test. Long enough that a slow machine never
/// trips it, short enough that a supervision bug fails rather than hangs.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

const SESSION_ID: &str = "c6529619-d49b-479b-b4ce-a97cad085fda";
const TASK_TITLE: &str = "Add truncate_slug";

// ---------------------------------------------------------------------------
// The argument vector
// ---------------------------------------------------------------------------
//
// Exact vectors, never substrings. These are the flags that decide what an
// unattended agent is permitted to do and whose configuration it runs under —
// the same class of contract as prompt composition, and the one place a silent
// change would be least visible.

/// The canonical invocation the assertions below vary one field of at a time.
fn invocation() -> Invocation {
    Invocation {
        session_id: SESSION_ID.to_string(),
        resume: false,
        permission_mode: PermissionMode::BypassPermissions,
        run_environment: RunEnvironment::Inherit,
        system_append: "You are running unattended, started by Rimaia.".to_string(),
        model: Some("claude-sonnet-5".to_string()),
        effort: None,
        disallowed_tools: Vec::new(),
        max_turns: None,
    }
}

fn argv(invocation: &Invocation) -> Vec<String> {
    invocation.args()
}

#[test]
fn a_queued_run_bypasses_permissions_and_inherits_the_operators_configuration() {
    // ADR-0012's unattended posture, and ADR-0004's amendment defaulting to
    // `inherit` — so there are no isolation flags at all in this vector.
    assert_eq!(
        argv(&invocation()),
        vec![
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--session-id",
            SESSION_ID,
            "--permission-mode",
            "bypassPermissions",
            "--append-system-prompt",
            "You are running unattended, started by Rimaia.",
            "--model",
            "claude-sonnet-5",
        ]
    );
}

#[test]
fn a_manual_run_accepts_edits_rather_than_bypassing_permissions() {
    // ADR-0012 point 6: "a run started manually with the app in the foreground
    // defaults to `acceptEdits`; bypass is for the unattended path."
    let invocation = Invocation {
        permission_mode: RunTrigger::Manual.permission_mode(),
        ..invocation()
    };

    assert_eq!(
        argv(&invocation),
        vec![
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--session-id",
            SESSION_ID,
            "--permission-mode",
            "acceptEdits",
            "--append-system-prompt",
            "You are running unattended, started by Rimaia.",
            "--model",
            "claude-sonnet-5",
        ]
    );
}

#[test]
fn strict_local_adds_two_isolation_flags_and_never_bare() {
    // `spike/FINDINGS.md` section 2 measured what these buy and cost: 255 tools
    // and 2 MCP servers become 26 and 0, at roughly a third of the price.
    //
    // `--bare` is named in the negative on purpose. ADR-0004's amendment says
    // in as many words not to implement this mode with it, because it also
    // disables `CLAUDE.md` discovery — which is the repository's own
    // instructions, wanted in both modes.
    let invocation = Invocation {
        run_environment: RunEnvironment::StrictLocal,
        ..invocation()
    };
    let args = argv(&invocation);

    assert_eq!(
        args,
        vec![
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--session-id",
            SESSION_ID,
            "--permission-mode",
            "bypassPermissions",
            "--strict-mcp-config",
            "--setting-sources",
            "project,local",
            "--append-system-prompt",
            "You are running unattended, started by Rimaia.",
            "--model",
            "claude-sonnet-5",
        ]
    );
    assert!(!args.iter().any(|arg| arg == "--bare"));
}

#[test]
fn an_effort_is_passed_only_when_the_task_names_one() {
    let with_effort = Invocation {
        effort: Some("high".to_string()),
        ..invocation()
    };

    assert_eq!(
        &argv(&with_effort)[10..],
        ["--model", "claude-sonnet-5", "--effort", "high"]
    );
    assert!(!argv(&invocation()).iter().any(|arg| arg == "--effort"));
}

#[test]
fn a_task_with_no_model_lets_the_cli_choose_its_own() {
    // ADR-0016 makes the column nullable precisely so "not set" is expressible,
    // and an empty `--model` would be a worse answer than no flag at all.
    let invocation = Invocation {
        model: None,
        ..invocation()
    };

    assert_eq!(
        argv(&invocation),
        vec![
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--session-id",
            SESSION_ID,
            "--permission-mode",
            "bypassPermissions",
            "--append-system-prompt",
            "You are running unattended, started by Rimaia.",
        ]
    );
}

#[test]
fn each_disallowed_tool_is_its_own_argument_and_a_turn_budget_terminates_the_list() {
    // `--disallowedTools` is variadic, so what ends its list is the next flag.
    // That is why the order in `Invocation::args` is a contract and not a
    // preference: a budget appended anywhere else would be swallowed as a tool.
    let invocation = Invocation {
        disallowed_tools: vec![
            "Bash(git push --force:*)".to_string(),
            "Bash(git push -f:*)".to_string(),
        ],
        max_turns: Some(40),
        ..invocation()
    };

    assert_eq!(
        &argv(&invocation)[10..],
        [
            "--model",
            "claude-sonnet-5",
            "--disallowedTools",
            "Bash(git push --force:*)",
            "Bash(git push -f:*)",
            "--max-turns",
            "40",
        ]
    );
}

#[test]
fn a_resume_replaces_the_session_id_it_would_otherwise_have_opened() {
    // `spike/FINDINGS.md` section 6: "`--session-id` on the first run and
    // `--resume` on the retry is the right shape". They are alternatives — one
    // opens an id, the other reuses one — and everything else about the
    // invocation is unchanged, which is what makes a retry a continuation.
    let resume = Invocation {
        resume: true,
        ..invocation()
    };
    let first = argv(&invocation());
    let retry = argv(&resume);

    assert_eq!(&retry[4..6], ["--resume", SESSION_ID]);
    assert!(!retry.iter().any(|arg| arg == "--session-id"));
    assert_eq!(first[6..], retry[6..], "only the session flag differs");
}

#[test]
fn the_default_blocklist_is_adr_0012s_three_operations_flag_first_and_remote_first() {
    // Security-relevant and therefore spelled out rather than counted: ADR-0012
    // point 3 names force-pushing, hard resets against remotes, and remote
    // branch deletion. `Bash(x:*)` is a command-line *prefix* match, so a
    // pattern that only knows the flag-first ordering
    // (`git push --force origin main`) never matches the equally ordinary
    // remote-first one (`git push origin --force main`) — both orderings are
    // pinned here for that reason, plus the `git push origin :branch` delete
    // shorthand, which carries neither `--delete` nor `-d`.
    assert_eq!(
        DEFAULT_DISALLOWED_TOOLS,
        [
            "Bash(git push --force:*)",
            "Bash(git push -f:*)",
            "Bash(git push --force-with-lease:*)",
            "Bash(git push --delete:*)",
            "Bash(git push -d:*)",
            "Bash(git push origin --force:*)",
            "Bash(git push origin -f:*)",
            "Bash(git push origin --delete:*)",
            "Bash(git push origin -d:*)",
            "Bash(git push origin :*)",
            "Bash(git reset --hard origin/:*)",
        ]
    );
}

#[tokio::test]
async fn an_unset_blocklist_is_the_default_and_an_emptied_one_is_no_blocklist() {
    // ADR-0012 point 3 makes the list "a setting so it can grow with
    // experience", which means an operator can also shrink it to nothing. Absent
    // and empty are deliberately not the same value — silently restoring the
    // default over an emptied field is the defect `settings::base_instructions`
    // documents from the other side.
    let harness = TestContext::new().await;

    assert_eq!(
        disallowed_tools(&harness.context.pool)
            .await
            .expect("read the default"),
        DEFAULT_DISALLOWED_TOOLS
            .iter()
            .map(|pattern| (*pattern).to_string())
            .collect::<Vec<_>>()
    );

    settings::set(
        &harness.context,
        DISALLOWED_TOOLS,
        "Bash(git push --force:*)\n\n  Bash(rm -rf /:*)  \n",
    )
    .await
    .expect("store a list");
    assert_eq!(
        disallowed_tools(&harness.context.pool)
            .await
            .expect("read it back"),
        vec![
            "Bash(git push --force:*)".to_string(),
            "Bash(rm -rf /:*)".to_string()
        ],
        "blank lines and padding are formatting, not patterns"
    );

    settings::set(&harness.context, DISALLOWED_TOOLS, "")
        .await
        .expect("empty the list");
    assert_eq!(
        disallowed_tools(&harness.context.pool)
            .await
            .expect("read it back"),
        Vec::<String>::new()
    );
}

// ---------------------------------------------------------------------------
// The environment the child inherits
// ---------------------------------------------------------------------------

/// The `CLAUDE_*` / `CLAUDECODE` variables `spike/FINDINGS.md` section 2b found
/// Claude Code 2.1.234 exporting into its children. Spelled out here because the
/// rule under test is a *prefix* rule chosen precisely so this list can grow
/// without anyone editing code — testing it against the real vocabulary is what
/// keeps that claim honest.
const OBSERVED_CLAUDE_VARS: [&str; 13] = [
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
    "CLAUDE_CODE_AUTO_CONNECT_IDE",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_BASH_MAINTAIN_PROJECT_WORKING_DIR",
    "CLAUDE_AGENT_SDK_VERSION",
    "CLAUDE_CODE_ACTION",
    "CLAUDE_CODE_OAUTH_TOKEN",
];

#[test]
fn every_variable_claude_code_exports_into_a_child_is_stripped() {
    for name in OBSERVED_CLAUDE_VARS {
        assert!(is_process_identity(name), "{name} would reach the child");
    }

    let parent: Vec<String> = OBSERVED_CLAUDE_VARS
        .iter()
        .map(|name| (*name).to_string())
        .chain(
            [
                "PATH",
                "HOME",
                "SHELL",
                "ANTHROPIC_API_KEY",
                "MY_CLAUDE_LOG",
            ]
            .map(str::to_string),
        )
        .collect();

    assert_eq!(
        inherited_identity_vars(parent).len(),
        OBSERVED_CLAUDE_VARS.len(),
        "the rule must take the identity variables and nothing else"
    );
}

#[tokio::test]
async fn a_run_never_hands_the_child_an_inherited_claude_variable_in_either_mode() {
    // Task 008's acceptance criterion, proved against a real spawned process
    // whose parent — this test — is carrying the variables. Setting them here
    // rather than asserting over an empty map is the point: a rule that only
    // works on an environment nobody has is not the rule that is needed.
    //
    // Only ever *adding* `CLAUDE_*` names to the shared process environment, so
    // a sibling test running concurrently can only make this assertion stronger.
    std::env::set_var("CLAUDE_CODE_SESSION_ID", "a-parent-session");
    std::env::set_var("CLAUDECODE", "1");
    std::env::set_var("RIMAIA_TEST_MARKER", "kept");

    for environment in [RunEnvironment::Inherit, RunEnvironment::StrictLocal] {
        let fixture = RunnerFixture::new().await;
        settings::set_run_environment(&fixture.harness.context, environment)
            .await
            .expect("choose the run environment");
        let cli = FakeCli::replaying("success", 0);

        fixture.run(&cli).await.expect("the run completes");

        let names = cli.child_env_names();
        assert!(
            !names.iter().any(|name| is_process_identity(name)),
            "{environment:?} leaked a process-identity variable: {names:?}",
        );
        assert!(
            names.iter().any(|name| name == "RIMAIA_TEST_MARKER"),
            "{environment:?} stripped more than it was asked to",
        );
    }
}

#[tokio::test]
async fn strict_local_is_the_only_mode_that_reaches_the_cli_as_isolation_flags() {
    // The other half of `strict_local_adds_two_isolation_flags_and_never_bare`:
    // that one pins the vector, this one proves the vector is what a real
    // process is actually handed, read back off the child's own `argv`.
    let fixture = RunnerFixture::new().await;
    settings::set_run_environment(&fixture.harness.context, RunEnvironment::StrictLocal)
        .await
        .expect("choose the run environment");
    let cli = FakeCli::replaying("success", 0);

    fixture.run(&cli).await.expect("the run completes");

    let argv = cli.child_argv();
    assert!(argv.contains(&"--strict-mcp-config".to_string()));
    assert_eq!(
        argv.iter()
            .position(|arg| arg == "--setting-sources")
            .and_then(|at| argv.get(at + 1))
            .map(String::as_str),
        Some("project,local")
    );
    assert!(!argv.contains(&"--bare".to_string()));

    // And the blocklist, which is the other flag a silent regression would be
    // invisible in: ADR-0012's mitigations are only mitigations if they reach
    // the process.
    for pattern in DEFAULT_DISALLOWED_TOOLS {
        assert!(
            argv.contains(&pattern.to_string()),
            "{pattern} never arrived"
        );
    }
    assert!(argv.contains(&"--disallowedTools".to_string()));
}

#[test]
fn the_two_environment_recordings_are_what_each_mode_looks_like_from_the_init_event() {
    // Task 008's acceptance criterion names the evidence: "verified by asserting
    // on the `init` event's `mcp_servers[]` and `tools[]`". These are the two
    // recordings of one probe run, and the numbers are the measurement — 229
    // fewer tools and two fewer MCP servers, for the same prompt.
    let inherited = init_of("env-leak-default-settings");
    let isolated = init_of("env-leak-isolated-settings");

    assert_eq!(inherited.tools.len(), 255);
    assert_eq!(
        inherited
            .mcp_servers
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>(),
        ["Brewale", "claude.ai Google Drive"],
    );

    assert_eq!(isolated.tools.len(), 26);
    assert_eq!(isolated.mcp_servers, vec![]);
}

// ---------------------------------------------------------------------------
// The prerequisite
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_claude_binary_is_refused_before_any_run_state_is_written() {
    // Task 008's acceptance criterion, and the reason the probe runs before the
    // claim rather than after it: a prerequisite that is not installed must not
    // leave a task reading "running" until the next launch reconciles it.
    let fixture = RunnerFixture::new().await;
    let config = RunnerConfig {
        program: fixture.data.path().join("no-such-claude"),
        ..RunnerConfig::default()
    };

    let error = run_task(
        &fixture.harness.context,
        &fixture.paths,
        &config,
        fixture.request(),
    )
    .await
    .expect_err("a run needs the CLI");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert!(
        error
            .to_string()
            .contains("Rimaia drives your own installation"),
        "the message has to say what to do: {error}"
    );
    assert!(error.to_string().contains("no-such-claude"), "{error}");

    let detail = fixture.detail().await;
    assert_eq!(
        detail.task.run_state,
        RunState::Idle,
        "the task was claimed"
    );
    assert_eq!(detail.last_run, None, "a `runs` row was opened");
    assert_eq!(detail.task.branch, None, "a worktree was prepared");
}

// ---------------------------------------------------------------------------
// Verifying what the CLI actually applied
// ---------------------------------------------------------------------------

#[test]
fn an_init_that_echoes_the_mode_that_was_requested_passes() {
    // Every recording in the corpus was captured under `bypassPermissions`.
    let init = init_of("success");

    assert_eq!(init.permission_mode.as_deref(), Some("bypassPermissions"));
    verify_permission_mode(&init, PermissionMode::BypassPermissions).expect("the modes agree");
}

#[test]
fn an_init_that_echoes_a_different_mode_is_refused_by_name() {
    // Driven from a *modified copy* of a recording's own init, held here rather
    // than written into `tests/fixtures/cli/` — the corpus records what the CLI
    // did, and a CLI that silently widened permissions is a thing it has never
    // been observed doing.
    let line = fixture_lines("success").next().expect("the init event");
    let mut raw: serde_json::Value = serde_json::from_str(&line).expect("the init event is JSON");
    raw["permissionMode"] = serde_json::json!("bypassPermissions");

    let RunEvent::Init(init) = parse_line(&raw.to_string()).expect("still JSON") else {
        panic!("the first line of a recording is its init event");
    };
    let error = verify_permission_mode(&init, PermissionMode::AcceptEdits)
        .expect_err("a widened posture is not something to proceed under");

    assert_eq!(error.code(), ErrorCode::Internal);
    assert_eq!(
        error.to_string(),
        "Claude Code applied permission mode \"bypassPermissions\" when Rimaia asked for \
         \"acceptEdits\". The run was stopped rather than continued under a posture nobody \
         chose (ADR-0012)."
    );
}

#[test]
fn an_init_that_names_no_permission_mode_at_all_does_not_stop_the_run() {
    // ADR-0004's tolerance rule, at its sharpest: a renamed field would fail
    // every run at once, which is the loudest possible version of the
    // "a Claude Code update must not break a queue" failure that rule prevents.
    // A widened mode is evidence; an absent one is only an absence.
    let init = rimaia_core::runner::events::InitEvent::default();

    verify_permission_mode(&init, PermissionMode::BypassPermissions)
        .expect("an unverifiable mode is not a mismatch");
}

#[tokio::test]
async fn a_cli_that_applied_a_permission_mode_nobody_asked_for_stops_the_run() {
    // The same recorded fact as the passing case, turned around: `success.jsonl`
    // was captured under `bypassPermissions`, so asking for `acceptEdits` — what
    // ADR-0012 gives a manual foreground run — is a genuine recorded mismatch
    // rather than a synthesized one. The run is stopped and recorded as fatal.
    let fixture = RunnerFixture::new().await;
    let cli = FakeCli::replaying("success", 0);

    // The process itself ran fine; the refusal is Rimaia's, so it is recorded
    // the way every other failed run is rather than raised as a spawn error.
    let run = run_task(
        &fixture.harness.context,
        &fixture.paths,
        &fixture.config(&cli),
        RunRequest {
            task_id: fixture.task_id.clone(),
            trigger: RunTrigger::Manual,
            cancel: CancelSignal::new(),
        },
    )
    .await
    .expect("the attempt is recorded rather than lost");

    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(run.exit_class, Some(ExitClass::Fatal));
    assert!(
        run.error_message
            .as_deref()
            .is_some_and(|message| message.contains("acceptEdits")),
        "the row must name both modes: {:?}",
        run.error_message
    );
    assert_eq!(fixture.task().await.run_state, RunState::Failed);
    assert_eq!(
        fixture.task().await.column,
        BoardColumn::Ready,
        "a stopped run must not move the card to in_review"
    );
}

#[tokio::test]
async fn a_manual_run_against_an_init_that_echoes_accept_edits_succeeds() {
    // Every whole-loop test in this file but this one drives `RunTrigger::Queued`,
    // because every recording was captured under `bypassPermissions` — the mode
    // that trigger requests. `RunTrigger::Manual` (the foreground "Run now"
    // button's trigger, ADR-0012's `acceptEdits`) otherwise has exactly one test
    // anywhere in the suite, and it expects the run to fail
    // (`a_cli_that_applied_a_permission_mode_nobody_asked_for_stops_the_run`
    // above). A regression specific to the manual path would still be green.
    // `success.jsonl`'s `init` line, rewritten in memory to the mode a manual
    // run actually requests, gives it one real success to drive through.
    let fixture = RunnerFixture::new().await;
    let cli = FakeCli::replaying_with_permission_mode("success", "acceptEdits", 0);

    let run = run_task(
        &fixture.harness.context,
        &fixture.paths,
        &fixture.config(&cli),
        RunRequest {
            task_id: fixture.task_id.clone(),
            trigger: RunTrigger::Manual,
            cancel: CancelSignal::new(),
        },
    )
    .await
    .expect("the manual run completes");

    assert_eq!(run.status, RunStatus::Succeeded);
    assert_eq!(run.exit_class, Some(ExitClass::Success));
    assert!(
        cli.child_argv()
            .windows(2)
            .any(|pair| pair == ["--permission-mode", "acceptEdits"]),
        "the manual trigger must have asked for its own mode, not the queued one: {:?}",
        cli.child_argv()
    );

    let task = fixture.task().await;
    assert_eq!(task.column, BoardColumn::InReview);
    assert_eq!(task.run_state, RunState::Idle);
}

// ---------------------------------------------------------------------------
// The whole loop, against a recorded stream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_recorded_success_drives_its_task_to_in_review() {
    // The shape of task 008's first acceptance criterion, minus the part only a
    // real agent can supply: a plan goes in, a run is recorded, and the card
    // ends up where the morning review starts.
    let fixture = RunnerFixture::new().await;
    let cli = FakeCli::replaying("success", 0);

    let run = fixture.run(&cli).await.expect("the run completes");

    assert_eq!(run.status, RunStatus::Succeeded);
    assert_eq!(run.exit_class, Some(ExitClass::Success));
    assert_eq!(run.num_turns, Some(5));
    assert_eq!(
        run.cost_usd,
        Some(0.150_292_5),
        "ADR-0004: show what it cost"
    );
    assert_eq!(run.error_message, None);
    assert_eq!(run.session_id.len(), 36, "a UUID, minted before the spawn");

    let task = fixture.task().await;
    assert_eq!(task.column, BoardColumn::InReview);
    assert_eq!(task.run_state, RunState::Idle);
    assert!(task.branch.is_some(), "task 007 prepared a worktree first");
}

#[tokio::test]
async fn the_transcript_is_the_stream_verbatim_and_valid_jsonl() {
    // ADR-0013's transcript, and task 008's "complete transcript on disk as
    // valid JSONL". Byte-for-byte against the recording rather than a spot
    // check: the file is evidence, and evidence that has been reformatted is
    // not the same evidence.
    let fixture = RunnerFixture::new().await;
    let cli = FakeCli::replaying("success", 0);

    let run = fixture.run(&cli).await.expect("the run completes");

    let recorded: Vec<String> = fixture_lines("success").collect();
    let written = fixture.transcript(&run.id);
    assert_eq!(written, recorded);
    for line in &written {
        serde_json::from_str::<serde_json::Value>(line).expect("every line is a JSON document");
    }
    assert_eq!(
        run.log_path,
        transcript_path(&fixture.paths, &fixture.task_id, &run.id)
            .to_string_lossy()
            .into_owned()
    );
}

#[tokio::test]
async fn the_prompt_reaches_the_child_on_stdin_and_is_the_one_task_006_composes() {
    // Two acceptance criteria in one assertion, because they are the same
    // string: "the stored prompt matches what task 006 composes", and the prompt
    // is "written to stdin, then stdin closed". The child's `cat` only returns
    // at EOF, so the run completing at all is the closure being proved.
    let fixture = RunnerFixture::new().await;
    let cli = FakeCli::replaying("success", 0);

    let run = fixture.run(&cli).await.expect("the run completes");

    // Composed after the run, not before it: `worktree::prepare` is what writes
    // `tasks.branch`, and the prompt names the branch. A prompt composed from
    // the row as it stood a moment earlier would name nothing — which is why
    // the runner re-reads the task between those two steps.
    let expected = fixture.composed_prompt().await;
    assert_eq!(cli.child_stdin(), expected);
    assert_eq!(run.prompt, expected);
    assert!(
        expected.contains(&format!(
            "- Branch: {}",
            fixture.task().await.branch.expect("a prepared worktree")
        )),
        "the prompt has to name the branch the agent is working on",
    );
}

#[tokio::test]
async fn a_prompt_several_thousand_tokens_long_arrives_whole() {
    // Task 008's acceptance criterion. Larger than a pipe buffer on purpose:
    // this is what would deadlock if the prompt were written before the read
    // loop started rather than alongside it.
    let fixture = RunnerFixture::new().await;
    let plan = "- Rewrite the parser so it tolerates an unknown event type.\n".repeat(1_200);
    assert!(
        plan.len() > 64 * 1024,
        "smaller than a pipe buffer proves nothing"
    );
    fixture.set_plan(&plan).await;
    let cli = FakeCli::replaying("success", 0);

    let run = fixture.run(&cli).await.expect("the run completes");

    let delivered = cli.child_stdin();
    assert_eq!(delivered.len(), run.prompt.len());
    assert_eq!(delivered, run.prompt);
    assert!(
        delivered.contains(plan.trim()),
        "the plan arrived truncated"
    );
}

#[tokio::test]
async fn the_child_works_in_the_tasks_worktree_and_never_in_the_repository() {
    // ADR-0005: the worst common outcome is a bad branch, not lost local work.
    let fixture = RunnerFixture::new().await;
    let cli = FakeCli::replaying("success", 0);

    fixture.run(&cli).await.expect("the run completes");

    let worktree = fixture
        .task()
        .await
        .worktree_path
        .expect("task 007 recorded one");
    assert_eq!(canonical(&cli.child_cwd()), canonical(&worktree));
    assert_ne!(
        canonical(&cli.child_cwd()),
        canonical(&fixture.repository_path())
    );
}

#[tokio::test]
async fn the_live_view_reports_turns_and_the_current_tool_call_while_the_run_is_in_flight() {
    // Task 008's acceptance criterion, over the channel seam-contract D14 gives
    // the tail. Subscribed before the run so nothing is missed, and drained
    // afterwards so the assertion needs no timing at all — the snapshots are
    // evidence of what a watcher saw *during* the run either way.
    let fixture = RunnerFixture::new().await;
    let mut tail = fixture.harness.context.subscribe_tail();
    let cli = FakeCli::replaying("success", 0);

    let run = fixture.run(&cli).await.expect("the run completes");

    let mut snapshots = Vec::new();
    while let Ok(snapshot) = tail.try_recv() {
        snapshots.push(snapshot);
    }
    assert!(
        snapshots.len() > 1,
        "a run that published once is not a live view"
    );
    assert!(
        snapshots.iter().any(|snapshot| snapshot
            .current_tool
            .as_ref()
            .is_some_and(|call| call.name == "Bash")),
        "no snapshot showed the tool call the agent was making",
    );
    assert!(
        snapshots
            .iter()
            .any(|snapshot| snapshot.last_assistant_text.is_some()),
        "no snapshot carried the agent's own words",
    );
    assert_eq!(snapshots[0].run_id, run.id);
    assert_eq!(
        snapshots.last().map(|snapshot| snapshot.turns),
        Some(5),
        "the terminal event's own count replaces the running approximation",
    );
}

#[tokio::test]
async fn stderr_is_captured_beside_the_transcript_without_getting_into_it() {
    // Task 008's scope: "capture stderr separately into the same run directory".
    // Separately, because a stray warning interleaved into the `.jsonl` would
    // break every reader of it for the sake of one line.
    let fixture = RunnerFixture::new().await;
    let cli = FakeCli::with_body(&format!(
        "echo 'a warning nobody asked for' >&2\ncat '{}'\nexit 0\n",
        fixture_path("success").display()
    ));

    let run = fixture.run(&cli).await.expect("the run completes");

    let captured = std::fs::read_to_string(stderr_path(&fixture.paths, &fixture.task_id, &run.id))
        .expect("the stderr log");
    assert_eq!(captured, "a warning nobody asked for\n");
    assert_eq!(
        fixture.transcript(&run.id),
        fixture_lines("success").collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn an_injected_unknown_event_type_does_not_fail_the_run() {
    // Task 008's acceptance criterion and ADR-0004's rule, end to end.
    // `unknown-event-type.jsonl` carries a `telemetry_ping` and a
    // `system/context_compaction`, neither of which any recording contains.
    let fixture = RunnerFixture::new().await;
    let cli = FakeCli::replaying("unknown-event-type", 0);

    let run = fixture.run(&cli).await.expect("the run completes");

    assert_eq!(run.exit_class, Some(ExitClass::Success));
    assert_eq!(fixture.task().await.column, BoardColumn::InReview);
    assert!(
        fixture
            .transcript(&run.id)
            .iter()
            .any(|line| line.contains("telemetry_ping")),
        "an unmodelled event is persisted verbatim, not dropped",
    );
}

#[tokio::test]
async fn a_line_the_parser_cannot_read_neither_fails_the_run_nor_leaves_the_transcript() {
    let fixture = RunnerFixture::new().await;
    let cli = FakeCli::replaying("malformed-line", 0);

    let run = fixture.run(&cli).await.expect("the run completes");

    assert_eq!(run.exit_class, Some(ExitClass::Success));
    assert_eq!(
        fixture.transcript(&run.id),
        fixture_lines("malformed-line").collect::<Vec<_>>(),
        "the unreadable line is evidence too",
    );
}

#[tokio::test]
async fn a_stream_that_stops_without_a_result_is_recorded_rather_than_lost() {
    // `truncated-stream.jsonl` never reaches a terminal event. ADR-0011 gives
    // that to `transient`, and the task lands in `waiting_retry` so task 014 has
    // something to resume rather than a card stuck mid-run.
    let fixture = RunnerFixture::new().await;
    let cli = FakeCli::replaying("truncated-stream", 1);

    let run = fixture.run(&cli).await.expect("the run completes");

    assert_eq!(run.exit_class, Some(ExitClass::Transient));
    assert_eq!(
        run.error_message.as_deref(),
        Some("the event stream ended without a result event; the process exited with code 1")
    );
    assert_eq!(fixture.task().await.run_state, RunState::WaitingRetry);
}

#[tokio::test]
async fn a_background_process_the_agent_left_behind_does_not_hold_a_finished_run_open() {
    // The failure mode that makes the process group load-bearing even when
    // nobody cancels anything. An agent that starts a dev server hands it the
    // run's own stdout; the CLI then exits cleanly, and the pipe stays open
    // because something else is holding the write end. A runner that waited for
    // EOF would leave this run — and its task at `running` — hanging until the
    // app was restarted, with a `result` event already on disk saying it
    // succeeded.
    let fixture = RunnerFixture::new().await;
    let cli = FakeCli::leaking_a_background_process("success");

    let run = tokio::time::timeout(TEST_TIMEOUT, fixture.run(&cli))
        .await
        .expect("a leaked pipe must not hang a finished run")
        .expect("the run completes");

    assert_eq!(run.exit_class, Some(ExitClass::Success));
    assert_eq!(fixture.task().await.column, BoardColumn::InReview);

    let leaked = cli.grandchild_pid();
    assert!(
        !is_running(&leaked),
        "pid {leaked} survived the run that started it",
    );
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancelling_terminates_the_whole_process_tree_and_leaves_no_orphan() {
    // Task 008's acceptance criterion, and the measurement `spike/FINDINGS.md`
    // section 7 made: `process_group(0)` plus a signal to `-<pgid>` takes the
    // tree down, where signalling the child alone leaves everything it started.
    // The stand-in starts a grandchild and reports its pid precisely so the
    // difference between those two is what this asserts.
    let fixture = RunnerFixture::new().await;
    let cli = FakeCli::spawning_a_grandchild_then_waiting("interrupted-sigterm", 6);
    let cancel = CancelSignal::new();
    let tail = fixture.harness.context.subscribe_tail();

    let (run, ()) = tokio::time::timeout(TEST_TIMEOUT, async {
        tokio::join!(
            fixture.run_with(&cli, &cancel),
            cancel_once_the_run_is_live(tail, cancel.clone()),
        )
    })
    .await
    .expect("cancellation must not hang the runner");
    let run = run.expect("a cancelled run still records itself");

    assert_eq!(run.status, RunStatus::Cancelled);
    assert_eq!(run.exit_class, Some(ExitClass::Cancelled));
    assert_eq!(run.error_message.as_deref(), Some("the run was cancelled"));
    assert_eq!(fixture.task().await.run_state, RunState::Failed);

    let grandchild = cli.grandchild_pid();
    assert!(
        !is_running(&grandchild),
        "pid {grandchild} outlived the run it belonged to",
    );
}

#[tokio::test]
async fn a_run_that_emits_its_result_and_exits_143_is_cancelled_rather_than_a_dead_stream() {
    // `spike/FINDINGS.md` section 5, as process behaviour rather than as a
    // parsed file: the CLI answers SIGTERM by finishing its stream and *then*
    // exiting 143. The stand-in does exactly that, replaying the second half of
    // the recording of a real killed run from its own signal handler.
    let fixture = RunnerFixture::new().await;
    let cli = FakeCli::replaying_then_finishing_on_sigterm("interrupted-sigterm", 6, 143);
    let cancel = CancelSignal::new();
    let tail = fixture.harness.context.subscribe_tail();

    let (run, ()) = tokio::time::timeout(TEST_TIMEOUT, async {
        tokio::join!(
            fixture.run_with(&cli, &cancel),
            cancel_once_the_run_is_live(tail, cancel.clone()),
        )
    })
    .await
    .expect("cancellation must not hang the runner");
    let run = run.expect("a cancelled run still records itself");

    assert_eq!(run.exit_class, Some(ExitClass::Cancelled));
    assert_eq!(
        fixture.transcript(&run.id),
        fixture_lines("interrupted-sigterm").collect::<Vec<_>>(),
        "the events that arrived after the signal are part of the record",
    );
}

#[tokio::test]
async fn a_child_that_ignores_sigterm_is_killed_when_the_grace_period_ends() {
    // The escalation. The stand-in ignores SIGTERM outright and would otherwise
    // keep running for half a minute before replaying the rest of the stream —
    // so the tail of the recording being absent from the transcript is the
    // SIGKILL having landed, exactly, rather than a timing guess.
    let fixture = RunnerFixture::new().await;
    let cli = FakeCli::ignoring_sigterm("interrupted-sigterm", 6);
    let cancel = CancelSignal::new();
    let tail = fixture.harness.context.subscribe_tail();
    let config = RunnerConfig {
        grace_period: Duration::from_millis(150),
        ..fixture.config(&cli)
    };

    let (run, ()) = tokio::time::timeout(TEST_TIMEOUT, async {
        tokio::join!(
            fixture.run_with_config(&config, &cancel),
            cancel_once_the_run_is_live(tail, cancel.clone()),
        )
    })
    .await
    .expect("a child that ignores SIGTERM must not hang the runner");
    let run = run.expect("a killed run still records itself");

    assert_eq!(run.exit_class, Some(ExitClass::Cancelled));
    assert_eq!(
        fixture.transcript(&run.id).len(),
        6,
        "the stand-in reached its unreachable tail, so nothing killed it",
    );
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_repository_that_has_not_opted_in_cannot_start_a_task() {
    // ADR-0012 point 1, through task 003's own predicate rather than re-derived
    // here — so the board's disabled "Run now" explanation and this refusal are
    // one sentence rather than two that drift.
    let fixture = RunnerFixture::without_opt_in().await;
    let cli = FakeCli::replaying("success", 0);

    let error = fixture
        .run(&cli)
        .await
        .expect_err("an un-opted repository holds tasks but cannot run them");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert!(
        error
            .to_string()
            .contains("has not enabled unattended agent runs"),
        "{error}"
    );
    assert_eq!(fixture.detail().await.last_run, None);
    assert_eq!(fixture.task().await.run_state, RunState::Idle);
}

#[tokio::test]
async fn a_task_that_failed_last_night_can_be_started_again() {
    // "Run now" on a failed card is a requeue, and ADR-0007's own note on that
    // edge says trying again "re-enters at Queued like every other start". A
    // runner that refused would leave the user with a board they cannot use.
    let fixture = RunnerFixture::new().await;
    for state in [RunState::Queued, RunState::Running, RunState::Failed] {
        tasks::set_run_state(&fixture.harness.context, &fixture.task_id, state)
            .await
            .expect("last night's failure");
    }
    let cli = FakeCli::replaying("success", 0);

    let run = fixture.run(&cli).await.expect("the run completes");

    assert_eq!(run.attempt, 1, "last night failed before it opened a row");
    assert_eq!(fixture.task().await.run_state, RunState::Idle);
    assert_eq!(fixture.task().await.column, BoardColumn::InReview);
}

// ---------------------------------------------------------------------------
// A stand-in for the CLI
// ---------------------------------------------------------------------------

/// A shell script that behaves like `claude -p --output-format stream-json`
/// enough to supervise, and records everything it was handed.
///
/// Real process semantics — argv, environment, cwd, stdin, pipes, exit status,
/// signals, process groups — with no network, no tokens and no chance of the
/// nested-session hazard a real child would carry. See this file's header.
struct FakeCli {
    /// Held for its `Drop`; every path below points inside it.
    dir: TempDir,
}

impl FakeCli {
    /// Replays `fixture` to stdout and exits with `code`.
    fn replaying(fixture: &str, code: i32) -> Self {
        Self::with_body(&format!(
            "cat '{}'\nexit {code}\n",
            fixture_path(fixture).display()
        ))
    }

    /// Replays `fixture` with its `init` line's `permissionMode` rewritten to
    /// `permission_mode`, and exits with `code`.
    ///
    /// Every recording in the corpus was captured under `bypassPermissions`, so
    /// without this every whole-loop test drives `RunTrigger::Queued` — the
    /// trigger that asks for that mode — and the one test that ever exercises
    /// `RunTrigger::Manual` (`acceptEdits`) is the mismatch case, which expects
    /// the run to fail. This is the same technique
    /// [`an_init_that_echoes_a_different_mode_is_refused_by_name`] uses on one
    /// parsed event, applied to a whole spawned stream: an in-memory edited
    /// *copy* of a recording, not a new fixture on disk.
    fn replaying_with_permission_mode(fixture: &str, permission_mode: &str, code: i32) -> Self {
        let mut lines: Vec<String> = fixture_lines(fixture).collect();
        let mut init: serde_json::Value =
            serde_json::from_str(&lines[0]).expect("the first line is the init event");
        init["permissionMode"] = serde_json::json!(permission_mode);
        lines[0] = init.to_string();

        let cli = Self::empty();
        let rewritten = cli.path("rewritten.jsonl");
        std::fs::write(&rewritten, lines.join("\n") + "\n").expect("write the rewritten recording");
        cli.write_script(&format!("cat '{}'\nexit {code}\n", rewritten.display()));
        cli
    }

    /// Replays `fixture` and exits cleanly, having started a background process
    /// that inherits the run's stdout — an agent leaving a dev server behind.
    fn leaking_a_background_process(fixture: &str) -> Self {
        let cli = Self::empty();
        cli.write_script(&format!(
            "sleep 300 &\necho $! > '{pid}'\ncat '{fixture}'\nexit 0\n",
            pid = cli.path("grandchild.pid").display(),
            fixture = fixture_path(fixture).display(),
        ));
        cli
    }

    /// Replays the first `head` lines of `fixture`, starts a grandchild that
    /// would outlive an incorrectly-scoped signal, and then waits to be stopped.
    fn spawning_a_grandchild_then_waiting(fixture: &str, head: usize) -> Self {
        let cli = Self::empty();
        let (head_file, _) = cli.split_recording(fixture, head);
        cli.write_script(&format!(
            "sleep 300 &\necho $! > '{pid}'\ncat '{head_file}'\nsleep 300\n",
            pid = cli.path("grandchild.pid").display(),
            head_file = head_file.display(),
        ));
        cli
    }

    /// Replays the first `head` lines, then — on SIGTERM — the rest of the same
    /// recording, and exits `code`. What the real CLI was measured doing.
    fn replaying_then_finishing_on_sigterm(fixture: &str, head: usize, code: i32) -> Self {
        let cli = Self::empty();
        let (head_file, rest_file) = cli.split_recording(fixture, head);
        cli.write_script(&format!(
            "trap \"cat '{rest}'; exit {code}\" TERM\ncat '{head}'\nsleep 300 &\nwait\n",
            rest = rest_file.display(),
            head = head_file.display(),
        ));
        cli
    }

    /// Replays the first `head` lines and then ignores SIGTERM entirely. The
    /// unreachable tail after the loop is what makes the escalation assertable:
    /// if it ever appears in a transcript, nothing killed this process.
    fn ignoring_sigterm(fixture: &str, head: usize) -> Self {
        let cli = Self::empty();
        let (head_file, rest_file) = cli.split_recording(fixture, head);
        cli.write_script(&format!(
            "trap '' TERM\ncat '{head}'\nn=0\nwhile [ $n -lt 1200 ]; do sleep 0.05; n=$((n+1)); done\ncat '{rest}'\n",
            head = head_file.display(),
            rest = rest_file.display(),
        ));
        cli
    }

    fn with_body(body: &str) -> Self {
        let cli = Self::empty();
        cli.write_script(body);
        cli
    }

    fn empty() -> Self {
        Self {
            dir: tempfile::Builder::new()
                .prefix("rimaia-fake-cli-")
                .tempdir()
                .expect("temp dir for the stand-in CLI"),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn program(&self) -> PathBuf {
        self.path("claude")
    }

    /// Splits a recording into the part replayed immediately and the part a
    /// signal handler replays afterwards.
    ///
    /// Not a new fixture: the same recorded bytes, handed over in two pieces so
    /// a test can act between them. Nothing is edited, added or reordered.
    fn split_recording(&self, fixture: &str, head: usize) -> (PathBuf, PathBuf) {
        let lines: Vec<String> = fixture_lines(fixture).collect();
        assert!(head < lines.len(), "{fixture} is shorter than {head} lines");

        let head_file = self.path("head.jsonl");
        let rest_file = self.path("rest.jsonl");
        std::fs::write(&head_file, lines[..head].join("\n") + "\n").expect("write the head");
        std::fs::write(&rest_file, lines[head..].join("\n") + "\n").expect("write the rest");
        (head_file, rest_file)
    }

    /// A shebang script, executed directly rather than through `sh -c` — the
    /// same rule the production code follows, for the same reason: these paths
    /// contain spaces.
    ///
    /// `--version` short-circuits, because the runner probes for the
    /// prerequisite before it starts anything and a stand-in that replayed a
    /// whole stream at the probe would be answering the wrong question.
    fn write_script(&self, body: &str) {
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = '--version' ]; then echo '2.1.234 (Claude Code)'; exit 0; fi\n\
             printf '%s\\0' \"$@\" > '{argv}'\n\
             env > '{env}'\n\
             pwd > '{cwd}'\n\
             cat > '{stdin}'\n\
             {body}",
            argv = self.path("argv").display(),
            env = self.path("env").display(),
            cwd = self.path("cwd").display(),
            stdin = self.path("stdin").display(),
        );

        let program = self.program();
        std::fs::write(&program, script).expect("write the stand-in CLI");
        make_executable(&program);
    }

    /// The child's own `argv`, NUL-separated on the way out because the composed
    /// system prompt contains newlines.
    fn child_argv(&self) -> Vec<String> {
        let raw = std::fs::read(self.path("argv")).expect("the stand-in recorded its argv");
        raw.split(|byte| *byte == 0)
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .filter(|part| !part.is_empty())
            .collect()
    }

    fn child_env_names(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("env"))
            .expect("the stand-in recorded its environment")
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_string()))
            .collect()
    }

    fn child_cwd(&self) -> String {
        std::fs::read_to_string(self.path("cwd"))
            .expect("the stand-in recorded its working directory")
            .trim()
            .to_string()
    }

    fn child_stdin(&self) -> String {
        std::fs::read_to_string(self.path("stdin")).expect("the stand-in recorded its stdin")
    }

    fn grandchild_pid(&self) -> String {
        std::fs::read_to_string(self.path("grandchild.pid"))
            .expect("the stand-in recorded its grandchild")
            .trim()
            .to_string()
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("make the stand-in executable");
}

/// Whether `pid` is still a process doing something.
///
/// A zombie counts as gone: its parent died with it and `init` has not got round
/// to reaping it yet, which is a corpse rather than the orphaned `npm` this
/// exists to rule out.
fn is_running(pid: &str) -> bool {
    let output = Command::new("ps")
        .args(["-o", "state=", "-p", pid])
        .output()
        .expect("ps must be runnable");
    let state = String::from_utf8_lossy(&output.stdout).trim().to_string();

    !state.is_empty() && !state.starts_with('Z')
}

fn canonical(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

/// The `init` event of a recording, as the runner would see it.
fn init_of(fixture: &str) -> rimaia_core::runner::events::InitEvent {
    fixture_lines(fixture)
        .filter_map(|line| parse_line(&line).ok())
        .find_map(|event| match event {
            RunEvent::Init(init) => Some(init),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{fixture} has no init event"))
}

/// Cancels as soon as the run says it is in flight.
///
/// The tail channel is the run reporting its own progress (seam-contract D14),
/// so this needs no sleep and no polling. The bound is there for the failing
/// case only: a run that dies before it streams anything would otherwise leave
/// this waiting on a sender that outlives it.
async fn cancel_once_the_run_is_live(mut tail: Receiver<RunTail>, cancel: CancelSignal) {
    let _ = tokio::time::timeout(TEST_TIMEOUT, tail.recv()).await;
    cancel.cancel();
}

// ---------------------------------------------------------------------------
// A registered repository with a runnable task
// ---------------------------------------------------------------------------

struct RunnerFixture {
    harness: TestContext,
    /// Held for their `Drop`; the paths below point inside them.
    repository: TempRepo,
    data: TempDir,
    paths: AppPaths,
    repository_id: String,
    task_id: String,
}

impl RunnerFixture {
    /// A real git repository, registered and opted in to unattended runs, with
    /// one task in `ready` carrying a plan.
    async fn new() -> Self {
        Self::build(true).await
    }

    /// The same, with ADR-0012's per-repository opt-in left off.
    async fn without_opt_in() -> Self {
        Self::build(false).await
    }

    async fn build(opt_in: bool) -> Self {
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

        if opt_in {
            repo::set_allow_unattended_runs(&harness.context, &registered.id, true)
                .await
                .expect("ADR-0012's per-repository opt-in");
        }

        let task = tasks::create_task(
            &harness.context,
            NewTask {
                repository_id: registered.id.clone(),
                title: TASK_TITLE.to_string(),
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
            repository,
            data,
            paths,
            repository_id: registered.id,
            task_id: task.id,
        }
    }

    /// A queued run — the trigger whose permission mode every recording in the
    /// corpus was captured under. See this file's header.
    fn request(&self) -> RunRequest {
        RunRequest {
            task_id: self.task_id.clone(),
            trigger: RunTrigger::Queued,
            cancel: CancelSignal::new(),
        }
    }

    fn config(&self, cli: &FakeCli) -> RunnerConfig {
        RunnerConfig {
            program: cli.program(),
            ..RunnerConfig::default()
        }
    }

    async fn run(&self, cli: &FakeCli) -> rimaia_core::Result<Run> {
        run_task(
            &self.harness.context,
            &self.paths,
            &self.config(cli),
            self.request(),
        )
        .await
    }

    async fn run_with(&self, cli: &FakeCli, cancel: &CancelSignal) -> rimaia_core::Result<Run> {
        self.run_with_config(&self.config(cli), cancel).await
    }

    async fn run_with_config(
        &self,
        config: &RunnerConfig,
        cancel: &CancelSignal,
    ) -> rimaia_core::Result<Run> {
        run_task(
            &self.harness.context,
            &self.paths,
            config,
            RunRequest {
                cancel: cancel.clone(),
                ..self.request()
            },
        )
        .await
    }

    fn repository_path(&self) -> String {
        self.repository.path().to_string_lossy().into_owned()
    }

    async fn set_plan(&self, plan: &str) {
        tasks::update_task(
            &self.harness.context,
            &self.task_id,
            TaskPatch {
                plan: Patch::Set(plan.to_string()),
                ..TaskPatch::default()
            },
        )
        .await
        .expect("write a longer plan");
    }

    async fn detail(&self) -> tasks::TaskDetail {
        tasks::get_task(&self.harness.context, &self.task_id)
            .await
            .expect("read the task")
    }

    async fn task(&self) -> Task {
        self.detail().await.task
    }

    /// What task 006 composes for this task right now — the string the run is
    /// expected to have sent and stored.
    async fn composed_prompt(&self) -> String {
        let detail = self.detail().await;
        let repository = repo::get(&self.harness.context, &self.repository_id)
            .await
            .expect("read the repository");
        let base = settings::base_instructions(&self.harness.context.pool)
            .await
            .expect("read the base instructions");

        compose_prompt(&base, &detail, &repository)
    }

    /// The JSONL transcript, line by line.
    fn transcript(&self, run_id: &str) -> Vec<String> {
        std::fs::read_to_string(transcript_path(&self.paths, &self.task_id, run_id))
            .expect("the transcript is on disk")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect()
    }
}
