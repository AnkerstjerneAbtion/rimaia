//! The review-and-fix loop, end to end (ADR-0017, task 021).
//!
//! The CLI is `testing::FakeCli` replaying recorded streams; a review or fix
//! that writes back does so over the real run-scoped HTTP route, as the
//! planner's write-back does in `runner_strategy.rs`. Git runs against real
//! repositories in temporary directories, and the clock is the harness's.
//! Nothing sleeps: the `timeout`s are failure bounds, not waits, and nothing
//! here spawns anything but the stand-in, so the engine never spends.
//!
//! What a decision is, row by row, is `review_loop::decide`'s unit tests; what
//! a prompt says is `tests/prompt.rs`. This file is the loop as a run sees it:
//! the phases, the claim they share, the rows they leave, the argv they spawn
//! with, and where the task lands.

#![cfg(unix)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::TimeDelta;
use pretty_assertions::assert_eq;
use rimaia_core::board::OwnerPresence;
use rimaia_core::board::{
    BoardFuture, BoardPort, Claim, ClaimTarget, FinishReceipt, FinishRun, Heartbeat, LeaseRef,
    PreviewOf, RunContext, StartRun, TranscriptAck, TranscriptChunk,
};
use rimaia_core::credentials::{CredentialAccess, CredentialStore, Secret};
use rimaia_core::db::settings::{self, RunEnvironment};
use rimaia_core::db::{BoardColumn, ExitClass, Run, RunKind, RunState, RunStatus};
use rimaia_core::mcp::requests::{
    SetRepositoryReviewConfigRequest, SetReviewSettingsRequest, SetTaskReviewRequest,
};
use rimaia_core::mcp::{self, McpHandle, RimaiaServer, RunHandles, Tool, MCP_SERVER_NAME};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::review;
use rimaia_core::review::findings::{FindingStatus, NewReviewFinding, ReviewFinding};
use rimaia_core::review_loop::{self, config as review_config, UnreviewedReason, Verdict};
use rimaia_core::runner::events::{stderr_path, transcript_path, RunTail};
use rimaia_core::runner::limits::{DISALLOWED_TOOLS, MAX_TURNS};
use rimaia_core::runner::outcome::{start_run, NewRun};
use rimaia_core::runner::process::DEFAULT_DISALLOWED_TOOLS;
use rimaia_core::runner::prompt::{
    compose_fix_prompt, compose_fix_resume, compose_review_resume, compose_review_system_append,
};
use rimaia_core::runner::provider::{ClaudeProvider, ProviderId};
use rimaia_core::runner::{
    claim_manual_start, run_task, CancelSignal, ManualStart, RunRequest, RunTrigger, RunnerConfig,
};
use rimaia_core::scheduler::{self, InFlight, SlotOwner};
use rimaia_core::startup;
use rimaia_core::tasks::strategy::StrategyPlan;
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::board::claim_run;
use rimaia_core::testing::credentials::MemoryStore;
use rimaia_core::testing::fixtures::path_for;
use rimaia_core::testing::provider::LedgerWithoutResume;
use rimaia_core::testing::{self, FakeCli, TempRepo, TestContext, WorktreeAction};
use rimaia_core::{AppPaths, ChangeEvent, Clock, ErrorCode, ServiceContext};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};
use tempfile::TempDir;

/// A failure bound for anything that waits on a child or on the queue.
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The two tools, spelled the way Claude Code is told them: at the run-scoped
/// handle's own server (seam-contract D30 point 1).
const RECORD_TOOL: &str = "mcp__rimaia-run__record_review_findings";
const RESOLVE_TOOL: &str = "mcp__rimaia-run__resolve_review_finding";
/// The same tool in the Ledger provider's own spelling.
const LEDGER_RESOLVE_TOOL: &str = "rimaia-run.resolve_review_finding";

/// A token nothing else in the suite could produce.
const SENTINEL: &str = "ghp_rimaia_loop_sentinel_0123456789abcdef";

const TITLE: &str = "Alpha";

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
    let team = fixture.harness.solo.team_id.clone();
    assert_eq!(
        drain(&mut fixture.harness),
        vec![
            // `worktree::prepare` records the worktree in this machine's
            // store, then the branch it created on the board (task 066).
            ChangeEvent::tasks(team.clone(), [task.clone()]),
            ChangeEvent::tasks(team.clone(), [task.clone()]),
            // `start_run`.
            ChangeEvent::runs(team.clone(), [run.id.clone()]),
            ChangeEvent::tasks(team.clone(), [task.clone()]),
            // `finish_run`: the row, then the task it lands, its column,
            // run state and lease in one transaction (task 043).
            ChangeEvent::runs(team.clone(), [run.id.clone()]),
            ChangeEvent::tasks(team.clone(), [task.clone()]),
            ChangeEvent::tasks(team.clone(), [task.clone()]),
        ],
    );

    let detail = fixture.detail().await;
    assert_eq!(detail.task.column, BoardColumn::InReview);
    assert_eq!(detail.task.run_state, RunState::Idle);
    assert_eq!(detail.review_loop, None);
}

#[tokio::test]
async fn the_loop_is_off_on_a_fresh_database() {
    let fixture = Fixture::new().await;

    let settings = review_config::get_review_settings(fixture.ctx())
        .await
        .expect("read the global settings");
    assert_eq!(settings.instructions, "");
    assert_eq!(settings.config, review_config::ReviewConfig::default());

    fixture.run().await;

    assert_eq!(fixture.cli.started(), vec![fixture.task_id.clone()]);
    assert_eq!(fixture.kinds().await, vec![RunKind::Implementation]);
    assert_eq!(fixture.detail().await.review_loop, None);
}

#[tokio::test]
async fn saving_the_global_settings_commits_both_keys_and_announces_once() {
    let mut fixture = Fixture::new().await;
    drain(&mut fixture.harness);

    fixture.enable(json!({})).await;

    let settings = review_config::get_review_settings(fixture.ctx())
        .await
        .expect("read the global settings");
    assert_eq!(settings.instructions, "Run /review.");
    assert_eq!(
        settings.config.enabled,
        Some(review_config::ReviewEnabled::OnCostAcknowledged),
        "the configuration saved with them"
    );
    assert_eq!(
        drain(&mut fixture.harness),
        vec![ChangeEvent::settings(fixture.harness.solo.team_id.clone())],
        "the instructions and the configuration are one save"
    );
}

#[tokio::test]
async fn enabling_the_loop_requires_the_cost_acknowledged_spelling() {
    let fixture = Fixture::new().await;
    let provider = ClaudeProvider;
    let operator = fixture.operator();

    for on in [json!(true), json!("on")] {
        let config = json!({ "enabled": on });

        // The service, at all three levels.
        for refused in [
            review_config::set_review_settings(fixture.ctx(), &provider, "", config.clone())
                .await
                .err(),
            review_config::set_repository_review_config(
                fixture.ctx(),
                &provider,
                &fixture.repository_id,
                config.clone(),
            )
            .await
            .err(),
            review_config::set_task_review(
                fixture.ctx(),
                &provider,
                &fixture.task_id,
                None,
                config.clone(),
            )
            .await
            .err(),
        ] {
            let refused = refused.expect("only the acknowledgement turns the loop on");
            assert_eq!(refused.code(), ErrorCode::Invalid);
            assert!(
                refused.to_string().contains("on_cost_acknowledged"),
                "{refused}"
            );
        }

        // The tools: the same refusal, because they hand the raw value to
        // the same functions (the commands do too; see `commands::review`).
        let service =
            review_config::set_review_settings(fixture.ctx(), &provider, "", config.clone())
                .await
                .expect_err("refused");
        for over_mcp in [
            operator
                .set_review_settings(Parameters(request::<SetReviewSettingsRequest>(
                    json!({ "instructions": "", "config": config }),
                )))
                .await
                .err(),
            operator
                .set_repository_review_config(Parameters(
                    request::<SetRepositoryReviewConfigRequest>(json!({
                        "repository_id": fixture.repository_id,
                        "config": config,
                    })),
                ))
                .await
                .err(),
            operator
                .set_task_review(Parameters(request::<SetTaskReviewRequest>(json!({
                    "task_id": fixture.task_id,
                    "config": config,
                }))))
                .await
                .err(),
        ] {
            let over_mcp = over_mcp.expect("refused over MCP");
            assert_eq!(over_mcp.0.to_string(), service.to_string());
            assert_eq!(over_mcp.0.code(), ErrorCode::Invalid);
        }
    }

    // Nothing was written anywhere, and the loop is still off.
    let detail = fixture.detail().await;
    assert_eq!(detail.review_config, review_config::ReviewConfig::default());
    assert_eq!(
        review_config::get_review_settings(fixture.ctx())
            .await
            .expect("read")
            .config,
        review_config::ReviewConfig::default()
    );
}

#[tokio::test]
async fn a_hand_edited_true_in_stored_config_reads_as_off() {
    // D17.2's tolerance: a value that does not parse reads as nothing set,
    // which is off. A typo must not enable a spend.
    let fixture = Fixture::new().await;
    rimaia_core::testing::settings::set(
        fixture.ctx(),
        review_config::REVIEW_CONFIG,
        r#"{"enabled": true}"#,
    )
    .await
    .expect("hand-edit the stored value");

    fixture.run().await;

    assert_eq!(fixture.kinds().await, vec![RunKind::Implementation]);
    let detail = fixture.detail().await;
    assert_eq!(detail.task.column, BoardColumn::InReview);
    assert_eq!(detail.review_loop, None);
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_planted_finding_is_fixed_and_the_second_review_is_clean() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    fixture
        .cli
        .commits_on_attempt(&task, 1, "Implement Alpha", "success", 0);
    fixture.reviews_on(2, vec![high("The retry never stops")]);
    fixture.fixes_on(3, "fixed", "Bounded the retry.");
    fixture.reviews_on(4, vec![]);

    fixture.run().await;

    for attempt in [2, 3, 4] {
        assert_served(&fixture.cli.tool_answer(&task, attempt));
    }
    assert_eq!(fixture.cli.attempts(&task), 4, "exactly four spawns");
    let rows = fixture.rows().await;
    assert_eq!(
        rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
        vec![
            RunKind::Implementation,
            RunKind::Review,
            RunKind::Fix,
            RunKind::Review
        ]
    );
    assert_eq!(
        rows.iter().map(|row| row.attempt).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert!(rows.iter().all(|row| row.status == RunStatus::Succeeded));
    for row in &rows {
        assert_eq!(row.base_ref, rows[0].base_ref, "attempt {}", row.attempt);
        assert_eq!(row.base_sha, rows[0].base_sha, "attempt {}", row.attempt);
    }
    assert!(rows[0].base_sha.is_some());

    let detail = fixture.detail().await;
    assert_eq!(detail.task.column, BoardColumn::InReview);
    assert_eq!(detail.task.run_state, RunState::Idle);
    let summary = detail.review_loop.expect("the loop ran");
    assert_eq!(summary.verdict, Verdict::Clean);
    assert_eq!(summary.fixes_spent, 1);
    assert_eq!(summary.reviews, 2);

    let findings = fixture.findings().await;
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].status, FindingStatus::Fixed);
    assert_eq!(
        findings[0].resolved_by_run_id.as_deref(),
        Some(rows[2].id.as_str())
    );
    assert_eq!(
        findings[0].resolution.as_deref(),
        Some("Bounded the retry.")
    );
}

#[tokio::test]
async fn an_unfixable_finding_ends_the_loop_at_budget_with_findings_attached() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({ "max_review_loops": 2 })).await;
    let task = fixture.task_id.clone();
    for review in [2, 4, 6] {
        fixture.reviews_on(review, vec![high("The retry never stops")]);
    }
    // The fixes run and resolve nothing (attempts 3 and 5 replay `success`).

    fixture.run().await;

    assert_eq!(fixture.cli.attempts(&task), 6, "exactly six spawns");
    assert_eq!(
        fixture.kinds().await,
        vec![
            RunKind::Implementation,
            RunKind::Review,
            RunKind::Fix,
            RunKind::Review,
            RunKind::Fix,
            RunKind::Review,
        ]
    );
    let detail = fixture.detail().await;
    assert_eq!(detail.task.column, BoardColumn::InReview);
    assert_eq!(detail.task.run_state, RunState::Idle);
    let summary = detail.review_loop.expect("the loop ran");
    assert_eq!(
        summary.verdict,
        Verdict::FindingsRemain { open_blocking: 1 }
    );
    assert_eq!(summary.open_blocking, 1);
    assert_eq!(summary.fixes_spent, 2);

    let history = review_loop::history(fixture.ctx(), &task)
        .await
        .expect("read the history");
    let rounds = &history.loops[0].rounds;
    assert_eq!(rounds.len(), 3);
    assert_eq!(rounds[2].findings[0].title, "The retry never stops");
    assert_eq!(rounds[2].findings[0].status, FindingStatus::Open);
    assert!(rounds[2].fix.is_none(), "the budget was spent");
}

#[tokio::test]
async fn a_rejected_finding_raised_again_is_stored_rejected_and_not_blocking() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    fixture.reviews_on(
        2,
        vec![json!({
            "severity": "high", "title": "Unchecked unwrap", "body": "Panics on empty input.",
            "file": "README.md", "line": 1,
        })],
    );
    fixture.fixes_on(3, "rejected", "The input is never empty here.");
    fixture.reviews_on(
        4,
        vec![json!({
            "severity": "high", "title": "unchecked  UNWRAP", "body": "Still panics.",
            "file": "README.md", "line": 9,
        })],
    );

    fixture.run().await;

    let findings = fixture.findings().await;
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0].status, FindingStatus::Rejected);
    assert_eq!(
        findings[0].resolution.as_deref(),
        Some("The input is never empty here.")
    );
    assert_eq!(findings[1].status, FindingStatus::Rejected);
    assert_eq!(
        findings[1].resolution,
        Some(format!(
            "Rejected earlier as {}: The input is never empty here.",
            findings[0].id
        ))
    );

    // It started no fix: four spawns, and the loop ended clean.
    assert_eq!(fixture.cli.attempts(&task), 4);
    let summary = fixture.detail().await.review_loop.expect("the loop ran");
    assert_eq!(summary.verdict, Verdict::Clean);

    // And the second reviewer was told about it before it raised it.
    assert!(
        fixture.cli.stdin(&task, 4).contains(
            "# Findings already rejected\n\n- **Unchecked unwrap** (`README.md:1`). Rejected: The \
             input is never empty here."
        ),
        "{}",
        fixture.cli.stdin(&task, 4)
    );
}

#[tokio::test]
async fn the_fix_receives_only_the_newest_reviews_open_blocking_findings() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({ "max_review_loops": 2 })).await;
    let task = fixture.task_id.clone();
    fixture.reviews_on(2, vec![high("First review's finding")]);
    // The first fix resolves nothing, so that finding stays open in history.
    fixture.reviews_on(
        4,
        vec![
            high("Second review's finding"),
            json!({ "severity": "low", "title": "A nit", "body": "Rename it." }),
        ],
    );
    fixture.reviews_on(6, vec![]);

    fixture.run().await;

    let findings = fixture.findings().await;
    let id_of = |title: &str| {
        findings
            .iter()
            .find(|finding| finding.title == title)
            .expect("the finding")
            .id
            .clone()
    };
    let second_fix = fixture.cli.stdin(&task, 5);
    assert!(second_fix.contains(&id_of("Second review's finding")));
    assert!(!second_fix.contains(&id_of("First review's finding")));
    assert!(!second_fix.contains(&id_of("A nit")), "below the threshold");
    assert!(fixture
        .cli
        .stdin(&task, 3)
        .contains(&id_of("First review's finding")));
}

// ---------------------------------------------------------------------------
// Phases, posture and argv
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_review_phase_opens_a_fresh_session_and_never_resumes() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    fixture.reviews_on(2, vec![]);

    fixture.run().await;

    let review = fixture.cli.argv(&task, 2);
    assert!(!review.iter().any(|arg| arg == "--resume"), "{review:?}");
    let rows = fixture.rows().await;
    assert_eq!(value_after(&review, "--session-id"), rows[1].session_id);
    assert_ne!(rows[1].session_id, rows[0].session_id);
}

#[tokio::test]
async fn a_review_phase_argv_carries_rimaia_run_and_denies_file_mutation() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    fixture.reviews_on(2, vec![]);

    fixture.run().await;

    let argv = fixture.cli.argv(&task, 2);
    let rows = fixture.rows().await;
    let url = mcp_url(&argv);
    let mut expected: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--session-id",
        &rows[1].session_id,
        "--permission-mode",
        "bypassPermissions",
        "--append-system-prompt",
    ]
    .iter()
    .map(|arg| (*arg).to_string())
    .collect();
    expected.extend(lines_of(&compose_review_system_append(RECORD_TOOL)));
    expected.extend(["--allowedTools", RECORD_TOOL, "--disallowedTools"].map(str::to_string));
    expected.extend(
        DEFAULT_DISALLOWED_TOOLS
            .iter()
            .map(|rule| (*rule).to_string()),
    );
    // ADR-0012's review row: no file mutation, with the shell left alone.
    expected.extend(["Write", "Edit", "NotebookEdit"].map(str::to_string));
    expected.extend(operator_surface_denial());
    expected.extend(
        [
            "--mcp-config".to_string(),
            format!(r#"{{"mcpServers":{{"rimaia-run":{{"type":"http","url":"{url}"}}}}}}"#),
            "--max-turns".to_string(),
            "300".to_string(),
        ]
        .into_iter(),
    );
    assert_eq!(argv, expected);
    assert!(
        !argv
            .iter()
            .any(|arg| arg.starts_with("mcp__rimaia-run") && arg != RECORD_TOOL),
        "no pattern denies or allows anything else at the run's own server"
    );
}

#[tokio::test]
async fn a_review_phase_is_spawned_with_the_same_limits_as_its_implementation_phase() {
    // ADR-0028 point 2 and ADR-0032 point 5: every process a runner starts is
    // held to the stricter of the team's limits and the runner's, a review
    // phase included. The review adds only its own denial.
    let fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    fixture.reviews_on(2, vec![]);
    let team = &fixture.harness.solo.team_id;
    testing::settings::set_team(fixture.ctx(), team, MAX_TURNS, "300")
        .await
        .expect("the team's budget");
    testing::settings::set_team(fixture.ctx(), team, DISALLOWED_TOOLS, "Bash(rm:*)")
        .await
        .expect("the team's rule");
    fixture
        .machine()
        .store
        .set_setting(MAX_TURNS, "40")
        .await
        .expect("the runner's lower budget");
    fixture
        .machine()
        .store
        .set_setting(DISALLOWED_TOOLS, "Bash(curl:*)")
        .await
        .expect("the runner's added rule");

    fixture.run().await;

    let implementation = fixture.cli.argv(&task, 1);
    let review = fixture.cli.argv(&task, 2);
    assert_eq!(value_after(&implementation, "--max-turns"), "40");
    assert_eq!(value_after(&review, "--max-turns"), "40");

    let both: Vec<String> = ["Bash(rm:*)", "Bash(curl:*)"].map(str::to_string).to_vec();
    let mut expected = both.clone();
    expected.extend(operator_surface_denial());
    assert_eq!(list_after(&implementation, "--disallowedTools"), expected);

    let mut expected = both;
    expected.extend(["Write", "Edit", "NotebookEdit"].map(str::to_string));
    expected.extend(operator_surface_denial());
    assert_eq!(list_after(&review, "--disallowedTools"), expected);
}

#[tokio::test]
async fn a_fix_phase_argv_allows_only_resolve_review_finding() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    fixture.reviews_on(2, vec![high("The retry never stops")]);
    fixture.fixes_on(3, "fixed", "Bounded it.");
    fixture.reviews_on(4, vec![]);

    fixture.run().await;

    let argv = fixture.cli.argv(&task, 3);
    let rows = fixture.rows().await;
    let detail = fixture.detail().await;
    let repository = repo::get(fixture.ctx(), &fixture.repository_id)
        .await
        .expect("the repository");
    let url = mcp_url(&argv);
    let mut expected: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--session-id",
        &rows[2].session_id,
        "--permission-mode",
        "bypassPermissions",
        "--append-system-prompt",
    ]
    .iter()
    .map(|arg| (*arg).to_string())
    .collect();
    expected.extend(lines_of(
        &rimaia_core::runner::prompt::compose_system_append(&detail, &repository),
    ));
    expected.extend(["--allowedTools", RESOLVE_TOOL, "--disallowedTools"].map(str::to_string));
    expected.extend(
        DEFAULT_DISALLOWED_TOOLS
            .iter()
            .map(|rule| (*rule).to_string()),
    );
    expected.extend(operator_surface_denial());
    expected.extend([
        "--mcp-config".to_string(),
        format!(r#"{{"mcpServers":{{"rimaia-run":{{"type":"http","url":"{url}"}}}}}}"#),
        "--max-turns".to_string(),
        "300".to_string(),
    ]);
    assert_eq!(argv, expected);
}

#[tokio::test]
async fn a_fix_phase_spawn_carries_the_repository_credentials_and_strips_claude_vars() {
    // D25's variables reach every phase, and are scrubbed from what it writes
    // down; D27.5's identity strip applies too, because every phase spawns
    // through the implementation's own `execute`. Only ever *adding* names to
    // the shared environment, as `runner_process.rs` does, so a sibling test
    // can only make this stronger.
    std::env::set_var("CLAUDE_CODE_SESSION_ID", "a-parent-session");
    std::env::set_var("CLAUDECODE", "1");
    let fixture = Fixture::new().await;
    let store = MemoryStore::new();
    store
        .set(
            &fixture.repository_id,
            &Secret::new(SENTINEL).expect("a token"),
        )
        .expect("store the token");
    repo::set_credential_metadata(
        fixture.ctx(),
        fixture.machine(),
        &fixture.repository_id,
        Some("ea"),
        Some("t"),
    )
    .await
    .expect("record the credential");
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    fixture.reviews_on(2, vec![high("The retry never stops")]);
    fixture.fixes_on(3, "fixed", "Bounded it.");
    fixture.reviews_on(4, vec![]);
    let config = RunnerConfig {
        credentials: CredentialAccess::new(store),
        ..fixture.config()
    };

    fixture.run_with(&config).await;

    let child = fixture.cli.env(&task, 3);
    assert_eq!(child.get("GH_TOKEN").map(String::as_str), Some(SENTINEL));
    assert_eq!(
        child.get("GIT_TERMINAL_PROMPT").map(String::as_str),
        Some("0")
    );
    assert!(
        !child
            .keys()
            .any(|name| rimaia_core::runner::process::is_process_identity(name)),
        "a CLAUDE_* variable reached the fix: {:?}",
        child.keys().collect::<Vec<_>>()
    );

    let fix = &fixture.rows().await[2];
    let stderr = std::fs::read_to_string(stderr_path(&fixture.paths, &task, &fix.id))
        .expect("the fix's stderr capture");
    assert!(
        stderr.contains("token "),
        "the stand-in echoed the token: {stderr}"
    );
    assert!(
        !stderr.contains(SENTINEL),
        "the token was redacted: {stderr}"
    );
    let transcript = std::fs::read_to_string(transcript_path(&fixture.paths, &task, &fix.id))
        .expect("the fix's transcript");
    assert!(!transcript.contains(SENTINEL));
}

#[tokio::test]
async fn a_phase_grant_is_revoked_when_the_phase_ends() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    fixture.reviews_on(2, vec![]);

    fixture.run().await;

    assert_served(&fixture.cli.tool_answer(&task, 2));
    let url = mcp_url(&fixture.cli.argv(&task, 2));
    // `tokio`'s, not `std`'s: the server answering it runs on this test's own
    // single-threaded runtime, which a blocking wait would starve.
    let status = tokio::process::Command::new("curl")
        .args([
            "-s",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "-X",
            "POST",
            "-H",
            "accept: application/json, text/event-stream",
            "-H",
            "content-type: application/json",
            "-d",
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            &url,
        ])
        .output()
        .await
        .expect("curl must be runnable");
    assert_eq!(String::from_utf8_lossy(&status.stdout), "404");
}

// ---------------------------------------------------------------------------
// Rows without a spawn, and the worktree checks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_review_refused_before_spawn_is_recorded_as_a_failed_review_row() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    // No MCP endpoint bound: the busy-port case D16.7 makes non-fatal.
    let config = RunnerConfig {
        run_handles: RunHandles::default(),
        ..fixture.config()
    };

    fixture.run_with(&config).await;

    assert_eq!(fixture.cli.attempts(&task), 1, "the review never spawned");
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 2);
    let review = &rows[1];
    assert_eq!(review.kind, RunKind::Review);
    assert_eq!(review.exit_class, Some(ExitClass::Fatal));
    assert_eq!(
        review.error_message.as_deref(),
        Some("The review needs Rimaia's MCP server, which is not listening (see Settings → MCP).")
    );
    assert_eq!(review.prompt, "");
    assert_eq!(review.base_sha, rows[0].base_sha);

    let detail = fixture.detail().await;
    assert_eq!(detail.task.column, BoardColumn::InReview);
    assert_eq!(detail.task.run_state, RunState::Idle);
    assert_eq!(
        detail.review_loop.expect("the loop ran").verdict,
        Verdict::Unreviewed {
            reason: UnreviewedReason::ReviewFailed
        }
    );
    let report = startup::survey(fixture.ctx(), fixture.machine(), &fixture.paths)
        .await
        .expect("survey the store");
    assert_eq!(report.missing_run_logs, Vec::<String>::new());
}

#[tokio::test]
async fn a_cancel_between_phases_lands_in_review_unreviewed() {
    // The Cancel lands once the implementation has finished and before the
    // review has spawned: nothing is running to stop, so the review is
    // recorded as a cancelled row of its own kind.
    let fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let cancel = CancelSignal::new();
    let config = fixture.config();
    let spy = Spy::new(fixture.board(&config));
    *spy.cancel_on_finish.lock().expect("the spy") = Some(cancel.clone());

    fixture
        .run_through(
            &spy,
            &config,
            RunRequest {
                cancel,
                in_flight: None,
            },
        )
        .await;

    assert_eq!(fixture.cli.attempts(&fixture.task_id), 1);
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].kind, RunKind::Review);
    assert_eq!(rows[1].status, RunStatus::Cancelled);
    let detail = fixture.detail().await;
    assert_eq!(detail.task.column, BoardColumn::InReview);
    assert_eq!(detail.task.run_state, RunState::Idle, "never failed");
}

#[tokio::test]
async fn cancelling_during_a_review_ends_the_loop_in_review_unreviewed() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    fixture
        .cli
        .commits_on_attempt(&task, 1, "Implement Alpha", "success", 0);
    // Every later attempt of this task hangs until it is stopped.
    fixture.cli.hangs(&task, "interrupted-sigterm", 5);
    let cancel = CancelSignal::new();
    let config = fixture.config();
    let spy = Spy::new(fixture.board(&config));
    *spy.cancel_on_review_start.lock().expect("the spy") = Some(cancel.clone());

    fixture
        .run_through(
            &spy,
            &config,
            RunRequest {
                cancel,
                in_flight: None,
            },
        )
        .await;

    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 2, "the loop did not continue after it");
    assert_eq!(rows[1].kind, RunKind::Review);
    assert_eq!(rows[1].exit_class, Some(ExitClass::Cancelled));
    let detail = fixture.detail().await;
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
async fn a_review_that_leaves_tracked_changes_lands_unreviewed() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    fixture.cli.calls_tool_on_attempt(
        &task,
        2,
        "record_review_findings",
        json!({ "task_id": task, "findings": [] }),
        WorktreeAction::Edit("README.md".to_string()),
        "success",
    );

    fixture.run().await;

    assert_served(&fixture.cli.tool_answer(&task, 2));
    let rows = fixture.rows().await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].exit_class, Some(ExitClass::Fatal));
    assert_eq!(
        rows[1].error_message.as_deref(),
        Some("The review changed the worktree without committing.")
    );
    let detail = fixture.detail().await;
    assert_eq!(detail.task.column, BoardColumn::InReview);
    assert_eq!(
        detail.review_loop.expect("the loop ran").verdict,
        Verdict::Unreviewed {
            reason: UnreviewedReason::ReviewFailed
        },
        "a review that recorded findings: [] and edited the worktree is not clean",
    );
}

#[tokio::test]
async fn a_review_is_refused_when_the_implementation_left_tracked_changes() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    fixture
        .cli
        .edits_on_attempt(&task, 1, "README.md", "success");

    fixture.run().await;

    assert_eq!(fixture.cli.attempts(&task), 1, "no review was spawned");
    let rows = fixture.rows().await;
    assert_eq!(rows[1].kind, RunKind::Review);
    assert_eq!(rows[1].exit_class, Some(ExitClass::Fatal));
    assert_eq!(
        rows[1].error_message.as_deref(),
        Some(
            "The worktree has uncommitted changes to tracked files, and a review judges \
             commits, so the review was not started."
        )
    );
    assert_eq!(fixture.detail().await.task.column, BoardColumn::InReview);
}

// ---------------------------------------------------------------------------
// One claim across phases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_in_flight_slot_is_held_across_phases_and_released_once() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    fixture.reviews_on(2, vec![high("The retry never stops")]);
    fixture.fixes_on(3, "fixed", "Bounded it.");
    fixture.reviews_on(4, vec![]);
    let in_flight = InFlight::new();
    let slot = in_flight
        .acquire_unbounded(&task, &fixture.repository_id, SlotOwner::Queue)
        .expect("the queue's slot");
    let config = fixture.config();
    let spy = Spy::new(fixture.board(&config));
    *spy.in_flight.lock().expect("the spy") = Some(in_flight.clone());

    fixture
        .run_through(
            &spy,
            &config,
            RunRequest {
                cancel: slot.cancel_signal(),
                in_flight: Some(in_flight.clone()),
            },
        )
        .await;

    let starts = spy.starts.lock().expect("the spy").clone();
    assert_eq!(starts.len(), 4);
    assert!(
        starts.iter().all(|start| start.slot_held),
        "a second starter could have taken the task between phases: {starts:?}"
    );
    assert_eq!(*spy.releases.lock().expect("the spy"), 0, "never released");
    assert!(in_flight
        .acquire_unbounded(&task, &fixture.repository_id, SlotOwner::Manual)
        .is_err());
    drop(slot);
    assert!(in_flight
        .acquire_unbounded(&task, &fixture.repository_id, SlotOwner::Manual)
        .is_ok());
}

#[tokio::test]
async fn the_task_stays_running_between_phases_and_moves_to_in_review_once() {
    let mut fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    let team = fixture.harness.solo.team_id.clone();
    fixture.reviews_on(2, vec![]);
    let config = fixture.config();
    let spy = Spy::new(fixture.board(&config));
    let claim = claim_run(&spy, &task, RunTrigger::Queued, false)
        .await
        .expect("claim");
    drain(&mut fixture.harness);

    fixture.run_claimed(&spy, &config, claim).await;

    let starts = spy.starts.lock().expect("the spy").clone();
    assert_eq!(
        starts
            .iter()
            .map(|start| (start.kind, start.column, start.run_state))
            .collect::<Vec<_>>(),
        vec![
            (
                RunKind::Implementation,
                BoardColumn::Ready,
                RunState::Running
            ),
            (RunKind::Review, BoardColumn::Ready, RunState::Running),
        ]
    );
    let rows = fixture.rows().await;
    let (implementation, review) = (rows[0].id.clone(), rows[1].id.clone());
    assert_eq!(
        drain(&mut fixture.harness),
        vec![
            // The worktree record and the branch (task 066).
            ChangeEvent::tasks(team.clone(), [task.clone()]),
            ChangeEvent::tasks(team.clone(), [task.clone()]),
            ChangeEvent::runs(team.clone(), [implementation.clone()]),
            ChangeEvent::tasks(team.clone(), [task.clone()]),
            // The implementation's close lands nothing: the loop continues.
            ChangeEvent::runs(team.clone(), [implementation.clone()]),
            ChangeEvent::tasks(team.clone(), [task.clone()]),
            ChangeEvent::runs(team.clone(), [review.clone()]),
            ChangeEvent::tasks(team.clone(), [task.clone()]),
            // `record_review_findings`.
            ChangeEvent::tasks(team.clone(), [task.clone()]),
            // The review's close, then the loop's exit: the move, the
            // run-state write and the lease in one transaction (task 043).
            ChangeEvent::runs(team.clone(), [review.clone()]),
            ChangeEvent::tasks(team.clone(), [task.clone()]),
            ChangeEvent::tasks(team.clone(), [task.clone()]),
        ],
    );
}

// ---------------------------------------------------------------------------
// Resume by kind
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_review_waiting_on_a_usage_limit_is_resumed_by_the_queue_as_a_review() {
    // The queue's half: the claim the runner loop makes, `Next`, over the view
    // it sends (task 042). The board picks the due review and resumes it as
    // itself; the loop only hands the claim to `run_task`.
    let fixture = Fixture::new().await;
    fixture.waits_in_a_review_on_a_usage_limit().await;
    let review = fixture.rows().await[1].clone();
    fixture.reviews_on(3, vec![]);
    fixture.advance_past(review.resume_after.expect("a deadline"));

    let config = fixture.config();
    let board = fixture.board(&config);
    let (repositories, capacity) = scheduler::for_runner(fixture.machine(), &InFlight::new())
        .await
        .expect("the runner's view");
    let claim = board
        .claim(ClaimTarget::Next {
            capacity,
            repositories,
            wait: Duration::ZERO,
            ceiling: Default::default(),
        })
        .await
        .expect("claim")
        .expect("the due review is the next task");
    fixture.run_claimed(board.as_ref(), &config, claim).await;

    fixture.assert_resumed_as_a_review(&review).await;

    // And Retry now does the same.
    let fixture = Fixture::new().await;
    fixture.waits_in_a_review_on_a_usage_limit().await;
    let review = fixture.rows().await[1].clone();
    fixture.reviews_on(3, vec![]);
    fixture.advance_past(review.resume_after.expect("a deadline"));
    let config = fixture.config();
    let board = fixture.board(&config);
    let in_flight = InFlight::new();
    let started = claim_manual_start(
        // Asked from away, for the trigger every recording echoes; the route
        // is Retry now's.
        fixture.harness.starter(OwnerPresence::Remote),
        board.as_ref(),
        fixture.machine(),
        &fixture.paths,
        &config,
        &in_flight,
        ManualStart {
            task_id: fixture.task_id.clone(),
            continue_session: true,
        },
    )
    .await
    .expect("Retry now claims the waiting review");
    fixture
        .run_claimed(board.as_ref(), &config, started.claim)
        .await;
    drop(started.slot);

    fixture.assert_resumed_as_a_review(&review).await;
}

#[tokio::test]
async fn a_fix_waiting_on_a_usage_limit_resumes_as_a_fix_with_the_fix_resume_prompt() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({})).await;
    let task = fixture.task_id.clone();
    fixture.reviews_on(2, vec![high("The retry never stops")]);
    fixture.cli.replays_on_attempt(&task, 3, "usage-limit", 143);
    fixture.run().await;
    let fix = fixture.rows().await[2].clone();
    assert_eq!(fix.kind, RunKind::Fix);
    assert_eq!(
        fixture.detail().await.task.run_state,
        RunState::WaitingRetry
    );

    fixture.fixes_on(4, "fixed", "Bounded it.");
    fixture.reviews_on(5, vec![]);
    fixture.advance_past(fix.resume_after.expect("a deadline"));
    let config = fixture.config();
    let board = fixture.board(&config);
    let claim = claim_run(board.as_ref(), &task, RunTrigger::Queued, true)
        .await
        .expect("the waiting fix is claimed");
    fixture.run_claimed(board.as_ref(), &config, claim).await;

    let argv = fixture.cli.argv(&task, 4);
    assert_eq!(value_after(&argv, "--resume"), fix.session_id);
    assert_eq!(
        fixture.cli.stdin(&task, 4),
        compose_fix_resume(&fixture.detail().await, RESOLVE_TOOL)
    );
    let rows = fixture.rows().await;
    assert_eq!(rows[3].kind, RunKind::Fix);
    assert_eq!(rows[3].session_id, fix.session_id);
    assert_eq!(
        fixture
            .detail()
            .await
            .review_loop
            .expect("the loop")
            .verdict,
        Verdict::Clean
    );
}

#[tokio::test]
async fn a_resumed_fix_continues_the_implementation_session_not_the_reviews() {
    let fixture = Fixture::new().await;
    fixture.enable(json!({ "fix_session": "resume" })).await;
    let task = fixture.task_id.clone();
    fixture.reviews_on(2, vec![high("The retry never stops")]);
    fixture.fixes_on(3, "fixed", "Bounded it.");
    fixture.reviews_on(4, vec![]);

    fixture.run().await;

    let rows = fixture.rows().await;
    let argv = fixture.cli.argv(&task, 3);
    assert_eq!(value_after(&argv, "--resume"), rows[0].session_id);
    assert_ne!(rows[2].session_id, rows[1].session_id, "not the review's");
    assert_eq!(rows[2].session_id, rows[0].session_id);
    // Only the findings and how to answer: the session holds the rest.
    let finding = fixture.findings().await.remove(0);
    let stdin = fixture.cli.stdin(&task, 3);
    assert!(stdin.starts_with("# Findings to address"), "{stdin}");
    assert!(stdin.contains(&finding.id));
    assert!(!stdin.contains("# Plan"));
}

#[tokio::test]
async fn a_resumed_fix_with_no_implementation_session_opens_a_fresh_one_and_says_so() {
    // The local store always holds an implementation row by the time a fix
    // runs, so a board that leaves the session out of its context is the only
    // way to the arm. The fix is cut short by a usage limit so the store still
    // reads as it did when the fix was composed.
    let fixture = Fixture::new().await;
    fixture.enable(json!({ "fix_session": "resume" })).await;
    let task = fixture.task_id.clone();
    fixture.reviews_on(2, vec![high("The retry never stops")]);
    fixture.cli.replays_on_attempt(&task, 3, "usage-limit", 143);
    let config = fixture.config();
    let spy = Spy::new(fixture.board(&config));
    *spy.forgets_implementation.lock().expect("the spy") = true;

    fixture
        .run_through(&spy, &config, RunRequest::default())
        .await;

    let rows = fixture.rows().await;
    assert_eq!(
        rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
        vec![RunKind::Implementation, RunKind::Review, RunKind::Fix]
    );
    let fix = &rows[2];
    let argv = fixture.cli.argv(&task, 3);
    assert!(!argv.iter().any(|arg| arg == "--resume"), "{argv:?}");
    assert_eq!(value_after(&argv, "--session-id"), fix.session_id);
    assert_ne!(
        fix.session_id, rows[0].session_id,
        "not the implementation's"
    );
    assert_ne!(fix.session_id, rows[1].session_id, "not the review's");

    let context = fixture
        .board(&config)
        .preview(&task, PreviewOf::Run)
        .await
        .expect("the task's context");
    let mut review = context.review.expect("the loop's context");
    review.implementation = None;
    let composed = compose_fix_prompt(
        &context.base_instructions,
        &context.task,
        &context.repository,
        None,
        &review,
        RESOLVE_TOOL,
    );
    assert_eq!(fixture.cli.stdin(&task, 3), composed);
    assert_eq!(fix.prompt, composed);

    fixture.assert_noted(
        fix,
        "rimaia: fix_session is resume, and this fix opened a fresh session instead: \
         the task has no implementation session to continue",
    );
}

#[tokio::test]
async fn a_resumed_fix_on_a_provider_that_cannot_continue_opens_a_fresh_one_and_says_so() {
    // Ledger cannot deny file mutation, so an unattended review on it is
    // refused (ADR-0012): the run is a manual one, which records the
    // mitigation it lacks and proceeds. Its handle is its configuration home,
    // so the implementation, which carries none, must run `inherit` and the
    // phases, which do, `strict_local`: the spy switches between them. The fix
    // is cut short by its usage window so the store still reads as it did
    // when the fix was composed.
    let fixture = Fixture::new().await;
    fixture.enable(json!({ "fix_session": "resume" })).await;
    let task = fixture.task_id.clone();
    let finished = path_for(ProviderId::Ledger, "finished");
    fixture.cli.replays_path(&task, &finished, 0);
    fixture.cli.calls_tool_on_attempt_path(
        &task,
        2,
        "record_review_findings",
        json!({ "task_id": task, "findings": [high("The retry never stops")] }),
        WorktreeAction::Nothing,
        &finished,
    );
    fixture.cli.replays_path_on_attempt(
        &task,
        3,
        &path_for(ProviderId::Ledger, "window-closed"),
        1,
    );
    let config = RunnerConfig {
        provider: Arc::new(LedgerWithoutResume),
        program: fixture.cli.program_for(ProviderId::Ledger),
        run_handles: fixture.handles.clone(),
        ..RunnerConfig::default()
    };
    let board = fixture.board(&config);
    let spy = Spy::new(board.clone());
    *spy.isolates_after_first_finish.lock().expect("the spy") = Some(fixture.machine().clone());
    let claim = claim_run(&spy, &task, RunTrigger::Manual, false)
        .await
        .expect("claim the task");

    fixture.run_claimed(&spy, &config, claim).await;

    assert_served(&fixture.cli.tool_answer(&task, 2));
    let rows = fixture.rows().await;
    assert_eq!(
        rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
        vec![RunKind::Implementation, RunKind::Review, RunKind::Fix]
    );
    let fix = &rows[2];
    assert_eq!(fix.exit_class, Some(ExitClass::UsageLimit));
    let argv = fixture.cli.argv(&task, 3);
    assert!(!argv.iter().any(|arg| arg == "--continue"), "{argv:?}");
    assert_ne!(
        fix.session_id, rows[0].session_id,
        "not the implementation's"
    );
    assert_ne!(fix.session_id, rows[1].session_id, "not the review's");

    let context = board
        .preview(&task, PreviewOf::Run)
        .await
        .expect("the task's context");
    let review = context.review.expect("the loop's context");
    assert!(
        review.implementation.is_some(),
        "there was a session to resume"
    );
    let composed = compose_fix_prompt(
        &context.base_instructions,
        &context.task,
        &context.repository,
        None,
        &review,
        LEDGER_RESOLVE_TOOL,
    );
    assert_eq!(fixture.cli.stdin(&task, 3), composed);
    assert_eq!(fix.prompt, composed);

    fixture.assert_noted(
        fix,
        "rimaia: fix_session is resume, and this fix opened a fresh session instead: \
         this provider cannot continue a session",
    );
}

#[tokio::test]
async fn a_retried_review_counts_once_in_the_digest_and_the_summary() {
    let fixture = Fixture::new().await;
    fixture.waits_in_a_review_on_a_usage_limit().await;
    let review = fixture.rows().await[1].clone();
    fixture.reviews_on(3, vec![]);
    fixture.advance_past(review.resume_after.expect("a deadline"));
    let config = fixture.config();
    let board = fixture.board(&config);
    let claim = claim_run(board.as_ref(), &fixture.task_id, RunTrigger::Queued, true)
        .await
        .expect("claim the waiting review");
    fixture.run_claimed(board.as_ref(), &config, claim).await;

    assert_eq!(fixture.rows().await.len(), 3, "a review across two rows");
    let summary = fixture.detail().await.review_loop.expect("the loop");
    assert_eq!(summary.reviews, 1);
    assert_eq!(summary.verdict, Verdict::Clean);
    let digest = review::digest(fixture.ctx(), Some(fixture.machine()))
        .await
        .expect("the digest");
    let entry = digest
        .entries
        .iter()
        .find(|entry| entry.task_id == fixture.task_id)
        .expect("the task's entry");
    assert_eq!(
        entry
            .review_loop
            .expect("a loop to report")
            .reviews_since_implementation,
        1
    );
}

// ---------------------------------------------------------------------------
// Crash recovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_review_left_open_by_a_crash_is_reconciled_into_in_review_or_a_review_resume() {
    // With retry budget left: offered for resume, as a review.
    let fixture = Fixture::new().await;
    fixture.open_review_left_by_a_crash(0).await;
    // Lease-less rows, as a build older than task 043 left them: solo's arm.
    scheduler::reconcile_unrecorded(
        fixture.ctx(),
        &fixture.harness.solo.runner_id,
        &ClaudeProvider,
        &[],
    )
    .await
    .expect("reconcile");
    let detail = fixture.detail().await;
    assert_eq!(detail.task.run_state, RunState::WaitingRetry);
    let last = detail.last_run.expect("the closed review");
    assert_eq!(last.kind, RunKind::Review);
    assert!(last.resume_after.is_some());
    assert_eq!(
        scheduler::resume_point(fixture.ctx(), &fixture.task_id)
            .await
            .expect("read")
            .expect("a point")
            .kind,
        RunKind::Review
    );

    // With the budget spent: in review, idle, unreviewed. Never failed.
    let fixture = Fixture::new().await;
    fixture
        .open_review_left_by_a_crash(scheduler::MAX_TRANSIENT_ATTEMPTS as usize)
        .await;
    // Lease-less rows, as a build older than task 043 left them: solo's arm.
    scheduler::reconcile_unrecorded(
        fixture.ctx(),
        &fixture.harness.solo.runner_id,
        &ClaudeProvider,
        &[],
    )
    .await
    .expect("reconcile");
    let detail = fixture.detail().await;
    assert_eq!(detail.task.column, BoardColumn::InReview);
    assert_eq!(detail.task.run_state, RunState::Idle);
    assert_eq!(detail.last_run.expect("the review").resume_after, None);
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

fn high(title: &str) -> Value {
    json!({ "severity": "high", "title": title, "body": format!("{title}, explained.") })
}

fn request<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("a well-formed request deserializes")
}

/// That a stand-in's write-back was served rather than refused.
fn assert_served(answer: &Value) {
    assert_eq!(answer["error"], Value::Null, "{answer}");
    assert_ne!(
        answer["result"]["isError"],
        Value::Bool(true),
        "the server refused the write-back: {answer}"
    );
}

/// The argument following `flag`.
fn value_after(argv: &[String], flag: &str) -> String {
    argv.iter()
        .position(|arg| arg == flag)
        .and_then(|at| argv.get(at + 1))
        .unwrap_or_else(|| panic!("{flag} is not in {argv:?}"))
        .clone()
}

/// Every element between `flag` and the next flag.
fn list_after(argv: &[String], flag: &str) -> Vec<String> {
    let at = argv
        .iter()
        .position(|arg| arg == flag)
        .unwrap_or_else(|| panic!("{flag} is not in {argv:?}"));
    argv[at + 1..]
        .iter()
        .take_while(|arg| !arg.starts_with("--"))
        .cloned()
        .collect()
}

/// An argument as the stand-in logs it: one element per line, so an argument
/// with newlines in it reads back as several.
fn lines_of(argument: &str) -> Vec<String> {
    argument.lines().map(str::to_string).collect()
}

/// The scoped URL a phase was handed.
fn mcp_url(argv: &[String]) -> String {
    let config: Value =
        serde_json::from_str(&value_after(argv, "--mcp-config")).expect("inline JSON");
    config["mcpServers"]["rimaia-run"]["url"]
        .as_str()
        .expect("the run-scoped server")
        .to_string()
}

/// The operator surface every spawned run is denied (D30 point 2).
fn operator_surface_denial() -> Vec<String> {
    std::iter::once(format!("mcp__{MCP_SERVER_NAME}"))
        .chain(
            Tool::ALL
                .iter()
                .map(|tool| format!("mcp__{MCP_SERVER_NAME}__{}", tool.as_str())),
        )
        .collect()
}

struct Fixture {
    harness: TestContext,
    paths: AppPaths,
    repository_id: String,
    task_id: String,
    cli: FakeCli,
    handles: RunHandles,
    /// Held so the server keeps serving for the life of the fixture.
    mcp: McpHandle,
    /// Held for their `Drop`; the paths above point inside them.
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
                repository_id: registered.id.clone(),
                title: TITLE.to_string(),
                plan: Some("1. Implement Alpha\n2. Test it".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a ready task")
        .id;

        // A real bound server on an OS-chosen port, sharing the handles the
        // runner mints tokens in, the way the shell wires them in `setup()`.
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
        assert!(handles.endpoint().is_some(), "nothing to write back to");

        Self {
            harness,
            paths,
            repository_id: registered.id,
            task_id,
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

    /// This machine's own state, over the harness's machine store (task 041).
    fn machine(&self) -> &rimaia_core::machine::MachineContext {
        self.harness.machine()
    }

    fn config(&self) -> RunnerConfig {
        RunnerConfig {
            program: self.cli.program(),
            run_handles: self.handles.clone(),
            ..RunnerConfig::default()
        }
    }

    fn board(&self, config: &RunnerConfig) -> Arc<dyn BoardPort> {
        self.harness.board(&self.paths, config)
    }

    /// The operator's own MCP door.
    fn operator(&self) -> RimaiaServer {
        RimaiaServer::new(
            self.ctx().with_source(rimaia_core::db::MutationSource::Mcp),
            testing::doctor::provider(),
            Some(testing::doctor::local_tools(self.machine())),
        )
    }

    /// Turns the loop on globally, with `extra` on top.
    async fn enable(&self, extra: Value) {
        let mut config = json!({ "enabled": "on_cost_acknowledged" });
        for (key, value) in extra.as_object().expect("an object") {
            config[key] = value.clone();
        }
        review_config::set_review_settings(self.ctx(), &ClaudeProvider, "Run /review.", config)
            .await
            .expect("turn the loop on");
    }

    /// Attempt `attempt` reviews and records `findings` over its handle.
    fn reviews_on(&self, attempt: usize, findings: Vec<Value>) {
        self.cli.calls_tool_on_attempt(
            &self.task_id,
            attempt,
            "record_review_findings",
            json!({ "task_id": self.task_id, "findings": findings }),
            WorktreeAction::Nothing,
            "success",
        );
    }

    /// Attempt `attempt` commits and resolves the first finding its prompt
    /// names as `status`, saying `resolution`.
    fn fixes_on(&self, attempt: usize, status: &str, resolution: &str) {
        self.cli.calls_tool_on_attempt(
            &self.task_id,
            attempt,
            "resolve_review_finding",
            json!({
                "task_id": self.task_id,
                "finding_id": "__FINDING_ID__",
                "status": status,
                "resolution": resolution,
            }),
            WorktreeAction::Commit(format!("Address the review ({status})")),
            "success",
        );
    }

    /// A queued run claimed and run to the end of its loop.
    async fn run(&self) {
        self.run_with(&self.config()).await;
    }

    async fn run_with(&self, config: &RunnerConfig) {
        let board = self.board(config);
        self.run_through(board.as_ref(), config, RunRequest::default())
            .await;
    }

    async fn run_through(&self, board: &dyn BoardPort, config: &RunnerConfig, request: RunRequest) {
        let claim = claim_run(board, &self.task_id, RunTrigger::Queued, false)
            .await
            .expect("claim the task");
        tokio::time::timeout(
            TEST_TIMEOUT,
            run_task(board, self.machine(), &self.paths, config, claim, request),
        )
        .await
        .expect("a run must finish inside the test timeout")
        .expect("the run completes");
    }

    async fn run_claimed(&self, board: &dyn BoardPort, config: &RunnerConfig, claim: Claim) -> Run {
        tokio::time::timeout(
            TEST_TIMEOUT,
            run_task(
                board,
                self.machine(),
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
        let mut rows = rimaia_core::runs::list_runs_for_task(self.ctx(), &self.task_id)
            .await
            .expect("read the runs");
        rows.sort_by_key(|row| row.attempt);
        rows
    }

    async fn kinds(&self) -> Vec<RunKind> {
        self.rows().await.iter().map(|row| row.kind).collect()
    }

    async fn findings(&self) -> Vec<ReviewFinding> {
        review::findings::list(self.ctx(), &self.task_id, None)
            .await
            .expect("read the findings")
    }

    async fn detail(&self) -> tasks::TaskDetail {
        tasks::get_task(self.ctx(), &self.task_id)
            .await
            .expect("read the task")
    }

    /// That `note` is a whole line of `run`'s recorded stderr, which is where
    /// the run's history shows it.
    fn assert_noted(&self, run: &Run, note: &str) {
        let stderr = std::fs::read_to_string(stderr_path(&self.paths, &self.task_id, &run.id))
            .expect("the run's stderr capture");
        assert!(stderr.lines().any(|line| line == note), "{stderr}");
    }

    fn advance_past(&self, at: chrono::DateTime<chrono::Utc>) {
        let by = at - self.harness.clock.now() + TimeDelta::minutes(1);
        self.harness.clock.advance(by);
    }

    /// The loop on, the implementation done, and the review it started
    /// stopped at a usage limit and waiting to be resumed.
    async fn waits_in_a_review_on_a_usage_limit(&self) {
        self.enable(json!({})).await;
        self.cli
            .replays_on_attempt(&self.task_id, 2, "usage-limit", 143);
        self.run().await;
        let detail = self.detail().await;
        assert_eq!(detail.task.run_state, RunState::WaitingRetry);
        assert_eq!(detail.task.column, BoardColumn::Ready);
        assert_eq!(
            detail.last_run.expect("the review").exit_class,
            Some(ExitClass::UsageLimit)
        );
    }

    /// The second review spawn resumed the first's session with the review's
    /// own continuation, and the loop ended clean.
    async fn assert_resumed_as_a_review(&self, waiting: &Run) {
        let task = &self.task_id;
        let argv = self.cli.argv(task, 3);
        assert_eq!(value_after(&argv, "--resume"), waiting.session_id);
        let detail = self.detail().await;
        assert_eq!(
            self.cli.stdin(task, 3),
            compose_review_resume(&detail, RECORD_TOOL, false)
        );
        let rows = self.rows().await;
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].kind, RunKind::Review);
        assert_eq!(rows[2].session_id, waiting.session_id);
        assert_eq!(detail.task.column, BoardColumn::InReview);
        assert_eq!(
            detail.review_loop.expect("the loop").verdict,
            Verdict::Clean
        );
    }

    /// The task `running`, its implementation succeeded, and a review left
    /// open by a crash after `spent` interrupted rows of the same review.
    async fn open_review_left_by_a_crash(&self, spent: usize) {
        let ctx = self.ctx();
        for state in [RunState::Queued, RunState::Running] {
            tasks::set_run_state(ctx, &self.task_id, state)
                .await
                .expect("walk to running");
        }
        let open = |kind: RunKind, session: &str| NewRun {
            task_id: self.task_id.clone(),
            kind,
            session_id: session.to_string(),
            prompt: "a prompt".to_string(),
            base_ref: Some("main".to_string()),
            base_sha: None,
        };
        let implementation = start_run(ctx, &self.paths, open(RunKind::Implementation, "impl"))
            .await
            .expect("the implementation row")
            .id;
        testing::runs::close_run(
            ctx,
            &implementation,
            RunStatus::Succeeded,
            ExitClass::Success,
            None,
        )
        .await;
        for _ in 0..spent {
            let earlier = start_run(ctx, &self.paths, open(RunKind::Review, "review"))
                .await
                .expect("an earlier review row")
                .id;
            testing::runs::close_run(
                ctx,
                &earlier,
                RunStatus::Interrupted,
                ExitClass::Interrupted,
                None,
            )
            .await;
        }
        start_run(ctx, &self.paths, open(RunKind::Review, "review"))
            .await
            .expect("the review a crash left open");
    }
}

// ---------------------------------------------------------------------------
// A board that watches the runner
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Started {
    kind: RunKind,
    column: BoardColumn,
    run_state: RunState,
    slot_held: bool,
}

/// Passes every call through, and records what the board looked like at each
/// `start_run`. Optionally cancels the run at a chosen moment.
struct Spy {
    inner: Arc<dyn BoardPort>,
    starts: Mutex<Vec<Started>>,
    releases: Mutex<usize>,
    in_flight: Mutex<Option<InFlight>>,
    cancel_on_finish: Mutex<Option<CancelSignal>>,
    cancel_on_review_start: Mutex<Option<CancelSignal>>,
    /// Hands out every context without the implementation's session, as a
    /// board that does not know it would.
    forgets_implementation: Mutex<bool>,
    /// Switches the run environment to `strict_local` once the first run has
    /// finished.
    isolates_after_first_finish: Mutex<Option<rimaia_core::machine::MachineContext>>,
}

impl Spy {
    fn new(inner: Arc<dyn BoardPort>) -> Self {
        Self {
            inner,
            starts: Mutex::new(Vec::new()),
            releases: Mutex::new(0),
            in_flight: Mutex::new(None),
            cancel_on_finish: Mutex::new(None),
            cancel_on_review_start: Mutex::new(None),
            forgets_implementation: Mutex::new(false),
            isolates_after_first_finish: Mutex::new(None),
        }
    }
}

impl BoardPort for Spy {
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
        Box::pin(async move {
            let mut context = self.inner.run_context(lease).await?;
            if *self.forgets_implementation.lock().expect("the spy") {
                if let Some(review) = context.review.as_mut() {
                    review.implementation = None;
                }
            }
            Ok(context)
        })
    }

    fn record_branch<'a>(&'a self, lease: &'a LeaseRef, branch: &'a str) -> BoardFuture<'a, ()> {
        self.inner.record_branch(lease, branch)
    }

    fn start_run<'a>(&'a self, lease: &'a LeaseRef, run: StartRun) -> BoardFuture<'a, ()> {
        Box::pin(async move {
            let context = self.inner.run_context(lease).await?;
            let slot_held = self
                .in_flight
                .lock()
                .expect("the spy")
                .as_ref()
                .is_some_and(|in_flight| {
                    in_flight
                        .acquire_unbounded(
                            &lease.task_id,
                            &context.repository.id,
                            SlotOwner::Manual,
                        )
                        .is_err()
                });
            self.starts.lock().expect("the spy").push(Started {
                kind: run.kind,
                column: context.task.task.column,
                run_state: context.task.task.run_state,
                slot_held,
            });
            let kind = run.kind;
            self.inner.start_run(lease, run).await?;
            if kind == RunKind::Review {
                if let Some(cancel) = self.cancel_on_review_start.lock().expect("the spy").take() {
                    cancel.cancel();
                }
            }
            Ok(())
        })
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
            let isolate = self
                .isolates_after_first_finish
                .lock()
                .expect("the spy")
                .take();
            if let Some(ctx) = isolate {
                settings::set_run_environment(&ctx, RunEnvironment::StrictLocal).await?;
            }
            if let Some(cancel) = self.cancel_on_finish.lock().expect("the spy").take() {
                cancel.cancel();
            }
            Ok(receipt)
        })
    }

    fn release<'a>(&'a self, lease: &'a LeaseRef) -> BoardFuture<'a, ()> {
        *self.releases.lock().expect("the spy") += 1;
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
