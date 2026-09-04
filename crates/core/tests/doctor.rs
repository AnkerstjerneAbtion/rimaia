//! Task 018's preflight doctor, from the outside.
//!
//! Every check here runs against something real: a `TempDir` that is genuinely
//! unwritable, a git repository that has genuinely been renamed, a `gh` that
//! genuinely exits non-zero. Nothing is mocked, in keeping with ADR-0015 —
//! a mocked filesystem would only ever prove the mock works, and a doctor whose
//! whole job is to describe the real environment is the last place to fake one.
//!
//! The two external binaries the doctor probes are stood in for by scripts on
//! disk, which is the same technique `tests/runner_process.rs` already uses for
//! `claude`: it is the only way to test "signed out" and "gh is installed but
//! not authenticated" without depending on the login state of whoever is
//! running the suite. The scripts are executed by the OS through their shebang;
//! nothing here goes through `sh -c`.

use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use rimaia_core::db::Repository;
use rimaia_core::doctor::{
    checks, Check, CheckResult, CheckStatus, DoctorReport, Environment, Programs,
};
use rimaia_core::runner::RunnerConfig;
use rimaia_core::scheduler::{self, InFlight, QueueState};
use rimaia_core::testing::{TempRepo, TestContext};
use rimaia_core::AppPaths;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Stand-in binaries
// ---------------------------------------------------------------------------

/// Writes an executable script at `dir/name` and returns its path.
fn stub(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("the stub must be writable");
    #[cfg(unix)]
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

/// A test repository whose `origin` has a real host, so `gh auth status
/// --hostname` is a question that applies. [`TempRepo::with_remote`] points at a
/// bare local path on purpose, which correctly has no host at all — good for the
/// tests that must never touch `gh`, useless for the ones that must.
fn repo_with_a_github_remote() -> (TempRepo, Repository) {
    let repo = TempRepo::init();
    let status = std::process::Command::new("git")
        .current_dir(repo.path())
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/example/example.git",
        ])
        .status()
        .expect("git must be runnable");
    assert!(status.success(), "adding the remote failed");

    let row = row_for(repo.path(), "example");
    (repo, row)
}

/// A `Repository` row built directly rather than registered.
///
/// The per-repository checks take a row and a program path and touch no
/// database, which is the whole reason they are shaped that way — a test of
/// "this directory moved" has no business also exercising registration.
fn row_for(path: &Path, name: &str) -> Repository {
    Repository {
        id: format!("repository-{name}"),
        name: name.to_string(),
        path: path.display().to_string(),
        default_branch: "main".to_string(),
        worktree_root: path.join("worktrees").display().to_string(),
        allow_unattended_runs: true,
        max_concurrency: 1,
        created_at: rimaia_core::testing::test_epoch(),
    }
}

// ---------------------------------------------------------------------------
// The prerequisites
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_claude_binary_is_a_blocking_failure_with_a_useful_message() {
    // Task 018's first acceptance criterion. The binary is *renamed* rather
    // than removed from `PATH`, because that is the shape of the real accident
    // — an upgrade that moved it, a shell alias that was doing the work — and
    // because a test that edited the process's own `PATH` would race every
    // other test in the binary.
    let dir = TempDir::new().expect("a temporary directory");
    let claude = healthy_claude(dir.path());
    let renamed = dir.path().join("claude-2.1.258");
    std::fs::rename(&claude, &renamed).expect("the rename must succeed");

    let result = checks::claude_cli(&claude).await;

    assert_eq!(result.status, CheckStatus::Fail);
    assert_eq!(result.check, Check::ClaudeCli);
    // "Useful" is not "non-empty": it has to name the binary that could not be
    // run and say what to do about it, or it is the mid-run crash this check
    // exists to replace, moved earlier.
    assert!(
        result.detail.contains("claude"),
        "the failure must name the binary it could not run: {}",
        result.detail
    );
    let remediation = result
        .remediation
        .clone()
        .expect("a failure carries a remediation");
    assert!(
        remediation.contains("Install Claude Code"),
        "the remediation must be actionable: {remediation}"
    );

    // And the report built from it refuses the queue rather than merely
    // colouring a row.
    let report = DoctorReport::new(vec![result], Vec::new());
    assert!(report.is_blocking());
    assert!(report.blocking_summary().contains("Install Claude Code"));
}

#[tokio::test]
async fn a_claude_that_runs_but_is_signed_out_is_a_blocking_failure() {
    let dir = TempDir::new().expect("a temporary directory");
    let claude = stub(
        dir.path(),
        "claude",
        "#!/bin/sh\n\
         case \"$1\" in\n\
         --version) echo '2.1.258 (Claude Code)'; exit 0 ;;\n\
         auth) echo '{\"loggedIn\": false}'; exit 0 ;;\n\
         esac\n\
         exit 1\n",
    );

    assert_eq!(checks::claude_cli(&claude).await.status, CheckStatus::Pass);

    let result = checks::claude_authenticated(&claude).await;
    assert_eq!(result.status, CheckStatus::Fail);
    assert!(result
        .remediation
        .expect("a failure carries a remediation")
        .contains("claude auth login"));
}

#[tokio::test]
async fn a_cli_too_old_to_answer_auth_status_is_a_warning_rather_than_a_false_negative() {
    // The doctor must never report "not signed in" on the evidence of a CLI
    // that has no `auth status` subcommand — that is the check lying, and a
    // lying check is worse than an admitted gap.
    let dir = TempDir::new().expect("a temporary directory");
    let claude = stub(
        dir.path(),
        "claude",
        "#!/bin/sh\n\
         if [ \"$1\" = \"--version\" ]; then echo '2.1.100 (Claude Code)'; exit 0; fi\n\
         echo \"error: unknown command 'auth'\" >&2\n\
         exit 2\n",
    );

    let result = checks::claude_authenticated(&claude).await;

    assert_eq!(result.status, CheckStatus::Warn);
    assert!(!result.status.is_blocking());
}

#[tokio::test]
async fn a_claude_older_than_the_pinned_minimum_warns_instead_of_locking_the_user_out() {
    let dir = TempDir::new().expect("a temporary directory");
    let claude = stub(
        dir.path(),
        "claude",
        "#!/bin/sh\necho '2.0.1 (Claude Code)'\nexit 0\n",
    );

    let result = checks::claude_cli(&claude).await;

    // Deliberately not a failure: see the check's own doc and seam-contract
    // D19. A version string is evidence about what has been tested, not a
    // reason to refuse someone their own queue.
    assert_eq!(result.status, CheckStatus::Warn);
    assert!(result.detail.contains("2.0.1"));
}

#[tokio::test]
async fn a_git_too_old_for_worktree_removal_is_a_blocking_failure() {
    // The deliberate asymmetry with `claude`: every run begins by creating a
    // worktree, so there is nothing for the user to discover by being allowed
    // to try.
    let dir = TempDir::new().expect("a temporary directory");
    let git = stub(
        dir.path(),
        "git",
        "#!/bin/sh\necho 'git version 2.10.0'\nexit 0\n",
    );

    let result = checks::git(&git).await;

    assert_eq!(result.status, CheckStatus::Fail);
    assert!(result.detail.contains("2.10.0"));
}

#[tokio::test]
async fn the_real_git_on_this_machine_is_new_enough_for_worktrees() {
    // Not a tautology: the whole suite creates real worktrees, so a `git` that
    // failed this check would mean the minimum is wrong rather than the machine.
    let result = checks::git(Path::new("git")).await;

    assert_eq!(result.status, CheckStatus::Pass, "{}", result.detail);
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn an_unwritable_data_directory_is_a_blocking_failure() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new().expect("a temporary directory");
    let data_dir = root.path().join("rimaia");
    std::fs::create_dir(&data_dir).expect("the data directory");
    // Readable and traversable, but not writable — the case permission bits
    // alone would get wrong if the check inspected them instead of writing.
    std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o500))
        .expect("the permissions must be settable");

    let result = checks::data_directory(&AppPaths::new(&data_dir));

    // Restored before any assertion can panic and leave a directory `TempDir`
    // cannot clean up.
    std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o755))
        .expect("the permissions must be restorable");

    assert_eq!(result.status, CheckStatus::Fail);
    assert_eq!(result.check, Check::DataDirectory);
    assert!(result
        .remediation
        .expect("a failure carries a remediation")
        .contains("permissions"));
}

#[tokio::test]
async fn a_writable_data_directory_passes_and_leaves_no_probe_file_behind() {
    let root = TempDir::new().expect("a temporary directory");
    let paths = AppPaths::new(root.path());

    assert_eq!(
        checks::data_directory(&paths).status,
        CheckStatus::Pass,
        "a fresh temporary directory must be writable"
    );

    let leftovers: Vec<String> = std::fs::read_dir(root.path())
        .expect("the directory must be readable")
        .map(|entry| entry.expect("a readable entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the write probe must clean up after itself: {leftovers:?}"
    );
}

#[tokio::test]
async fn free_space_on_a_real_volume_is_measured_rather_than_guessed() {
    let root = TempDir::new().expect("a temporary directory");

    let result = checks::disk_space(&AppPaths::new(root.path()));

    // The status depends on the machine, which is the point — what must never
    // happen is the "could not measure" warning, because that would mean `fs4`
    // is not answering for an ordinary directory.
    assert!(
        !result.detail.contains("could not be measured"),
        "free space must be measurable on a temporary directory: {}",
        result.detail
    );
    assert!(result.detail.contains("free on"));
}

// ---------------------------------------------------------------------------
// Per repository
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unauthenticated_gh_warns_and_names_the_repository() {
    // Task 018's second acceptance criterion, and the reason `repo::gh_status`
    // exists beside `remote_info`: "not installed" and "not authenticated" are
    // different remediations, and `gh_ready: Some(false)` cannot tell them
    // apart.
    let dir = TempDir::new().expect("a temporary directory");
    let gh = stub(
        dir.path(),
        "gh",
        "#!/bin/sh\necho 'not logged in to github.com' >&2\nexit 1\n",
    );
    let (_repo, row) = repo_with_a_github_remote();

    let result = checks::github_cli(&row, &gh)
        .await
        .expect("git must be runnable");

    assert_eq!(result.status, CheckStatus::Warn);
    assert!(
        !result.status.is_blocking(),
        "a repository may not need a PR"
    );
    assert_eq!(result.repository.as_deref(), Some("example"));
    assert!(
        result.detail.contains("example"),
        "the warning must name the affected repository in its own sentence: {}",
        result.detail
    );
    assert!(result
        .remediation
        .expect("a warning carries a remediation")
        .contains("gh auth login"));
}

#[tokio::test]
async fn a_missing_gh_is_told_apart_from_an_unauthenticated_one() {
    let dir = TempDir::new().expect("a temporary directory");
    let absent = dir.path().join("gh-that-was-never-installed");
    let (_repo, row) = repo_with_a_github_remote();

    let result = checks::github_cli(&row, &absent)
        .await
        .expect("git must be runnable");

    assert_eq!(result.status, CheckStatus::Warn);
    assert!(
        result.detail.contains("not installed"),
        "installing gh and logging in to gh are different instructions: {}",
        result.detail
    );
}

#[tokio::test]
async fn a_repository_with_no_remote_host_never_asks_gh_anything() {
    // A bare local-path `origin` has no host, so there is no pull request to
    // open and nothing to warn about. The stub would fail if it were called.
    let dir = TempDir::new().expect("a temporary directory");
    let gh = stub(dir.path(), "gh", "#!/bin/sh\nexit 1\n");
    let repo = TempRepo::init().with_remote();
    let row = row_for(repo.path(), "local-only");

    let result = checks::github_cli(&row, &gh)
        .await
        .expect("git must be runnable");

    assert_eq!(result.status, CheckStatus::Pass);
    assert!(result.detail.contains("no remote host"));
}

#[tokio::test]
async fn a_repository_whose_path_has_moved_is_reported_rather_than_discovered_at_worktree_creation()
{
    // The row is written while the directory exists and read after it has been
    // renamed — which is exactly what happens to a real installation when
    // somebody tidies up their projects folder. Nothing tells Rimaia.
    let root = TempDir::new().expect("a temporary directory");
    let original = root.path().join("project");
    std::fs::create_dir(&original).expect("the project directory");
    let repo = TempRepo::init();
    let row = row_for(&original, "moved-away");

    assert_eq!(
        checks::repository_path(&row_for(repo.path(), "still-there"))
            .await
            .expect("git must be runnable")
            .status,
        CheckStatus::Pass,
        "a repository that has not moved must not be reported"
    );

    std::fs::rename(&original, root.path().join("project-renamed")).expect("the rename");

    let result = checks::repository_path(&row)
        .await
        .expect("git must be runnable");

    assert_eq!(result.status, CheckStatus::Fail);
    assert_eq!(result.repository.as_deref(), Some("moved-away"));
    assert!(result.detail.contains("no longer exists"));
    assert!(result
        .remediation
        .expect("a failure carries a remediation")
        .contains("Settings → Repositories"));
}

#[tokio::test]
async fn a_directory_that_is_no_longer_a_git_repository_is_reported_too() {
    // Existing is not the same as valid: `rm -rf .git` leaves a directory that
    // every path check based on `exists()` alone would happily accept.
    let repo = TempRepo::init();
    let row = row_for(repo.path(), "de-gitted");
    std::fs::remove_dir_all(repo.path().join(".git")).expect("the .git directory");

    let result = checks::repository_path(&row)
        .await
        .expect("git must be runnable");

    assert_eq!(result.status, CheckStatus::Fail);
    assert!(result.detail.contains("no longer a git repository"));
}

// ---------------------------------------------------------------------------
// The report, and the queue's refusal
// ---------------------------------------------------------------------------

#[test]
fn every_failing_result_carries_a_specific_remediation() {
    // A `Warn` or `Fail` without a remediation would be the "check your setup"
    // this whole module exists to not be.
    let report = DoctorReport::new(
        vec![
            CheckResult::pass(Check::Git, "fine"),
            CheckResult::warn(Check::McpPort, "detail", "do this"),
            CheckResult::fail(Check::ClaudeCli, "detail", "do that"),
        ],
        Vec::new(),
    );

    for result in &report.results {
        assert_eq!(
            result.remediation.is_some(),
            result.status != CheckStatus::Pass,
            "{} carries the wrong remediation for a {:?}",
            result.check.as_str(),
            result.status,
        );
    }
}

#[test]
fn a_blocking_summary_names_every_failing_check_rather_than_counting_them() {
    let report = DoctorReport::new(
        vec![
            CheckResult::fail(Check::ClaudeCli, "claude is missing.", "Install it."),
            CheckResult::fail(
                Check::RepositoryPath,
                "rimaia has moved.",
                "Re-register it.",
            )
            .about("rimaia"),
            CheckResult::warn(Check::McpPort, "nothing is listening.", "Pick a free port."),
        ],
        Vec::new(),
    );

    let summary = report.blocking_summary();

    assert!(summary.contains("claude is missing."));
    assert!(summary.contains("Install it."));
    assert!(summary.contains("rimaia has moved."));
    // The per-repository failure says which repository, in the summary and not
    // only in the row it came from.
    assert!(summary.contains("(rimaia)"));
    assert!(summary.contains("2 preflight checks are failing"));
    // A warning is not a reason the queue did not start, so it has no business
    // in the sentence explaining why it did not.
    assert!(!summary.contains("Pick a free port."));
}

#[test]
fn a_warning_does_not_block_the_queue() {
    let report = DoctorReport::new(
        vec![
            CheckResult::pass(Check::ClaudeCli, "2.1.258 on PATH."),
            CheckResult::warn(Check::McpPort, "nothing is listening.", "Pick a free port."),
            CheckResult::warn(Check::GitHubCli, "gh is not installed.", "Install gh.")
                .about("rimaia"),
        ],
        Vec::new(),
    );

    assert!(!report.is_blocking());
    assert_eq!(report.blocking().count(), 0);
}

#[test]
fn results_are_ordered_by_check_regardless_of_the_order_they_were_collected_in() {
    // The panel must not reshuffle between two presses of Re-check, and `run`
    // collects the per-repository rows last.
    let report = DoctorReport::new(
        vec![
            CheckResult::pass(Check::McpPort, "listening"),
            CheckResult::pass(Check::GitHubCli, "gh").about("b"),
            CheckResult::pass(Check::ClaudeCli, "claude"),
            CheckResult::pass(Check::GitHubCli, "gh").about("a"),
        ],
        Vec::new(),
    );

    let order: Vec<&str> = report
        .results
        .iter()
        .map(|result| result.check.as_str())
        .collect();
    assert_eq!(
        order,
        vec!["claude_cli", "github_cli", "github_cli", "mcp_port"]
    );
    // Stable within a check, so two repositories keep the order `repo::list`
    // returned them in.
    assert_eq!(report.results[1].repository.as_deref(), Some("b"));
    assert_eq!(report.results[2].repository.as_deref(), Some("a"));
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
    let (queue, _task) = scheduler::build(harness.context.clone(), paths, runner, InFlight::new());

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
        scheduler::queue_state(&harness.context.pool)
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
    let report = rimaia_core::doctor::run(&harness.context, &environment)
        .await
        .expect("the report must be readable");
    assert!(
        report.is_blocking(),
        "this test needs a blocking environment"
    );
    for result in &report.results {
        rimaia_core::doctor::dismiss(&harness.context, result.dismissal())
            .await
            .expect("the dismissal must store");
    }

    let (queue, _task) = scheduler::build(harness.context.clone(), paths, runner, InFlight::new());

    let refusal = queue
        .start()
        .await
        .expect_err("a blocking report must refuse the start, dismissed or not");

    assert!(
        refusal.to_string().contains("Install Claude Code"),
        "the refusal must carry the same remediation it always did: {refusal}"
    );
    assert_eq!(
        scheduler::queue_state(&harness.context.pool)
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
    let (queue, _task) = scheduler::build(
        harness.context.clone(),
        paths.clone(),
        runner.clone(),
        InFlight::new(),
    );

    let report =
        rimaia_core::doctor::run(&harness.context, &Environment::for_runner(paths, &runner))
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
        scheduler::queue_state(&harness.context.pool)
            .await
            .expect("the queue state must be readable"),
        QueueState::Running,
    );
}

// ---------------------------------------------------------------------------
// Dismissal (task 027)
// ---------------------------------------------------------------------------

fn warn_about(check: Check, detail: &str) -> CheckResult {
    CheckResult::warn(check, detail, "do the thing")
}

#[test]
fn every_check_serializes_with_the_spelling_its_accessor_returns() {
    // One string for the stored dismissal, the wire and `Check::as_str` — the
    // same three-way agreement `db::models` pins for every row enum. It matters
    // from task 027 onward because a dismissal is *keyed* on the check, so a
    // value written through serde and compared against `as_str` has to be the
    // same bytes. `GitHubCli` is the one `rename_all` gets wrong on its own.
    for check in Check::ALL {
        assert_eq!(
            serde_json::to_value(check).expect("a check must serialize"),
            serde_json::Value::String(check.as_str().to_string()),
            "{}",
            check.as_str()
        );
        assert_eq!(
            serde_json::from_value::<Check>(serde_json::Value::String(check.as_str().to_string()))
                .expect("a check must round-trip"),
            check
        );
    }
}

#[test]
fn a_dismissal_marks_the_row_it_names_and_leaves_the_report_otherwise_whole() {
    let dismissed = warn_about(Check::McpPort, "nothing is listening on 4517.");
    let report = DoctorReport::new(
        vec![
            dismissed.clone(),
            warn_about(Check::GitHubCli, "gh is not authenticated.").about("rimaia"),
        ],
        vec![dismissed.dismissal()],
    );

    let marked: Vec<(&str, bool)> = report
        .results
        .iter()
        .map(|result| (result.check.as_str(), result.dismissed))
        .collect();
    assert_eq!(marked, vec![("github_cli", false), ("mcp_port", true)]);
    // Marked, never dropped: Settings → Environment has to be able to give it
    // back, and a row that left the report could not be found again.
    assert_eq!(report.results.len(), 2);
}

#[test]
fn the_same_check_about_a_different_repository_is_a_different_warning() {
    // Task 027's first acceptance criterion, and the reason the key is the
    // row's content rather than its check.
    let first =
        warn_about(Check::GitHubCli, "gh is not authenticated, used by alpha.").about("alpha");
    let second =
        warn_about(Check::GitHubCli, "gh is not authenticated, used by beta.").about("beta");

    let report = DoctorReport::new(vec![first.clone(), second.clone()], vec![first.dismissal()]);

    let marked: Vec<bool> = report
        .results
        .iter()
        .map(|result| result.dismissed)
        .collect();
    assert_eq!(marked, vec![true, false]);
}

#[test]
fn a_warning_whose_detail_changed_comes_back_without_anything_being_cleared() {
    // "claude 2.1.200 is older than the pinned minimum" is answered; upgrading
    // to a version that is *still* too old is a sentence the user has not read.
    let old = warn_about(Check::ClaudeCli, "claude 2.1.200 is older than 2.1.234.");
    let new = warn_about(Check::ClaudeCli, "claude 2.1.210 is older than 2.1.234.");

    let report = DoctorReport::new(vec![new], vec![old.dismissal()]);

    assert!(!report.results[0].dismissed);
    // And the stale dismissal is still on the report, so it is something the
    // user can find and clear rather than an invisible permanent entry.
    assert_eq!(report.dismissals, vec![old.dismissal()]);
}

#[test]
fn a_failure_is_never_marked_dismissed_however_the_dismissal_was_stored() {
    // `fail` collapses; it does not disappear. Enforced when the report is
    // built rather than when the dismissal is written, so a row that was a
    // warning yesterday and is a failure today is not silently silenced.
    let row = CheckResult::fail(Check::ClaudeCli, "claude was not found.", "Install it.");

    let report = DoctorReport::new(vec![row.clone()], vec![row.dismissal()]);

    assert!(!report.results[0].dismissed);
    assert!(report.is_blocking());
    assert!(report.blocking_summary().contains("Install it."));
}

#[test]
fn dismissing_every_row_changes_nothing_about_what_blocks() {
    // The assertion task 027 calls the one that matters, at the unit level;
    // `tests/scheduler.rs` carries the same claim against a real queue start.
    let rows = vec![
        CheckResult::fail(Check::ClaudeCli, "claude was not found.", "Install it."),
        warn_about(Check::McpPort, "nothing is listening."),
    ];
    let every = rows.iter().map(CheckResult::dismissal).collect();

    let report = DoctorReport::new(rows, every);

    assert!(report.is_blocking());
    assert_eq!(report.blocking().count(), 1);
    assert!(report.blocking_summary().contains("Install it."));
}

#[tokio::test]
async fn a_dismissal_survives_a_restart_and_a_recheck() {
    // Stored in `settings`, so "the app was restarted" is just another read.
    let harness = TestContext::new().await;
    let row = warn_about(Check::McpPort, "nothing is listening on 4517.");

    let stored = rimaia_core::doctor::dismiss(&harness.context, row.dismissal())
        .await
        .expect("the dismissal must store");
    assert_eq!(stored, vec![row.dismissal()]);

    // A second press of Dismiss is not a second entry.
    let stored = rimaia_core::doctor::dismiss(&harness.context, row.dismissal())
        .await
        .expect("dismissing twice must be idempotent");
    assert_eq!(stored, vec![row.dismissal()]);

    assert_eq!(
        rimaia_core::db::settings::doctor_dismissals(&harness.context.pool)
            .await
            .expect("a fresh read is what a restart does"),
        vec![row.dismissal()]
    );

    let cleared = rimaia_core::doctor::restore(&harness.context, &row.dismissal())
        .await
        .expect("the dismissal must clear");
    assert_eq!(cleared, Vec::new());
}

#[tokio::test]
async fn a_hand_edited_dismissals_row_costs_a_warning_rather_than_a_launch() {
    // ADR-0003 counts hand-editing the file as supported, and `settings` has no
    // CHECK on `value`. The `run_environment` precedent: warn and fall back.
    let harness = TestContext::new().await;

    rimaia_core::db::settings::set(&harness.context, "doctor_dismissals", "not json at all")
        .await
        .expect("store a typo");
    assert_eq!(
        rimaia_core::db::settings::doctor_dismissals(&harness.context.pool)
            .await
            .expect("an unreadable value must not fail the read"),
        Vec::new()
    );

    // One bad element, the rest intact — the two failures are different sizes.
    rimaia_core::db::settings::set(
        &harness.context,
        "doctor_dismissals",
        r#"[{"check":"not_a_check","repository":null,"detail":"x"},
            {"check":"mcp_port","repository":null,"detail":"nothing is listening."}]"#,
    )
    .await
    .expect("store a half-good value");

    let read = rimaia_core::db::settings::doctor_dismissals(&harness.context.pool)
        .await
        .expect("one bad element must not lose the rest");
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].check, Check::McpPort);
}

#[tokio::test]
async fn the_doctor_reads_its_dismissals_from_settings_on_every_run() {
    let harness = TestContext::new().await;
    let root = TempDir::new().expect("a temporary directory");
    let paths = AppPaths::new(root.path());
    paths.create_all().expect("the app directories");
    let runner = RunnerConfig {
        program: healthy_claude(root.path()),
        ..RunnerConfig::default()
    };
    let environment = Environment::for_runner(paths, &runner);

    // The unbound MCP port is a real warning on a report nothing has touched.
    let before = rimaia_core::doctor::run(&harness.context, &environment)
        .await
        .expect("the report must be readable");
    let mcp_port = before
        .results
        .iter()
        .find(|result| result.check == Check::McpPort)
        .expect("the MCP port is always checked")
        .clone();
    assert_eq!(mcp_port.status, CheckStatus::Warn);
    assert!(!mcp_port.dismissed);

    rimaia_core::doctor::dismiss(&harness.context, mcp_port.dismissal())
        .await
        .expect("the dismissal must store");

    let after = rimaia_core::doctor::run(&harness.context, &environment)
        .await
        .expect("the report must be readable");
    assert!(
        after
            .results
            .iter()
            .find(|result| result.check == Check::McpPort)
            .expect("the MCP port is always checked")
            .dismissed,
        "a Re-check with the environment unchanged must keep the row dismissed",
    );
    assert_eq!(after.dismissals, vec![mcp_port.dismissal()]);
}

#[tokio::test]
async fn the_doctor_probes_the_runners_own_claude_rather_than_the_one_on_path() {
    // A doctor that reported on a different binary from the one the queue
    // spawns would be reassuring about the wrong thing — which is why
    // `Environment::for_runner` exists rather than `Programs::default`.
    let root = TempDir::new().expect("a temporary directory");
    let runner = RunnerConfig {
        program: root.path().join("claude-that-is-not-installed"),
        ..RunnerConfig::default()
    };

    let environment = Environment::for_runner(AppPaths::new(root.path()), &runner);

    assert_eq!(environment.programs.claude, runner.program);
    // The other two keep their defaults; only `claude` is configurable.
    assert_eq!(environment.programs.git, Programs::default().git);
}
