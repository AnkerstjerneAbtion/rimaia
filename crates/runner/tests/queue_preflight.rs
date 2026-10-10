//! **Unix only.** The `claude` stand-ins here are `/bin/sh` shebang scripts,
//! and Windows has no shebang, so the file is gated whole rather than test by
//! test: a file that compiled and ran nothing would report a green Windows job
//! that had checked none of this.
#![cfg(unix)]

//! The preflight doctor's gate on the run queue (task 018, seam-contract D22
//! point 1), from the outside.
//!
//! The refusal lives on `QueueHandle::start`, which moved to this crate with
//! the loop in task 042, so these tests moved from `crates/core/tests/doctor.rs`
//! with it, keeping their names and their assertions. The doctor's own checks
//! stay tested in core.

use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use rimaia_core::doctor::{Check, CheckStatus, Environment};
use rimaia_core::runner::RunnerConfig;
use rimaia_core::scheduler::{self, InFlight, QueueState};
use rimaia_core::testing::TestContext;
use rimaia_core::AppPaths;
use rimaia_runner::queue::{self, QueueHandle, QueueTask, SoloBoard};
use tempfile::TempDir;

/// A queue over the harness's board and machine, unspawned: these tests only
/// ever press Start.
fn build_queue(
    harness: &TestContext,
    paths: AppPaths,
    runner: RunnerConfig,
) -> (QueueHandle, QueueTask) {
    queue::build(
        harness.machine().clone(),
        harness.board(&paths, &runner),
        harness.context.subscribe(),
        SoloBoard::new(harness.context.clone()),
        InFlight::new(),
        paths,
        runner,
    )
}

fn stub(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("the stub must be writable");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("the stub must be executable");
    }
    path
}

/// A `claude` that answers both questions the doctor asks it, the way the real
/// one does (verified against 2.1.258).
fn healthy_claude(dir: &Path) -> PathBuf {
    stub(
        dir,
        "claude",
        "#!/bin/sh\n\
         case \"$1\" in\n\
         --version) echo '2.1.258 (Claude Code)'; exit 0 ;;\n\
         auth) echo '{\"loggedIn\": true, \"authMethod\": \"claude.ai\"}'; exit 0 ;;\n\
         esac\n\
         exit 1\n",
    )
}

#[tokio::test]
async fn a_blocking_report_refuses_to_start_the_queue_and_writes_no_queue_state() {
    let harness = TestContext::new().await;
    let root = TempDir::new().expect("a temporary directory");
    let paths = AppPaths::new(root.path());
    paths.create_all().expect("the app directories");

    // A `claude` that is not there. Everything else about this installation is
    // fine, which is the point: one failing check is enough.
    let runner = RunnerConfig {
        program: root.path().join("claude-that-is-not-installed"),
        ..RunnerConfig::default()
    };
    let (queue, _task) = build_queue(&harness, paths, runner);

    let refusal = queue
        .start()
        .await
        .expect_err("a blocking report must refuse the start");

    assert!(
        refusal.to_string().contains("Install Claude Code"),
        "the refusal must carry the remediation, not just a count: {refusal}"
    );
    // The half-done state this ordering exists to prevent: a queue that says it
    // is running while nothing will ever start.
    assert_eq!(
        scheduler::queue_state(harness.machine())
            .await
            .expect("the queue state must be readable"),
        QueueState::Paused,
    );
}

#[tokio::test]
async fn dismissing_every_row_still_refuses_to_start_the_queue_and_writes_no_queue_state() {
    // Task 027's load-bearing test, and the reason it is written against
    // `QueueHandle::start` rather than against the report: dismissal is
    // presentation, the refusal is the rule (D22 point 1, ADR-0006). If a later
    // change ever wires the dismissal set into the gate, this is what says so.
    let harness = TestContext::new().await;
    let root = TempDir::new().expect("a temporary directory");
    let paths = AppPaths::new(root.path());
    paths.create_all().expect("the app directories");

    let runner = RunnerConfig {
        program: root.path().join("claude-that-is-not-installed"),
        ..RunnerConfig::default()
    };
    let environment = Environment::for_runner(paths.clone(), &runner);

    // Every row on the report, not only the warnings — the point is that even
    // an unusually determined user cannot dismiss their way past the gate.
    let report = rimaia_core::doctor::run(harness.machine(), &harness.context, &environment)
        .await
        .expect("the report must be readable");
    assert!(
        report.is_blocking(),
        "this test needs a blocking environment"
    );
    for result in &report.results {
        rimaia_core::doctor::dismiss(harness.machine(), result.dismissal())
            .await
            .expect("the dismissal must store");
    }

    let (queue, _task) = build_queue(&harness, paths, runner);

    let refusal = queue
        .start()
        .await
        .expect_err("a blocking report must refuse the start, dismissed or not");

    assert!(
        refusal.to_string().contains("Install Claude Code"),
        "the refusal must carry the same remediation it always did: {refusal}"
    );
    assert_eq!(
        scheduler::queue_state(harness.machine())
            .await
            .expect("the queue state must be readable"),
        QueueState::Paused,
    );
}

#[tokio::test]
async fn a_healthy_installation_starts_the_queue_even_with_warnings_outstanding() {
    // The other half of the refusal, and the one that would rot silently: a
    // preflight that blocked on warnings would look identical in the test above.
    // Here the MCP endpoint is deliberately unbound — a real `Warn` — and the
    // queue starts anyway.
    let harness = TestContext::new().await;
    let root = TempDir::new().expect("a temporary directory");
    let paths = AppPaths::new(root.path());
    paths.create_all().expect("the app directories");

    let runner = RunnerConfig {
        program: healthy_claude(root.path()),
        ..RunnerConfig::default()
    };
    let (queue, _task) = build_queue(&harness, paths.clone(), runner.clone());

    let report = rimaia_core::doctor::run(
        harness.machine(),
        &harness.context,
        &Environment::for_runner(paths, &runner),
    )
    .await
    .expect("the report must be readable");
    assert!(
        report
            .results
            .iter()
            .any(|result| result.check == Check::McpPort && result.status == CheckStatus::Warn),
        "this test is only meaningful while the unbound MCP port is a warning",
    );
    assert!(!report.is_blocking(), "{}", report.blocking_summary());

    queue.start().await.expect("a warning must not refuse");

    assert_eq!(
        scheduler::queue_state(harness.machine())
            .await
            .expect("the queue state must be readable"),
        QueueState::Running,
    );
}
