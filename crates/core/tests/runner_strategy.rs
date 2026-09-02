//! The strategy run: a planner that decides, a write-back that lands, and an
//! implementation run spawned with what it chose (ADR-0016, ADR-0012,
//! seam-contract D17, task 020).
//!
//! # The planner actually calls back, over HTTP, through the real server
//!
//! Every other way of writing these tests is a test of a stub. `resolve` asks
//! one question after the planner exits — did `set_task_strategy` write? — and
//! it asks it of the database, never of what the run printed. So a stand-in that
//! only replays a recorded stream is a planner that produced *nothing*, and a
//! test built on one would pass while asserting the fallback path forever.
//!
//! The stand-in therefore reads its own `--mcp-config` out of `argv`, extracts
//! the scoped URL, and POSTs one JSON-RPC `tools/call` at a real bound
//! [`mcp::build`] server sharing this test's pool. That is the whole seam task
//! 020 introduced, exercised end to end: the token is minted by the runner, the
//! URL travels in the argument vector, the router resolves it to one task, and
//! the handler writes the row the implementation run is then spawned from.
//!
//! # Why the exact argument vectors are the assertions that matter
//!
//! The first real planner run spawned with `acceptEdits` and no
//! `--allowedTools`. `acceptEdits` auto-approves file *edits* and not MCP tool
//! calls, so the one call the planner exists to make was refused every time. The
//! process spawned, the stream parsed, the run classified as a success — and
//! every planned task silently fell back to its default. Nothing but an
//! exact-argv assertion would have caught it, so both vectors are pinned here in
//! full, in order, rather than probed for the flags somebody remembered.
//!
//! # No `sleep`, and one binary for two spawns
//!
//! The planner and the implementation run are the same `program`, so the
//! stand-in numbers its invocations and branches on `--mcp-config` — the flag
//! only a planner is given. Cancellation waits on the tail channel (D14), which
//! is the run reporting itself; the `timeout` wrappers are failure bounds, not
//! waits.

use std::path::{Path, PathBuf};
use std::time::Duration;

use pretty_assertions::assert_eq;
use rimaia_core::db::settings;
use rimaia_core::db::{
    BoardColumn, Repository, Run, RunState, RunStatus, StrategyMode, StrategySource, Task,
};
use rimaia_core::mcp::{self, McpHandle, RunHandles, Tool, MCP_SERVER_NAME};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::events::{transcript_path, RunTail};
use rimaia_core::runner::process::DEFAULT_DISALLOWED_TOOLS;
use rimaia_core::runner::prompt::{
    compose_prompt, compose_strategy_prompt, compose_strategy_system_append, compose_system_append,
    StrategyGuidance, SET_TASK_STRATEGY_TOOL,
};
use rimaia_core::runner::{run_task, CancelSignal, RunRequest, RunTrigger, RunnerConfig};
use rimaia_core::strategy::{self, StrategyDefaults};
use rimaia_core::tasks::{
    self, NewTask, Patch, StrategyPlan, StrategyPlanStatus, StrategyWorkflow, TaskDetail, TaskPatch,
};
use rimaia_core::testing::fixtures::{fixture_lines, fixture_path};
use rimaia_core::testing::{self, TempRepo, TestContext};
use rimaia_core::{AppPaths, ErrorCode};
use tempfile::TempDir;
use tokio::sync::broadcast::Receiver;

/// A ceiling on any single process test. Long enough that a slow machine never
/// trips it, short enough that a supervision bug fails rather than hangs.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

const TASK_TITLE: &str = "Add truncate_slug";
const PLAN: &str = "1. Add the function\n2. Test it";

/// What the recorded planner proposes, and therefore what the implementation
/// run must be spawned with. `strategy-proposal.jsonl` is the transcript of the
/// real run that chose exactly this pair.
const PROPOSED_MODEL: &str = "sonnet";
const PROPOSED_EFFORT: &str = "high";

/// A repository default that is nobody's answer, deliberately different from the
/// pair above. A planner that wrote nothing falls back to it, and a planner that
/// wrote something overrides it — one fixture distinguishes both directions.
const REPOSITORY_MODEL: &str = "opus";
const REPOSITORY_EFFORT: &str = "medium";

/// The planner's own budget, from `Catalogue::default`.
const PLANNER_MODEL: &str = "haiku";
const PLANNER_EFFORT: &str = "low";
const PLANNER_MAX_TURNS: &str = "6";

/// Tools a planner is denied on top of the operator's own blocklist.
const PLANNER_DENIED_TOOLS: [&str; 4] = ["Write", "Edit", "NotebookEdit", "Bash"];

// ---------------------------------------------------------------------------
// A planned task, planned
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_planned_task_spawns_its_planner_with_the_flags_that_let_it_answer() {
    // ADR-0012's 2026-08-28 amendment, as one vector. Every element is a
    // decision that ADR or ADR-0004 took: the narrow permission mode, the one
    // tool named so that mode can still answer, everything that writes denied,
    // isolation forced whatever the operator configured, and a turn budget.
    let fixture = StrategyFixture::planned().await;
    let cli = FakeCli::writing_back(&fixture.task_id);

    fixture.run(&cli).await.expect("the run completes");

    cli.assert_the_write_back_was_served();
    let argv = cli.argv(1);
    let session_id = fixture
        .recorded_plan()
        .await
        .run
        .expect("the planner's own accounting is stamped onto the proposal")
        .session_id
        .expect("a session id, minted before the spawn");

    // The scoped config is a fresh token per run, so it is spliced in rather
    // than guessed — and then read back on its own terms below.
    let mcp_config = value_after(&argv, "--mcp-config");
    let mut expected = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--session-id".to_string(),
        session_id,
        "--permission-mode".to_string(),
        "acceptEdits".to_string(),
        "--strict-mcp-config".to_string(),
        "--setting-sources".to_string(),
        "project,local".to_string(),
        "--append-system-prompt".to_string(),
        compose_strategy_system_append(&fixture.task_id, SET_TASK_STRATEGY_TOOL),
        "--model".to_string(),
        PLANNER_MODEL.to_string(),
        "--effort".to_string(),
        PLANNER_EFFORT.to_string(),
        "--allowedTools".to_string(),
        SET_TASK_STRATEGY_TOOL.to_string(),
        "--disallowedTools".to_string(),
    ];
    expected.extend(DEFAULT_DISALLOWED_TOOLS.map(str::to_string));
    expected.extend(PLANNER_DENIED_TOOLS.map(str::to_string));
    expected.extend([
        "--mcp-config".to_string(),
        mcp_config.clone(),
        "--max-turns".to_string(),
        PLANNER_MAX_TURNS.to_string(),
    ]);

    assert_eq!(argv, expected);

    // And the one element that could not be written out: seam-contract D17.4's
    // inline JSON, naming the run-scoped route on the address this server bound.
    let config: serde_json::Value =
        serde_json::from_str(&mcp_config).expect("--mcp-config is inline JSON, not a file path");
    let url = config["mcpServers"]["rimaia"]["url"]
        .as_str()
        .expect("the config names a url");
    assert_eq!(
        url,
        format!(
            "{endpoint}/mcp/run/{token}",
            endpoint = fixture.handles.endpoint().expect("a bound endpoint"),
            token = url.rsplit('/').next().expect("a token segment"),
        ),
        "the planner is handed the run-scoped route, not the operator's /mcp",
    );
}

#[tokio::test]
async fn the_implementation_run_spawns_with_exactly_the_model_and_effort_the_planner_chose() {
    // Task 020's acceptance criterion 2, end to end and unattended: the strategy
    // run completes, writes a strategy back over MCP, and the run that follows
    // uses it. The repository default is set to a *different* pair, so this
    // cannot pass by the fallback happening to agree.
    let fixture = StrategyFixture::planned().await;
    let cli = FakeCli::writing_back(&fixture.task_id);

    let run = fixture.run(&cli).await.expect("the run completes");

    assert_eq!(
        cli.spawns(),
        2,
        "a planner, and then the implementation run"
    );

    let detail = fixture.detail().await;
    let mut expected = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--session-id".to_string(),
        run.session_id.clone(),
        "--permission-mode".to_string(),
        "bypassPermissions".to_string(),
        "--append-system-prompt".to_string(),
        compose_system_append(&detail, &fixture.repository().await),
        "--model".to_string(),
        PROPOSED_MODEL.to_string(),
        "--effort".to_string(),
        PROPOSED_EFFORT.to_string(),
        "--disallowedTools".to_string(),
    ];
    expected.extend(DEFAULT_DISALLOWED_TOOLS.map(str::to_string));
    // Rimaia's own tools, denied whatever the operator's blocklist says. The
    // run inherits the operator's Claude Code config by default (ADR-0004),
    // which registers the *unscoped* `/mcp`, and `bypassPermissions`
    // auto-approves MCP calls — so without this an implementation run would
    // hold every tool `RunScope` refuses a run, including `move_task` and the
    // ADR-0021 configuration tools.
    expected.push(format!("mcp__{MCP_SERVER_NAME}"));
    expected.extend(
        Tool::ALL
            .iter()
            .map(|tool| format!("mcp__{MCP_SERVER_NAME}__{}", tool.as_str())),
    );

    assert_eq!(cli.argv(2), expected);
    assert!(
        !cli.argv(2).iter().any(|arg| arg == "--mcp-config"),
        "an implementation run reaches Rimaia through nothing",
    );

    // The columns the flags were read off, and the envelope they came in.
    assert_eq!(detail.task.model.as_deref(), Some(PROPOSED_MODEL));
    assert_eq!(detail.task.effort.as_deref(), Some(PROPOSED_EFFORT));
    assert_eq!(detail.task.strategy_source, Some(StrategySource::Planner));
    assert_eq!(detail.effective_model.as_deref(), Some(PROPOSED_MODEL));

    let plan = fixture.recorded_plan().await;
    assert_eq!(plan.status, StrategyPlanStatus::Proposed);
    assert_eq!(plan.model.as_deref(), Some(PROPOSED_MODEL));
    assert_eq!(plan.workflow, Some(StrategyWorkflow::MultiAgent));
    assert_eq!(
        plan.rationale.as_deref(),
        Some("The plan names a migration and a command surface."),
    );

    // The planner's own accounting, off the recording: a strategy run has no
    // `runs` row, so the envelope is the only place these numbers can live.
    let planner_run = plan.run.expect("the planner records what it cost");
    assert_eq!(planner_run.num_turns, Some(4));
    assert_eq!(planner_run.cost_usd, Some(0.031_041));
    assert_eq!(planner_run.error, None);

    // And the card ends where every successful run leaves it.
    assert_eq!(detail.task.column, BoardColumn::InReview);
    assert_eq!(detail.task.run_state, RunState::Idle);
}

#[tokio::test]
async fn each_run_is_sent_its_own_prompt_and_the_proposal_reaches_the_implementation_one() {
    // The planner is judging the plan; the implementation run is carrying it
    // out. They are different compositions, and ADR-0009's amendment is explicit
    // that base instructions — commit, test, open a pull request — must not
    // reach a planner. Exact strings, as the prompt rules require.
    let fixture = StrategyFixture::planned().await;
    let cli = FakeCli::writing_back(&fixture.task_id);

    fixture.run(&cli).await.expect("the run completes");

    // Composed from the task as it stands now, which is the same string it was
    // then: `task_context` renders title, repository, branch, base ref and
    // links, and the planner changed none of them.
    let detail = fixture.detail().await;
    let repository = fixture.repository().await;
    let catalogue = strategy::catalogue::catalogue(&fixture.harness.context.pool)
        .await
        .expect("the catalogue");

    assert_eq!(
        cli.stdin(1),
        compose_strategy_prompt(&detail, &repository, &catalogue)
    );
    let base = settings::base_instructions(&fixture.harness.context.pool)
        .await
        .expect("the base instructions");
    assert!(!base.trim().is_empty(), "an empty template proves nothing");
    assert!(
        !cli.stdin(1).contains(base.trim()),
        "base instructions are implementation workflow — commit, test, open a pull request — and \
         a planner that follows them is the defect ADR-0009's amendment names",
    );
    let guidance = StrategyGuidance::for_task(&detail);
    assert!(
        guidance
            .as_ref()
            .is_some_and(|guidance| guidance.multi_agent),
        "the recorded proposal fans out, so the implementation prompt has something to say",
    );
    assert_eq!(
        cli.stdin(2),
        compose_prompt(&base, &detail, &repository, guidance.as_ref())
    );
}

#[tokio::test]
async fn a_strategy_run_opens_no_runs_row_and_borrows_the_task_s_own_worktree() {
    // Seam-contract D17.5. A `runs` row for the planner would move the card to
    // `in_review` before the work started, would make `attempt` mean "attempts,
    // and also the plannings", and would put the planner's outcome on the badge
    // D12 reads off `last_run`. Its transcript still lands on disk, because that
    // is where somebody looking at 2am will look.
    let fixture = StrategyFixture::planned().await;
    let cli = FakeCli::writing_back(&fixture.task_id);

    let run = fixture.run(&cli).await.expect("the run completes");

    assert_eq!(fixture.run_rows().await, 1, "the planner opened a row");
    assert_eq!(run.attempt, 1, "the planner consumed an attempt number");

    let detail = fixture.detail().await;
    assert_eq!(
        detail.last_run.expect("the implementation run").status,
        run.status,
        "the badge reads the implementation run, not the planner",
    );

    // One worktree and one branch for the task, shared by both spawns.
    let worktree = detail.task.worktree_path.expect("task 007 prepared one");
    assert_eq!(canonical(&cli.cwd(1)), canonical(&worktree));
    assert_eq!(canonical(&cli.cwd(2)), canonical(&worktree));
    assert_ne!(
        canonical(&cli.cwd(1)),
        canonical(&fixture.repository_path())
    );
    assert_eq!(
        fixture.branches(),
        vec![
            "main".to_string(),
            detail.task.branch.expect("a prepared branch")
        ],
        "the planner took no branch of its own",
    );

    // The transcript, beside the implementation's and named by the prefix task
    // 016's cleanup matches on.
    let transcripts = fixture.transcript_names();
    assert_eq!(transcripts.len(), 2, "{transcripts:?}");
    let planner = transcripts
        .iter()
        .find(|name| name.starts_with("strategy-"))
        .unwrap_or_else(|| panic!("no strategy transcript in {transcripts:?}"));
    assert!(
        transcripts.contains(&format!("{}.jsonl", run.id)),
        "the implementation transcript is beside it: {transcripts:?}",
    );
    assert_eq!(
        fixture.transcript_lines(planner.trim_end_matches(".jsonl")),
        fixture_lines("strategy-proposal").collect::<Vec<_>>(),
        "the planner's stream is recorded verbatim, like every other run's",
    );
}

// ---------------------------------------------------------------------------
// When the planner does not answer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_failing_strategy_run_falls_back_to_the_default_and_annotates_the_card() {
    // Task 020's acceptance criterion 3, and the reason `resolve` never returns
    // `Err` for a planner failure: the queue carries on. The planner's stream
    // stops without a `result` and it exits 1 — a real failure the corpus
    // already classifies — and the implementation run is spawned regardless,
    // with the repository default rather than with half a proposal.
    let fixture = StrategyFixture::planned().await;
    let cli = FakeCli::dying_without_a_result();

    let run = fixture.run(&cli).await.expect("the queue is not blocked");

    assert_eq!(cli.spawns(), 2, "the implementation run still happened");
    assert_eq!(
        &cli.argv(2)[10..14],
        ["--model", REPOSITORY_MODEL, "--effort", REPOSITORY_EFFORT],
        "a failed planner falls back through the default chain",
    );

    let plan = fixture.recorded_plan().await;
    assert_eq!(plan.status, StrategyPlanStatus::Failed);
    assert_eq!(plan.model, None, "a failure names no model");
    assert_eq!(plan.effort, None);
    assert_eq!(
        plan.run.expect("a failure records its reason").error,
        Some(
            "the event stream ended without a result event; the process exited with code 1"
                .to_string()
        ),
        "the sentence on the card is the run's own, not a paraphrase",
    );

    let detail = fixture.detail().await;
    assert_eq!(detail.task.model, None, "the columns were cleared");
    assert_eq!(detail.task.strategy_source, Some(StrategySource::Planner));
    assert_eq!(detail.effective_model.as_deref(), Some(REPOSITORY_MODEL));

    // And the run that mattered finished normally.
    assert_eq!(run.status, RunStatus::Succeeded);
    assert_eq!(detail.task.column, BoardColumn::InReview);
}

#[tokio::test]
async fn a_planner_that_never_calls_the_tool_is_recorded_as_a_failure() {
    // The failure the first real planner run actually had, and the one no other
    // kind of test can see: the process spawns, the recorded stream parses, the
    // run classifies as a success — and nothing was written. `resolve` asks the
    // database rather than the transcript, which is why this is detectable at
    // all.
    let fixture = StrategyFixture::planned().await;
    let cli = FakeCli::silent();

    fixture.run(&cli).await.expect("the queue is not blocked");

    let plan = fixture.recorded_plan().await;
    assert_eq!(plan.status, StrategyPlanStatus::Failed);
    assert_eq!(
        plan.run.expect("a failure records its reason").error,
        Some(format!(
            "the strategy run finished without calling `{SET_TASK_STRATEGY_TOOL}`"
        )),
    );
    assert_eq!(
        &cli.argv(2)[10..14],
        ["--model", REPOSITORY_MODEL, "--effort", REPOSITORY_EFFORT],
    );
}

#[tokio::test]
async fn cancelling_during_the_strategy_run_never_spawns_the_implementation_run() {
    // Spawning the implementation run after the user has stopped its planner
    // would run the very thing they stopped. The claim goes back instead, so the
    // card is startable again rather than stuck at `running`.
    let fixture = StrategyFixture::planned().await;
    let cli = FakeCli::hanging();
    let cancel = CancelSignal::new();
    let tail = fixture.harness.context.subscribe_tail();

    let (result, ()) = tokio::time::timeout(TEST_TIMEOUT, async {
        tokio::join!(
            fixture.run_with(&cli, &cancel),
            cancel_once_the_run_is_live(tail, cancel.clone()),
        )
    })
    .await
    .expect("cancellation must not hang the runner");

    let error = result.expect_err("a cancelled planner does not produce a run");
    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        format!("\"{TASK_TITLE}\" was cancelled while its strategy was being planned"),
    );

    assert_eq!(cli.spawns(), 1, "the implementation run was spawned anyway");
    assert_eq!(fixture.run_rows().await, 0, "a `runs` row was opened");

    let task = fixture.detail().await.task;
    assert_eq!(
        task.run_state,
        RunState::Failed,
        "the claim was not released",
    );
    assert_eq!(task.strategy_plan, None, "nothing was recorded on the card");
    assert_eq!(task.column, BoardColumn::Ready);
}

// ---------------------------------------------------------------------------
// When there is nothing to plan
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_task_that_is_not_in_planned_mode_never_spawns_a_planner() {
    // The `manual` half of ADR-0016, and acceptance criterion 1: the chosen pair
    // appears verbatim in the spawned command line, with no planner in front of
    // it. `update_task` flipped the mode to `manual` when the model was set
    // (D17.6), so this is also that rule reaching a process.
    let fixture = StrategyFixture::planned().await;
    fixture
        .patch(TaskPatch {
            model: Patch::Set("haiku".to_string()),
            effort: Patch::Set("low".to_string()),
            ..TaskPatch::default()
        })
        .await;
    assert_eq!(
        fixture.detail().await.task.strategy_mode,
        StrategyMode::Manual
    );
    let cli = FakeCli::writing_back(&fixture.task_id);

    fixture.run(&cli).await.expect("the run completes");

    assert_eq!(
        cli.spawns(),
        1,
        "something planned a task that was not planned"
    );
    let argv = cli.argv(1);
    assert!(
        !argv.iter().any(|arg| arg == "--mcp-config"),
        "only a planner is handed a scoped handle: {argv:?}",
    );
    assert_eq!(&argv[10..14], ["--model", "haiku", "--effort", "low"]);
    assert_eq!(fixture.detail().await.task.strategy_plan, None);
}

#[tokio::test]
async fn a_task_that_already_carries_a_proposal_is_not_planned_again() {
    // Seam-contract D17.8's re-plan guard, reaching a process. Without it a
    // `planned` task is replanned on every queue pass — paying for the same
    // decision all night, which is the shape of overnight loss this product
    // exists to prevent. `clear_task_strategy` is the only thing that lifts it,
    // which is why "Re-plan" is a button and not a side effect.
    let fixture = StrategyFixture::planned().await;
    tasks::strategy::set_task_strategy(
        &fixture.harness.context,
        &fixture.task_id,
        StrategyPlan::proposed(Some("haiku".to_string()), Some("low".to_string())),
        StrategySource::Planner,
    )
    .await
    .expect("last night's planner");
    let cli = FakeCli::writing_back(&fixture.task_id);

    fixture.run(&cli).await.expect("the run completes");

    assert_eq!(cli.spawns(), 1, "the task was planned a second time");
    assert_eq!(
        &cli.argv(1)[10..14],
        ["--model", "haiku", "--effort", "low"]
    );
    assert_eq!(
        fixture.recorded_plan().await.model.as_deref(),
        Some("haiku"),
        "the recorded proposal was overwritten",
    );
}

#[tokio::test]
async fn a_planned_task_with_no_mcp_server_listening_falls_back_rather_than_planning_blind() {
    // Seam-contract D16.7 makes a busy MCP port non-fatal to startup, so a run
    // can reach the planner with nothing listening. Spawning one anyway would
    // burn a run to produce nothing, so it is a failure with an address instead.
    let fixture = StrategyFixture::planned().await;
    let cli = FakeCli::writing_back(&fixture.task_id);

    fixture
        .run_with_config(
            &RunnerConfig {
                program: cli.program(),
                // The default table has no endpoint, which is what "nothing is
                // listening" looks like from the runner.
                run_handles: RunHandles::default(),
                ..RunnerConfig::default()
            },
            &CancelSignal::new(),
        )
        .await
        .expect("the queue is not blocked");

    assert_eq!(
        cli.spawns(),
        1,
        "a planner with no way to answer was spawned"
    );
    assert_eq!(
        fixture
            .recorded_plan()
            .await
            .run
            .expect("a failure records its reason")
            .error,
        Some(
            "the strategy run needs Rimaia's MCP server, which is not listening (see Settings → \
             MCP)"
                .to_string()
        ),
    );
}

// ---------------------------------------------------------------------------
// A stand-in for the CLI that can answer
// ---------------------------------------------------------------------------

/// A shell script that behaves like `claude -p --output-format stream-json`
/// enough to supervise, numbers its invocations, and — when handed a
/// `--mcp-config` — calls back through it the way a planner does.
///
/// Numbering matters because a planned task spawns this binary twice and the two
/// vectors are the whole point. Branching on `--mcp-config` rather than on the
/// invocation number is deliberate: it is the flag that actually distinguishes
/// the two runs, so a regression that stopped handing the planner its handle
/// would make this stand-in behave like an implementation run, which is exactly
/// what the assertions should then see.
struct FakeCli {
    /// Held for its `Drop`; every path below points inside it.
    dir: TempDir,
}

impl FakeCli {
    /// A planner that calls `set_task_strategy` over its own scoped handle, then
    /// replays the recording of the run that really did that.
    ///
    /// The arguments are the recording's own proposal, so what this writes and
    /// what the transcript says are one answer rather than two.
    fn writing_back(task_id: &str) -> Self {
        let call = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "set_task_strategy",
                "arguments": {
                    "task_id": task_id,
                    "model": PROPOSED_MODEL,
                    "effort": PROPOSED_EFFORT,
                    "workflow": "multi_agent",
                    "phases": [
                        { "name": "Schema", "agents": 1, "summary": "the migration" },
                        { "name": "Surface", "agents": 2, "summary": "commands and tools" },
                    ],
                    "rationale": "The plan names a migration and a command surface.",
                },
            },
        })
        .to_string();
        assert!(
            !call.contains('\''),
            "the call is embedded in a single-quoted shell word: {call}",
        );

        Self::with_planner(&format!(
            "url=$(printf '%s' \"$config\" | sed -e 's/.*\"url\":\"//' -e 's/\".*//')\n\
             curl -sS -X POST \"$url\" \
               -H 'accept: application/json, text/event-stream' \
               -H 'content-type: application/json' \
               -d '{call}' > \"$dir/write-back\" 2> \"$dir/write-back.err\"\n\
             cat '{fixture}'\n\
             exit 0\n",
            fixture = fixture_path("strategy-proposal").display(),
        ))
    }

    /// A planner that runs, prints its whole recorded stream, exits cleanly —
    /// and never calls the tool.
    fn silent() -> Self {
        Self::with_planner(&format!(
            "cat '{fixture}'\nexit 0\n",
            fixture = fixture_path("strategy-proposal").display(),
        ))
    }

    /// A planner whose stream stops without a `result` and which exits 1.
    fn dying_without_a_result() -> Self {
        let cli = Self::empty();
        let head = cli.path("planner-head.jsonl");
        let lines: Vec<String> = fixture_lines("strategy-proposal").take(20).collect();
        std::fs::write(&head, lines.join("\n") + "\n").expect("write the truncated recording");
        cli.write_script(&format!("cat '{}'\nexit 1\n", head.display()));
        cli
    }

    /// A planner that replays enough of its recording to be visibly in flight —
    /// the first `assistant` event is what publishes a tail snapshot — and then
    /// waits to be stopped.
    fn hanging() -> Self {
        let cli = Self::empty();
        let head = cli.path("planner-head.jsonl");
        let lines: Vec<String> = fixture_lines("strategy-proposal").take(41).collect();
        std::fs::write(&head, lines.join("\n") + "\n").expect("write the head of the recording");
        cli.write_script(&format!("cat '{}'\nsleep 300\n", head.display()));
        cli
    }

    fn with_planner(body: &str) -> Self {
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

    /// A shebang script, executed directly rather than through `sh -c` — the
    /// same rule the production code follows, for the same reason: these paths
    /// contain spaces.
    ///
    /// `--version` short-circuits, because the runner probes for the
    /// prerequisite before it starts anything.
    fn write_script(&self, planner_body: &str) {
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = '--version' ]; then echo '2.1.234 (Claude Code)'; exit 0; fi\n\
             dir='{dir}'\n\
             n=$(cat \"$dir/count\" 2>/dev/null || echo 0)\n\
             n=$((n + 1))\n\
             printf '%s' \"$n\" > \"$dir/count\"\n\
             printf '%s\\0' \"$@\" > \"$dir/argv-$n\"\n\
             pwd > \"$dir/cwd-$n\"\n\
             cat > \"$dir/stdin-$n\"\n\
             config=''\n\
             prev=''\n\
             for arg in \"$@\"; do\n\
             \x20 if [ \"$prev\" = '--mcp-config' ]; then config=\"$arg\"; fi\n\
             \x20 prev=\"$arg\"\n\
             done\n\
             if [ -z \"$config\" ]; then\n\
             \x20 cat '{success}'\n\
             \x20 exit 0\n\
             fi\n\
             {planner_body}",
            dir = self.dir.path().display(),
            success = fixture_path("success").display(),
        );

        let program = self.program();
        std::fs::write(&program, script).expect("write the stand-in CLI");
        make_executable(&program);
    }

    /// How many times the binary was spawned for a run — the probe does not
    /// count, because `--version` short-circuits before the counter.
    fn spawns(&self) -> usize {
        std::fs::read_to_string(self.path("count"))
            .map(|count| count.trim().parse().expect("the counter is a number"))
            .unwrap_or(0)
    }

    /// The `n`th spawn's own `argv`, NUL-separated on the way out because the
    /// composed system prompt contains newlines.
    fn argv(&self, n: usize) -> Vec<String> {
        let raw = std::fs::read(self.path(&format!("argv-{n}")))
            .unwrap_or_else(|_| panic!("spawn {n} never happened"));
        raw.split(|byte| *byte == 0)
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .filter(|part| !part.is_empty())
            .collect()
    }

    fn cwd(&self, n: usize) -> String {
        std::fs::read_to_string(self.path(&format!("cwd-{n}")))
            .unwrap_or_else(|_| panic!("spawn {n} never happened"))
            .trim()
            .to_string()
    }

    fn stdin(&self, n: usize) -> String {
        std::fs::read_to_string(self.path(&format!("stdin-{n}")))
            .unwrap_or_else(|_| panic!("spawn {n} never happened"))
    }

    /// That the planner's own call reached Rimaia and was served.
    ///
    /// Asserted separately from anything the runner then did with it, because
    /// the two failures look identical from the card: a tool call the server
    /// refused and a `curl` that was never able to send one both leave the task
    /// with a `failed` envelope reading "finished without calling the tool".
    /// This one says which.
    fn assert_the_write_back_was_served(&self) {
        let body = std::fs::read_to_string(self.path("write-back")).unwrap_or_default();
        assert!(
            !body.trim().is_empty(),
            "the planner's write-back sent nothing. curl said: {}",
            std::fs::read_to_string(self.path("write-back.err")).unwrap_or_default(),
        );

        let answer: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|_| panic!("a JSON-RPC answer: {body}"));
        assert_eq!(answer["error"], serde_json::Value::Null, "{body}");
        assert!(
            answer["result"].is_object(),
            "a JSON-RPC answer with no result is not one the planner was served: {body}",
        );
        assert_ne!(
            answer["result"]["isError"],
            serde_json::Value::Bool(true),
            "the server refused the planner's own write-back: {body}",
        );
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("make the stand-in executable");
}

/// The argument following `flag` in a recorded vector.
fn value_after(argv: &[String], flag: &str) -> String {
    argv.iter()
        .position(|arg| arg == flag)
        .and_then(|at| argv.get(at + 1))
        .unwrap_or_else(|| panic!("{flag} is not in {argv:?}"))
        .clone()
}

fn canonical(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

/// Cancels as soon as the run says it is in flight.
///
/// The tail channel is the run reporting its own progress (seam-contract D14),
/// so this needs no sleep and no polling — and a strategy run publishes on it
/// like any other, which is why the hanging stand-in replays as far as its first
/// `assistant` event before waiting.
async fn cancel_once_the_run_is_live(mut tail: Receiver<RunTail>, cancel: CancelSignal) {
    let _ = tokio::time::timeout(TEST_TIMEOUT, tail.recv()).await;
    cancel.cancel();
}

// ---------------------------------------------------------------------------
// A planned task, a real repository, and a server the planner can reach
// ---------------------------------------------------------------------------

struct StrategyFixture {
    harness: TestContext,
    /// Held for their `Drop`; the paths below point inside them.
    repository: TempRepo,
    #[allow(dead_code)]
    data: TempDir,
    paths: AppPaths,
    repository_id: String,
    task_id: String,
    handles: RunHandles,
    /// Held so the server keeps serving for the life of the fixture.
    mcp: McpHandle,
}

impl StrategyFixture {
    /// A real git repository, opted in to unattended runs, holding one `ready`
    /// task in `planned` mode — with a repository default that is nobody's
    /// answer, so a fallback and a proposal are never the same string.
    async fn planned() -> Self {
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

        strategy::settings::set_repository_default(
            &harness.context,
            &registered.id,
            &StrategyDefaults {
                mode: StrategyMode::Default,
                model: Some(REPOSITORY_MODEL.to_string()),
                effort: Some(REPOSITORY_EFFORT.to_string()),
            },
        )
        .await
        .expect("a repository default to fall back to");

        let task = tasks::create_task(
            &harness.context,
            NewTask {
                repository_id: registered.id.clone(),
                title: TASK_TITLE.to_string(),
                plan: Some(PLAN.to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a ready task");

        tasks::update_task(
            &harness.context,
            &task.id,
            TaskPatch {
                strategy_mode: Some(StrategyMode::Planned),
                ..TaskPatch::default()
            },
        )
        .await
        .expect("ADR-0016's planned mode");

        // A real bound server on an OS-chosen port, sharing the handles the
        // runner mints tokens in — the way the shell wires them in `setup()`.
        let handles = RunHandles::default();
        let (mcp, task_handle) = mcp::build(
            harness.context.clone(),
            0,
            handles.clone(),
            testing::doctor::environment(),
        )
        .await;
        tokio::spawn(task_handle.run());
        assert!(
            handles.endpoint().is_some(),
            "the planner has nothing to call back through",
        );

        Self {
            harness,
            repository,
            data,
            paths,
            repository_id: registered.id,
            task_id: task.id,
            handles,
            mcp,
        }
    }

    fn config(&self, cli: &FakeCli) -> RunnerConfig {
        RunnerConfig {
            program: cli.program(),
            run_handles: self.handles.clone(),
            ..RunnerConfig::default()
        }
    }

    /// A queued run — ADR-0012's unattended posture, which is the mode
    /// `success.jsonl`'s own `init` echoes back.
    async fn run(&self, cli: &FakeCli) -> rimaia_core::Result<Run> {
        self.run_with(cli, &CancelSignal::new()).await
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
                task_id: self.task_id.clone(),
                trigger: RunTrigger::Queued,
                cancel: cancel.clone(),
                in_flight: None,
            },
        )
        .await
    }

    async fn patch(&self, patch: TaskPatch) -> Task {
        tasks::update_task(&self.harness.context, &self.task_id, patch)
            .await
            .expect("patch the task")
    }

    async fn detail(&self) -> TaskDetail {
        tasks::get_task(&self.harness.context, &self.task_id)
            .await
            .expect("read the task")
    }

    async fn repository(&self) -> Repository {
        repo::get(&self.harness.context, &self.repository_id)
            .await
            .expect("read the repository")
    }

    /// The envelope on the card, parsed the way the panel parses it.
    async fn recorded_plan(&self) -> StrategyPlan {
        let task = self.detail().await.task;
        StrategyPlan::from_stored(task.strategy_plan.as_deref())
            .expect("the card carries a strategy envelope")
    }

    /// How many `runs` rows this task has. A plain query rather than a macro:
    /// the offline cache is regenerated from production queries, and a count in
    /// a test has no business being in it.
    async fn run_rows(&self) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM runs WHERE task_id = ?1")
            .bind(&self.task_id)
            .fetch_one(&self.harness.context.pool)
            .await
            .expect("count the runs")
    }

    fn repository_path(&self) -> String {
        self.repository.path().to_string_lossy().into_owned()
    }

    /// Every branch in the real repository, in name order — real git, never a
    /// mock.
    fn branches(&self) -> Vec<String> {
        let output = std::process::Command::new("git")
            .args(["branch", "--format=%(refname:short)"])
            .current_dir(self.repository.path())
            .output()
            .expect("git must be runnable");
        let mut branches: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        branches.sort();
        branches
    }

    /// The file names in this task's run directory.
    fn transcript_names(&self) -> Vec<String> {
        let dir = transcript_path(&self.paths, &self.task_id, "ignored")
            .parent()
            .expect("a run directory")
            .to_path_buf();
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("the run directory exists")
            .map(|entry| {
                entry
                    .expect("a readable entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.ends_with(".jsonl"))
            .collect();
        names.sort();
        names
    }

    fn transcript_lines(&self, run_id: &str) -> Vec<String> {
        std::fs::read_to_string(transcript_path(&self.paths, &self.task_id, run_id))
            .expect("the transcript is on disk")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect()
    }
}

impl Drop for StrategyFixture {
    fn drop(&mut self) {
        self.mcp.shutdown();
    }
}
