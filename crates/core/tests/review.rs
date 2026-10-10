//! The review actions, the dependents read and the overnight digest (task 034,
//! ADR-0006, ADR-0007, ADR-0008, seam-contract D29 point 8).
//!
//! Three kinds of test, in the order of the module:
//!
//! 1. **Board rules** (refusals, atomicity, the digest, the marker). Tasks and
//!    run rows are written directly where the subject is what the service does
//!    with them; no run is faked by a mock. The clock is the harness's
//!    `TestClock` and nothing sleeps.
//! 2. **End to end** against a real repository in a `TempDir`, with the runs
//!    driven as real child processes replaying recorded fixture streams
//!    (`FakeCli`). Unix only, for `runner_process.rs`'s reason: the stand-in is a
//!    POSIX shell script. Git is never mocked (ADR-0015).
//! 3. Prompt and refusal strings are asserted whole, never by substring.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, Duration, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::db::{settings, BoardColumn, Run, RunKind, RunState, Task};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::review::digest::{DigestEntry, DigestOutcome, DigestTotals};
use rimaia_core::review::{self, Dependent, Digest};
use rimaia_core::scheduler::SkipReason;
use rimaia_core::tasks::{self, NewTask, TaskFilter};
use rimaia_core::testing::{test_epoch, TempRepo, TestContext};
use rimaia_core::{AppPaths, Change, ChangeEvent, ErrorCode, ServiceContext};
use tempfile::TempDir;

const CHANGES: &str =
    "Review note (changes requested; the reviewed commits were kept, so build on them):";
const REJECTED: &str =
    "Review note (rejected; the task restarted on a fresh branch without those commits):";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Verdict {
    Approve,
    RequestChanges,
    Reject,
}

const VERDICTS: [Verdict; 3] = [Verdict::Approve, Verdict::RequestChanges, Verdict::Reject];

// ---------------------------------------------------------------------------
// The note, through the services
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_blank_note_is_refused_by_reject_and_by_request_changes() {
    for blank in ["", "  \n"] {
        for verdict in [Verdict::RequestChanges, Verdict::Reject] {
            let f = Fixture::new().await;
            let id = f.task("Review me", BoardColumn::InReview).await;
            let before = f.snapshot(&id).await;

            let error = f
                .decide(verdict, &id, blank)
                .await
                .expect_err("a blank note is refused");

            assert_eq!(error.code(), ErrorCode::Invalid);
            assert_eq!(f.snapshot(&id).await, before, "nothing is written");
        }
    }
}

// ---------------------------------------------------------------------------
// Approve
// ---------------------------------------------------------------------------

#[tokio::test]
async fn approve_moves_the_task_to_the_bottom_of_done() {
    let f = Fixture::new().await;
    let finished = f.task("Already done", BoardColumn::Done).await;
    let id = f.task("Review me", BoardColumn::InReview).await;

    let approved = review::approve(f.ctx(), Some(f.machine()), &id)
        .await
        .expect("approve");

    assert_eq!(approved.column, BoardColumn::Done);
    let done: Vec<String> = f
        .column(BoardColumn::Done)
        .await
        .into_iter()
        .map(|task| task.id)
        .collect();
    assert_eq!(done, vec![finished, id]);
}

// ---------------------------------------------------------------------------
// Refusals and atomicity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn review_actions_refuse_a_task_that_is_not_in_review() {
    for column in [BoardColumn::NotReady, BoardColumn::Ready, BoardColumn::Done] {
        for verdict in VERDICTS {
            let f = Fixture::new().await;
            let id = f.task("Elsewhere", column).await;
            let before = f.snapshot(&id).await;

            let error = f
                .decide(verdict, &id, "A note.")
                .await
                .expect_err("only a task in review can be decided");

            assert_eq!(
                error.code(),
                ErrorCode::Invalid,
                "{verdict:?} in {column:?}"
            );
            assert!(
                error.to_string().contains("\"Elsewhere\""),
                "names the task"
            );
            assert_eq!(f.snapshot(&id).await, before, "{verdict:?} in {column:?}");
        }
    }
}

#[tokio::test]
async fn review_actions_refuse_an_archived_task() {
    for verdict in VERDICTS {
        let f = Fixture::new().await;
        let id = f.task("Archived", BoardColumn::InReview).await;
        f.set(&id, "archived_at = '2026-08-19T00:00:00+00:00'")
            .await;
        let before = f.snapshot(&id).await;

        let error = f
            .decide(verdict, &id, "A note.")
            .await
            .expect_err("an archived task is refused");

        assert_eq!(error.code(), ErrorCode::Invalid, "{verdict:?}");
        assert!(error.to_string().contains("\"Archived\""));
        assert_eq!(f.snapshot(&id).await, before, "{verdict:?}");
    }
}

#[tokio::test]
async fn review_actions_refuse_a_queued_running_or_waiting_task() {
    for state in ["queued", "running", "waiting_retry"] {
        for verdict in VERDICTS {
            let f = Fixture::new().await;
            let id = f.task("In flight", BoardColumn::InReview).await;
            f.set(&id, &format!("run_state = '{state}'")).await;
            let before = f.snapshot(&id).await;

            let error = f
                .decide(verdict, &id, "A note.")
                .await
                .expect_err("a live task is refused, with no override");

            assert_eq!(
                error.code(),
                ErrorCode::Invalid,
                "{verdict:?} while {state}"
            );
            assert_eq!(f.snapshot(&id).await, before, "{verdict:?} while {state}");
        }
    }
}

#[tokio::test]
async fn reject_and_request_changes_refuse_a_failed_task_and_say_to_retry() {
    for state in ["failed", "cancelled"] {
        for verdict in [Verdict::RequestChanges, Verdict::Reject] {
            let f = Fixture::new().await;
            let id = f.task("Needs attention", BoardColumn::InReview).await;
            f.set(&id, &format!("run_state = '{state}'")).await;
            let before = f.snapshot(&id).await;

            let error = f
                .decide(verdict, &id, "A note.")
                .await
                .expect_err("the queue would skip it");

            let word = if state == "failed" {
                "failed"
            } else {
                "cancelled"
            };
            assert_eq!(
                error.to_string(),
                format!(
                    "\"Needs attention\" ended its last run {word}, and the queue skips a task \
                     in that state, so sending it back to ready would leave it sitting there. \
                     Use Retry on it instead. (task {id})"
                ),
            );
            assert_eq!(f.snapshot(&id).await, before);
        }

        // Approving it is allowed: the human has looked at what is there.
        let f = Fixture::new().await;
        let id = f.task("Needs attention", BoardColumn::InReview).await;
        f.set(&id, &format!("run_state = '{state}'")).await;
        review::approve(f.ctx(), Some(f.machine()), &id)
            .await
            .expect("approve");
    }
}

#[tokio::test]
async fn review_actions_accept_an_idle_or_blocked_task() {
    for state in ["idle", "blocked"] {
        for verdict in VERDICTS {
            let f = Fixture::new().await;
            let id = f.task("Waiting its turn", BoardColumn::InReview).await;
            f.set(&id, &format!("run_state = '{state}'")).await;

            f.decide(verdict, &id, "A note.")
                .await
                .unwrap_or_else(|error| panic!("{verdict:?} while {state}: {error}"));

            let after = f.reload(&id).await;
            // No action writes `run_state`: a blocked task stays blocked, and
            // the queue keeps skipping it until the dependency succeeds.
            assert_eq!(
                after.run_state,
                if state == "blocked" {
                    RunState::Blocked
                } else {
                    RunState::Idle
                }
            );
        }
    }
}

#[tokio::test]
async fn a_refused_move_leaves_extra_instructions_unchanged() {
    let f = Fixture::new().await;
    let id = f.task("No plan", BoardColumn::InReview).await;
    f.set(&id, "plan = NULL, extra_instructions = 'Keep it small.'")
        .await;

    let error = review::request_changes(f.ctx(), &id, "Add a test.")
        .await
        .expect_err("a task with no plan cannot go back to ready");

    assert_eq!(
        error.to_string(),
        "cannot put \"No plan\" in ready without a plan"
    );
    assert_eq!(
        f.reload(&id).await.extra_instructions.as_deref(),
        Some("Keep it small.")
    );
}

// ---------------------------------------------------------------------------
// Notes and branches on a board with no disk in it
// ---------------------------------------------------------------------------

#[tokio::test]
async fn request_changes_appends_the_note_and_keeps_the_branch() {
    let f = Fixture::new().await;
    let id = f.task("Review me", BoardColumn::InReview).await;
    f.set(
        &id,
        "extra_instructions = 'Keep it small.', branch = 'rimaia/review-me'",
    )
    .await;

    let outcome = review::request_changes(f.ctx(), &id, "  Add a test.\n")
        .await
        .expect("request changes");

    assert_eq!(
        outcome.task.extra_instructions.as_deref(),
        Some(format!("Keep it small.\n\n{CHANGES}\nAdd a test.").as_str())
    );
    assert_eq!(outcome.task.column, BoardColumn::Ready);
    assert_eq!(outcome.task.branch.as_deref(), Some("rimaia/review-me"));
    assert_eq!(outcome.set_aside_branch, None);
}

// ---------------------------------------------------------------------------
// Dependents
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dependents_of_returns_direct_dependents_in_board_order() {
    let f = Fixture::new().await;
    let base = f.task("Base", BoardColumn::InReview).await;
    let late = f.task("Late", BoardColumn::Ready).await;
    let early = f.task("Early", BoardColumn::NotReady).await;
    let archived = f.task("Archived", BoardColumn::Done).await;
    let unrelated = f.task("Unrelated", BoardColumn::Ready).await;
    f.set(&archived, "archived_at = '2026-08-19T00:00:00+00:00'")
        .await;
    for dependent in [&late, &early, &archived] {
        tasks::set_task_dependencies(f.ctx(), dependent, std::slice::from_ref(&base))
            .await
            .expect("add the edge");
    }
    // A dependent of a dependent is not direct, and `unrelated` has no edge.
    tasks::set_task_dependencies(f.ctx(), &unrelated, std::slice::from_ref(&late))
        .await
        .expect("a second-level edge");

    let found: Vec<String> = tasks::dependents_of(f.ctx(), &base)
        .await
        .expect("dependents")
        .into_iter()
        .map(|task| task.id)
        .collect();

    // not_ready, ready, done: ADR-0008's column rank, archived included.
    assert_eq!(found, vec![early, late, archived]);
}

#[tokio::test]
async fn a_dependent_that_ran_on_this_tasks_branch_is_marked_built_on() {
    let f = Fixture::new().await;
    let base = f.task("Base", BoardColumn::InReview).await;
    f.set(&base, "branch = 'rimaia/base'").await;
    // Recorded before task 033: a branch name and no commits.
    let before_033 = f.dependent("Before 033", &base).await;
    f.run_row(RunRow::new(&before_033, 1).base("rimaia/base", None, None))
        .await;
    // Recorded since: the fork point is a commit this task's own run produced.
    f.run_row(RunRow::new(&base, 1).base("main", Some("aaa"), Some("bbb")))
        .await;
    let since_033 = f.dependent("Since 033", &base).await;
    f.run_row(RunRow::new(&since_033, 1).base("main", Some("bbb"), Some("ccc")))
        .await;
    // Forked from the base of the work and not from the work.
    let from_main = f.dependent("From main", &base).await;
    f.run_row(RunRow::new(&from_main, 1).base("main", Some("aaa"), Some("ddd")))
        .await;
    // Never ran.
    f.dependent("Never ran", &base).await;

    let marked: Vec<(String, bool)> = review::dependents(f.ctx(), &base)
        .await
        .expect("dependents")
        .into_iter()
        .map(|dependent| (dependent.title, dependent.built_on))
        .collect();

    assert_eq!(
        marked,
        vec![
            ("Before 033".to_string(), true),
            ("Since 033".to_string(), true),
            ("From main".to_string(), false),
            ("Never ran".to_string(), false),
        ]
    );
}

#[tokio::test]
async fn a_dependent_that_chained_from_another_dependency_is_not_marked_built_on() {
    let f = Fixture::new().await;
    let base = f.task("Base", BoardColumn::InReview).await;
    let other = f.task("Other", BoardColumn::InReview).await;
    f.set(&base, "branch = 'rimaia/base'").await;
    f.set(&other, "branch = 'rimaia/other'").await;
    f.run_row(RunRow::new(&base, 1).base("main", Some("aaa"), Some("bbb")))
        .await;
    f.run_row(RunRow::new(&other, 1).base("main", Some("aaa"), Some("eee")))
        .await;
    let both = f.dependent("On both", &base).await;
    tasks::set_task_dependencies(f.ctx(), &both, &[base.clone(), other.clone()])
        .await
        .expect("a second edge");
    f.run_row(RunRow::new(&both, 1).base("rimaia/other", Some("eee"), Some("fff")))
        .await;

    let dependents = review::dependents(f.ctx(), &base)
        .await
        .expect("dependents");

    assert_eq!(dependents.len(), 1);
    assert!(!dependents[0].built_on);
}

#[tokio::test]
async fn a_dependency_whose_only_run_committed_nothing_does_not_mark_a_default_branch_dependent() {
    let f = Fixture::new().await;
    let base = f.task("Base", BoardColumn::InReview).await;
    // 033 records `head_sha` for a run that committed nothing: it is the fork
    // point, a commit of the default branch.
    f.run_row(RunRow::new(&base, 1).base("main", Some("fork"), Some("fork")))
        .await;
    // After a reject cleared the branch, ADR-0008 falls through to the default
    // branch, and this dependent forked from the same commit.
    let dependent = f.dependent("Forked from main", &base).await;
    f.run_row(RunRow::new(&dependent, 1).base("main", Some("fork"), Some("work")))
        .await;

    let dependents = review::dependents(f.ctx(), &base)
        .await
        .expect("dependents");

    assert_eq!(dependents.len(), 1);
    assert!(!dependents[0].built_on);
}

#[tokio::test]
async fn reject_returns_every_dependent_and_marks_the_ones_that_built_on_it() {
    let f = Fixture::new().await;
    let base = f.task("Base", BoardColumn::InReview).await;
    f.set(&base, "branch = 'rimaia/base'").await;
    f.run_row(RunRow::new(&base, 1).base("main", Some("aaa"), Some("bbb")))
        .await;
    let built = f.dependent("Built on it", &base).await;
    f.run_row(RunRow::new(&built, 1).base("main", Some("bbb"), Some("ccc")))
        .await;
    let waiting = f.dependent("Waiting", &base).await;

    let outcome = review::reject(f.ctx(), Some(f.machine()), &base, "Wrong approach.")
        .await
        .expect("reject");

    // Computed before the branch was cleared: afterwards only the `base_sha`
    // clause could still say it.
    assert_eq!(outcome.task.branch, None);
    assert_eq!(outcome.set_aside_branch.as_deref(), Some("rimaia/base"));
    assert_eq!(
        outcome
            .dependents
            .iter()
            .map(|dependent| (dependent.id.clone(), dependent.built_on))
            .collect::<Vec<_>>(),
        vec![(built.clone(), true), (waiting, false)]
    );
    assert_eq!(
        outcome.task.extra_instructions.as_deref(),
        Some(format!("{REJECTED}\nWrong approach.").as_str())
    );
    // And a later read still agrees, through the `base_sha` clause.
    let later = review::dependents(f.ctx(), &base)
        .await
        .expect("dependents");
    assert!(
        later
            .iter()
            .find(|d| d.id == built)
            .expect("built")
            .built_on
    );
}

// ---------------------------------------------------------------------------
// The digest
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_digest_after_six_tasks_reports_each_outcome() {
    let f = Fixture::new().await;
    let unopted = f.unopted_repository().await;

    let succeeded = f.task("Succeeded", BoardColumn::InReview).await;
    f.run_row(
        RunRow::new(&succeeded, 1)
            .window(-90, Some(-60))
            .cost(Some(1.0))
            .pr("https://example.com/pull/1"),
    )
    .await;

    let failed = f.task("Failed twice", BoardColumn::Ready).await;
    f.set(&failed, "run_state = 'failed'").await;
    f.run_row(
        RunRow::new(&failed, 1)
            .window(-330, Some(-300))
            .status("failed", Some("transient"))
            .cost(Some(0.5))
            .error("rate limited"),
    )
    .await;
    f.run_row(
        RunRow::new(&failed, 2)
            .window(-270, Some(-240))
            .status("failed", Some("fatal"))
            .cost(Some(0.5))
            .error("boom"),
    )
    .await;

    let blocked = f.task("Blocked", BoardColumn::Ready).await;
    tasks::set_task_dependencies(f.ctx(), &blocked, std::slice::from_ref(&failed))
        .await
        .expect("edge");

    let skipped = f.task_in(&unopted, "Skipped", BoardColumn::Ready).await;

    let cancelled = f.task("Cancelled", BoardColumn::Ready).await;
    f.set(&cancelled, "run_state = 'cancelled'").await;
    f.run_row(
        RunRow::new(&cancelled, 1)
            .window(-45, Some(-30))
            .status("cancelled", Some("cancelled"))
            .cost(Some(0.25)),
    )
    .await;

    let uncosted = f.task("No cost recorded", BoardColumn::Done).await;
    f.run_row(RunRow::new(&uncosted, 1).window(-20, Some(-10)).cost(None))
        .await;

    let digest = review::digest(f.ctx(), Some(f.machine()))
        .await
        .expect("digest");

    let entry = |task_id: &str, title: &str, column, outcome| DigestEntry {
        task_id: task_id.to_string(),
        title: title.to_string(),
        repository_id: f.repository_id.clone(),
        column,
        outcome,
        runs: 0,
        run_seconds: None,
        cost_usd: None,
        last_run_id: None,
        error_message: None,
        pr_url: None,
        blocking_title: None,
        skip_reason: None,
        last_run_kind: None,
        review_loop: None,
    };
    let last_run = |task_id: &str, attempt: i64| format!("run-{task_id}-{attempt}");

    let mut expected_failed = entry(
        &failed,
        "Failed twice",
        BoardColumn::Ready,
        DigestOutcome::Failed,
    );
    expected_failed.runs = 2;
    expected_failed.run_seconds = Some(3600);
    expected_failed.cost_usd = Some(1.0);
    expected_failed.last_run_id = Some(last_run(&failed, 2));
    expected_failed.last_run_kind = Some(RunKind::Implementation);
    expected_failed.error_message = Some("boom".to_string());

    let mut expected_blocked = entry(
        &blocked,
        "Blocked",
        BoardColumn::Ready,
        DigestOutcome::Blocked,
    );
    expected_blocked.blocking_title = Some("Failed twice".to_string());

    let mut expected_cancelled = entry(
        &cancelled,
        "Cancelled",
        BoardColumn::Ready,
        DigestOutcome::Cancelled,
    );
    expected_cancelled.runs = 1;
    expected_cancelled.run_seconds = Some(900);
    expected_cancelled.cost_usd = Some(0.25);
    expected_cancelled.last_run_id = Some(last_run(&cancelled, 1));
    expected_cancelled.last_run_kind = Some(RunKind::Implementation);

    let mut expected_succeeded = entry(
        &succeeded,
        "Succeeded",
        BoardColumn::InReview,
        DigestOutcome::Completed,
    );
    expected_succeeded.runs = 1;
    expected_succeeded.run_seconds = Some(1800);
    expected_succeeded.cost_usd = Some(1.0);
    expected_succeeded.last_run_id = Some(last_run(&succeeded, 1));
    expected_succeeded.last_run_kind = Some(RunKind::Implementation);
    expected_succeeded.pr_url = Some("https://example.com/pull/1".to_string());

    let mut expected_uncosted = entry(
        &uncosted,
        "No cost recorded",
        BoardColumn::Done,
        DigestOutcome::Completed,
    );
    expected_uncosted.runs = 1;
    expected_uncosted.run_seconds = Some(600);
    expected_uncosted.last_run_id = Some(last_run(&uncosted, 1));
    expected_uncosted.last_run_kind = Some(RunKind::Implementation);

    let mut expected_skipped = entry(
        &skipped,
        "Skipped",
        BoardColumn::Ready,
        DigestOutcome::Skipped,
    );
    expected_skipped.repository_id = unopted.clone();
    expected_skipped.skip_reason = Some(SkipReason::UnattendedRunsNotAllowed);

    let now = test_epoch();
    assert_eq!(
        digest,
        Digest {
            since: now - Duration::hours(24),
            until: now,
            entries: vec![
                expected_failed,
                expected_blocked,
                expected_cancelled,
                expected_succeeded,
                expected_uncosted,
                expected_skipped,
            ],
            totals: DigestTotals {
                runs: 5,
                run_seconds: 3600 + 1800 + 900 + 600,
                // -330 minutes (the first failed attempt began) to -10.
                span_seconds: Some(320 * 60),
                cost_usd: 2.25,
                runs_without_cost: 1,
                counts: counts(&[
                    (DigestOutcome::Failed, 1),
                    (DigestOutcome::Blocked, 1),
                    (DigestOutcome::Cancelled, 1),
                    (DigestOutcome::Completed, 2),
                    (DigestOutcome::Skipped, 1),
                ]),
            },
        }
    );
}

#[tokio::test]
async fn failures_and_blocked_chains_lead_the_digest() {
    let f = Fixture::new().await;
    let unopted = f.unopted_repository().await;

    // Declared against the board's order on purpose: the two completed tasks
    // are created done-first, so only the comparator can put in_review ahead.
    let completed_done = f.task("Completed, approved", BoardColumn::Done).await;
    f.run_row(RunRow::new(&completed_done, 1).window(-50, Some(-40)))
        .await;
    let completed_waiting = f.task("Completed, waiting", BoardColumn::InReview).await;
    f.run_row(RunRow::new(&completed_waiting, 1).window(-50, Some(-40)))
        .await;

    let running = f.task("Running", BoardColumn::Ready).await;
    f.set(&running, "run_state = 'running'").await;
    f.run_row(
        RunRow::new(&running, 1)
            .window(-5, None)
            .status("running", None),
    )
    .await;

    let cancelled = f.task("Cancelled", BoardColumn::Ready).await;
    f.set(&cancelled, "run_state = 'cancelled'").await;
    f.run_row(
        RunRow::new(&cancelled, 1)
            .window(-50, Some(-40))
            .status("cancelled", Some("cancelled")),
    )
    .await;

    let interrupted = f.task("Interrupted", BoardColumn::Ready).await;
    f.set(&interrupted, "run_state = 'failed'").await;
    f.run_row(
        RunRow::new(&interrupted, 1)
            .window(-50, Some(-40))
            .status("interrupted", Some("interrupted")),
    )
    .await;

    let waiting = f.task("Waiting to retry", BoardColumn::Ready).await;
    f.set(&waiting, "run_state = 'waiting_retry'").await;
    f.run_row(
        RunRow::new(&waiting, 1)
            .window(-50, Some(-40))
            .status("failed", Some("usage_limit")),
    )
    .await;

    let failed = f.task("Failed", BoardColumn::Ready).await;
    f.set(&failed, "run_state = 'failed'").await;
    f.run_row(
        RunRow::new(&failed, 1)
            .window(-50, Some(-40))
            .status("failed", Some("fatal")),
    )
    .await;

    let blocked = f.task("Blocked", BoardColumn::Ready).await;
    tasks::set_task_dependencies(f.ctx(), &blocked, std::slice::from_ref(&failed))
        .await
        .expect("edge");
    f.task_in(&unopted, "Skipped", BoardColumn::Ready).await;

    let digest = review::digest(f.ctx(), Some(f.machine()))
        .await
        .expect("digest");

    assert_eq!(
        digest
            .entries
            .iter()
            .map(|entry| (entry.title.as_str(), entry.outcome))
            .collect::<Vec<_>>(),
        vec![
            ("Failed", DigestOutcome::Failed),
            ("Blocked", DigestOutcome::Blocked),
            ("Waiting to retry", DigestOutcome::WaitingRetry),
            ("Interrupted", DigestOutcome::Interrupted),
            ("Cancelled", DigestOutcome::Cancelled),
            ("Running", DigestOutcome::Running),
            ("Completed, waiting", DigestOutcome::Completed),
            ("Completed, approved", DigestOutcome::Completed),
            ("Skipped", DigestOutcome::Skipped),
        ]
    );
}

#[tokio::test]
async fn a_task_with_three_runs_in_the_window_is_one_entry_with_the_newest_rows_outcome() {
    let f = Fixture::new().await;
    let id = f.task("Persistent", BoardColumn::InReview).await;
    f.run_row(
        RunRow::new(&id, 1)
            .window(-200, Some(-190))
            .status("failed", Some("transient"))
            .cost(Some(0.5)),
    )
    .await;
    f.run_row(
        RunRow::new(&id, 2)
            .window(-150, Some(-140))
            .status("failed", Some("transient"))
            .cost(None),
    )
    .await;
    f.run_row(RunRow::new(&id, 3).window(-100, Some(-90)).cost(Some(1.5)))
        .await;

    let digest = review::digest(f.ctx(), Some(f.machine()))
        .await
        .expect("digest");

    assert_eq!(digest.entries.len(), 1);
    let entry = &digest.entries[0];
    assert_eq!(entry.outcome, DigestOutcome::Completed);
    assert_eq!(entry.runs, 3);
    assert_eq!(entry.run_seconds, Some(1800));
    // One missing cost makes the entry's sum "not recorded", where the totals
    // sum what exists and say how many are missing (D18).
    assert_eq!(entry.cost_usd, None);
    assert_eq!(digest.totals.runs, 3);
    assert_eq!(digest.totals.cost_usd, 2.0);
    assert_eq!(digest.totals.runs_without_cost, 1);
}

#[tokio::test]
async fn a_looped_task_is_one_digest_entry() {
    // D29 point 8: a digest that listed runs would list a looped task three
    // times. The entry says which kind its outcome came from, and its loop
    // numbers are derived from the rows.
    let f = Fixture::new().await;
    let looped = f.task("Looped", BoardColumn::InReview).await;
    let plain = f.task("Plain", BoardColumn::InReview).await;
    f.run_row(RunRow::new(&looped, 1).window(-300, Some(-250)))
        .await;
    f.run_row(
        RunRow::new(&looped, 2)
            .kind(RunKind::Review)
            .window(-240, Some(-230)),
    )
    .await;
    f.run_row(
        RunRow::new(&looped, 3)
            .kind(RunKind::Fix)
            .window(-220, Some(-200)),
    )
    .await;
    let review_run = format!("run-{looped}-2");
    f.finding(&looped, &review_run, 0, "open").await;
    f.finding(&looped, &review_run, 1, "fixed").await;
    f.finding(&looped, &review_run, 2, "open").await;
    f.run_row(RunRow::new(&plain, 1).window(-100, Some(-90)))
        .await;

    let digest = review::digest(f.ctx(), Some(f.machine()))
        .await
        .expect("digest");

    let looped_entries: Vec<&DigestEntry> = digest
        .entries
        .iter()
        .filter(|entry| entry.task_id == looped)
        .collect();
    assert_eq!(looped_entries.len(), 1, "one entry, not one per run");
    let entry = looped_entries[0];
    assert_eq!(entry.outcome, DigestOutcome::Completed);
    assert_eq!(entry.runs, 3);
    assert_eq!(entry.last_run_id, Some(format!("run-{looped}-3")));
    assert_eq!(entry.last_run_kind, Some(RunKind::Fix));
    assert_eq!(
        entry.review_loop,
        Some(review::DigestLoop {
            reviews_since_implementation: 1,
            open_findings: 2,
        }),
    );
    let plain_entry = digest
        .entries
        .iter()
        .find(|entry| entry.task_id == plain)
        .expect("the plain task's entry");
    assert_eq!(plain_entry.last_run_kind, Some(RunKind::Implementation));
    assert_eq!(plain_entry.review_loop, None);
    assert_eq!(digest.totals.runs, 4, "the totals count runs of every kind");

    // ADR-0021: the agent's door carries the same two fields.
    let server = rimaia_core::mcp::RimaiaServer::new(
        f.ctx().with_source(rimaia_core::db::MutationSource::Mcp),
        rimaia_core::testing::doctor::provider(),
        Some(rimaia_core::testing::doctor::local_tools(f.machine())),
    );
    let rmcp::handler::server::wrapper::Json(view) = server
        .get_review_digest()
        .await
        .expect("the digest over MCP");
    let wire = serde_json::to_value(&view).expect("a view serializes");
    let looped_wire = wire["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["task_id"] == serde_json::json!(looped))
        .expect("the looped task over MCP");
    assert_eq!(looped_wire["last_run_kind"], serde_json::json!("fix"));
    assert_eq!(
        looped_wire["review_loop"],
        serde_json::json!({ "reviews_since_implementation": 1, "open_findings": 2 }),
    );
    let plain_wire = wire["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["task_id"] == serde_json::json!(plain))
        .expect("the plain task over MCP");
    assert_eq!(
        plain_wire["last_run_kind"],
        serde_json::json!("implementation")
    );
    assert_eq!(plain_wire["review_loop"], serde_json::Value::Null);
}

#[tokio::test]
async fn a_board_with_only_blocked_or_unopted_ready_tasks_has_an_empty_digest() {
    let f = Fixture::new().await;
    let unopted = f.unopted_repository().await;
    let not_ready = f.task("Not ready", BoardColumn::NotReady).await;
    let blocked = f.task("Blocked", BoardColumn::Ready).await;
    tasks::set_task_dependencies(f.ctx(), &blocked, &[not_ready])
        .await
        .expect("edge");
    f.task_in(&unopted, "Unopted", BoardColumn::Ready).await;

    let digest = review::digest(f.ctx(), Some(f.machine()))
        .await
        .expect("digest");

    assert_eq!(digest.entries, vec![]);
    assert_eq!(
        digest.totals,
        DigestTotals {
            runs: 0,
            run_seconds: 0,
            span_seconds: None,
            cost_usd: 0.0,
            runs_without_cost: 0,
            counts: counts(&[]),
        }
    );
}

#[tokio::test]
async fn a_run_that_ended_at_or_before_the_marker_is_not_in_the_digest() {
    let f = Fixture::new().await;
    let at_marker = f.task("At the marker", BoardColumn::InReview).await;
    let after = f.task("After the marker", BoardColumn::InReview).await;
    let marker = test_epoch() - Duration::minutes(10);
    f.run_row(RunRow::new(&at_marker, 1).window(-30, Some(-10)))
        .await;
    f.run_row(
        RunRow::new(&after, 1)
            .window(-30, Some(-10))
            .ended_at(marker + Duration::seconds(1)),
    )
    .await;
    review::mark_seen(f.ctx(), marker).await.expect("mark seen");

    let digest = review::digest(f.ctx(), Some(f.machine()))
        .await
        .expect("digest");

    assert_eq!(digest.since, marker);
    assert_eq!(
        digest
            .entries
            .iter()
            .map(|e| e.title.as_str())
            .collect::<Vec<_>>(),
        vec!["After the marker"]
    );
    assert_eq!(digest.totals.runs, 1);
}

#[tokio::test]
async fn without_a_marker_the_digest_covers_the_last_24_hours() {
    let f = Fixture::new().await;
    let inside = f.task("Inside", BoardColumn::InReview).await;
    let boundary = f.task("On the boundary", BoardColumn::InReview).await;
    let outside = f.task("Outside", BoardColumn::InReview).await;
    f.run_row(RunRow::new(&inside, 1).window(-1500, Some(-1439)))
        .await;
    f.run_row(RunRow::new(&boundary, 1).window(-1500, Some(-1440)))
        .await;
    f.run_row(RunRow::new(&outside, 1).window(-1600, Some(-1500)))
        .await;

    let digest = review::digest(f.ctx(), Some(f.machine()))
        .await
        .expect("digest");

    assert_eq!(digest.since, test_epoch() - Duration::hours(24));
    assert_eq!(
        digest
            .entries
            .iter()
            .map(|e| e.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Inside"]
    );
}

#[tokio::test]
async fn archived_tasks_are_not_in_the_digest() {
    let f = Fixture::new().await;
    let id = f.task("Archived", BoardColumn::Done).await;
    f.run_row(RunRow::new(&id, 1).window(-30, Some(-20))).await;
    f.set(&id, "archived_at = '2026-08-19T00:00:00+00:00'")
        .await;

    let digest = review::digest(f.ctx(), Some(f.machine()))
        .await
        .expect("digest");

    assert_eq!(digest.entries, vec![]);
    assert_eq!(digest.totals.runs, 0);
}

#[tokio::test]
async fn marking_the_digest_seen_never_moves_the_marker_backwards() {
    let f = Fixture::new().await;
    let later = test_epoch() - Duration::minutes(5);
    let earlier = test_epoch() - Duration::minutes(50);

    review::mark_seen(f.ctx(), later).await.expect("mark seen");
    let stored = review::mark_seen(f.ctx(), earlier)
        .await
        .expect("mark seen");

    assert_eq!(stored, later);
    assert_eq!(f.marker().await, Some(later));
}

#[tokio::test]
async fn marking_the_digest_seen_through_a_future_instant_is_refused() {
    let f = Fixture::new().await;

    let error = review::mark_seen(f.ctx(), test_epoch() + Duration::seconds(1))
        .await
        .expect_err("a time that has not happened yet");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(f.marker().await, None);
}

// ---------------------------------------------------------------------------
// The marker, advanced by a review that finishes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_review_that_empties_the_queue_advances_the_marker() {
    for verdict in VERDICTS {
        let mut f = Fixture::new().await;
        let id = f.task("Last in review", BoardColumn::InReview).await;
        f.drain_events();

        f.decide(verdict, &id, "A note.").await.expect("decide");

        assert_eq!(f.marker().await, Some(test_epoch()), "{verdict:?}");
        let events = f.drain_events();
        assert!(
            events.contains(&ChangeEvent::settings(f.harness.solo.team_id.clone())),
            "{verdict:?}: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event.change, Change::Tasks(_))),
            "{verdict:?}: {events:?}"
        );
    }
}

#[tokio::test]
async fn the_review_that_empties_the_queue_leaves_an_empty_digest() {
    for verdict in VERDICTS {
        let f = Fixture::new().await;
        let unopted = f.unopted_repository().await;
        let id = f.task("Last in review", BoardColumn::InReview).await;
        f.run_row(RunRow::new(&id, 1).window(-60, Some(-30))).await;
        f.task_in(&unopted, "Skipped", BoardColumn::Ready).await;
        let upstream = f.task("Upstream", BoardColumn::NotReady).await;
        let blocked = f.task("Blocked", BoardColumn::Ready).await;
        tasks::set_task_dependencies(f.ctx(), &blocked, &[upstream])
            .await
            .expect("edge");
        let before = review::digest(f.ctx(), Some(f.machine()))
            .await
            .expect("digest");
        assert_eq!(before.entries.len(), 3, "the night had something to say");

        f.decide(verdict, &id, "A note.").await.expect("decide");

        let after = review::digest(f.ctx(), Some(f.machine()))
            .await
            .expect("digest");
        assert_eq!(after.entries, vec![], "{verdict:?}");
        assert_eq!(after.totals.runs, 0, "{verdict:?}");
    }
}

#[tokio::test]
async fn a_review_that_leaves_tasks_in_review_does_not_advance_the_marker() {
    for verdict in VERDICTS {
        let mut f = Fixture::new().await;
        let decided = f.task("Decided", BoardColumn::InReview).await;
        f.task("Still waiting", BoardColumn::InReview).await;
        f.drain_events();

        f.decide(verdict, &decided, "A note.")
            .await
            .expect("decide");

        assert_eq!(f.marker().await, None, "{verdict:?}");
        assert!(
            !f.drain_events()
                .contains(&ChangeEvent::settings(f.harness.solo.team_id.clone())),
            "{verdict:?}"
        );
    }
}

#[tokio::test]
async fn emptying_in_review_by_a_drag_or_an_archive_does_not_advance_the_marker() {
    let f = Fixture::new().await;
    let dragged = f.task("Dragged", BoardColumn::InReview).await;
    tasks::move_task(
        f.ctx(),
        Some(f.machine()),
        &dragged,
        BoardColumn::Done,
        None,
        None,
    )
    .await
    .expect("drag to done");
    assert_eq!(f.marker().await, None);

    let archived = f.task("Archived", BoardColumn::InReview).await;
    tasks::archive_task(f.ctx(), Some(f.machine()), &archived)
        .await
        .expect("archive");
    assert_eq!(f.marker().await, None);
}

// ---------------------------------------------------------------------------
// End to end, against a real repository
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod end_to_end {
    use super::*;
    use pretty_assertions::assert_eq;

    use std::time::Duration as StdDuration;

    use rimaia_core::runner::{run_task, CancelSignal, RunRequest, RunTrigger, RunnerConfig};
    use rimaia_core::testing::board::claim_run;
    use rimaia_core::testing::FakeCli;
    use rimaia_core::worktree::{self, AutoCleanup};

    const TEST_TIMEOUT: StdDuration = StdDuration::from_secs(30);

    #[tokio::test]
    async fn needs_changes_keeps_the_worktree_and_branch_and_the_next_run_continues_there_with_the_note(
    ) {
        let f = Fixture::new().await;
        let cli = FakeCli::new();
        let id = f.main_task.clone();
        cli.commits_a_file_on_attempt(&id, 1, "notes.txt", "Write notes", "success", 0);
        cli.commits_a_file_on_attempt(&id, 2, "more.txt", "Address the review", "success", 0);

        f.run(&cli, &id).await.expect("the first run");
        let reviewed = f.reload(&id).await;
        let reviewed_worktree = f.harness.worktree_path(&id).await;
        assert_eq!(reviewed.column, BoardColumn::InReview);
        let checkout = PathBuf::from(reviewed_worktree.clone().expect("a worktree"));
        let branch = reviewed.branch.clone().expect("a branch");
        let reviewed_commit = git(&checkout, &["rev-parse", "HEAD"]);

        let outcome =
            review::request_changes(f.ctx(), &id, "  The migration must be reversible.\n")
                .await
                .expect("request changes");

        assert_eq!(outcome.task.column, BoardColumn::Ready);
        assert_eq!(f.harness.worktree_path(&id).await, reviewed_worktree);
        assert_eq!(outcome.task.branch, reviewed.branch);
        assert_eq!(outcome.set_aside_branch, None);
        assert!(checkout.exists());

        f.run(&cli, &id).await.expect("the next run");

        let after = f.reload(&id).await;
        assert_eq!(
            f.harness.worktree_path(&id).await,
            reviewed_worktree,
            "the same directory"
        );
        assert_eq!(
            after.branch.as_deref(),
            Some(branch.as_str()),
            "the same branch"
        );
        assert!(
            is_ancestor(&checkout, &reviewed_commit, "HEAD"),
            "the reviewed commit is an ancestor of the new HEAD"
        );
        let base = settings::base_instructions(f.ctx()).await.expect("base");
        assert_eq!(
            cli.stdin(&id, 2),
            format!(
                "# Base instructions\n\n{base}\n\n# Task context\n\n- Title: Write the notes\n\
                 - Repository: {repository}\n- Branch: {branch}\n- Base ref: main\n\n\
                 # Plan\n\n1. Write the notes\n\n# Extra instructions\n\n\
                 {CHANGES}\nThe migration must be reversible.",
                repository = f.repository_name,
            )
        );
    }

    #[tokio::test]
    async fn reject_sets_the_branch_aside_and_the_next_run_starts_on_a_fresh_branch_from_the_base()
    {
        let f = Fixture::new().await;
        let cli = FakeCli::new();
        let id = f.main_task.clone();
        cli.commits_a_file_on_attempt(&id, 1, "wrong.txt", "The wrong approach", "success", 0);
        cli.commits_a_file_on_attempt(&id, 2, "right.txt", "The right approach", "success", 0);
        // A dependent that builds on the rejected work, to prove `built_on`
        // survives the branch being renamed.
        let dependent = f.task_with_plan("Builds on it").await;
        tasks::set_task_dependencies(f.ctx(), &dependent, std::slice::from_ref(&id))
            .await
            .expect("edge");
        cli.commits_a_file_on_attempt(&dependent, 1, "on_top.txt", "Build on top", "success", 0);

        f.run(&cli, &id).await.expect("the first run");
        f.run(&cli, &dependent).await.expect("the dependent's run");
        let reviewed = f.reload(&id).await;
        let reviewed_worktree = f.harness.worktree_path(&id).await;
        let checkout = PathBuf::from(reviewed_worktree.clone().expect("a worktree"));
        let branch = reviewed.branch.clone().expect("a branch");
        let rejected_commit = git(&checkout, &["rev-parse", "HEAD"]);

        let outcome = review::reject(f.ctx(), Some(f.machine()), &id, "Wrong approach.")
            .await
            .expect("reject");

        assert_eq!(outcome.task.branch, None);
        assert_eq!(f.harness.worktree_path(&id).await, None);
        assert_eq!(outcome.task.column, BoardColumn::Ready);
        assert_eq!(outcome.set_aside_branch.as_deref(), Some(branch.as_str()));
        assert!(!checkout.exists(), "the directory is gone");
        assert!(
            branch_exists(f.source.path(), &branch),
            "the branch is set aside, never deleted"
        );
        assert_eq!(
            git(f.source.path(), &["rev-parse", &branch]),
            rejected_commit
        );
        assert_eq!(
            outcome
                .dependents
                .iter()
                .map(|d: &Dependent| (d.id.clone(), d.built_on))
                .collect::<Vec<_>>(),
            vec![(dependent.clone(), true)]
        );

        f.run(&cli, &id).await.expect("the next run");

        let after = f.reload(&id).await;
        assert_eq!(after.branch, Some(format!("{branch}-2")));
        let fresh = PathBuf::from(f.harness.worktree_path(&id).await.expect("a new worktree"));
        assert!(
            !is_ancestor(&fresh, &rejected_commit, "HEAD"),
            "the new branch does not contain the rejected commit"
        );
        let base = settings::base_instructions(f.ctx()).await.expect("base");
        assert_eq!(
            cli.stdin(&id, 2),
            format!(
                "# Base instructions\n\n{base}\n\n# Task context\n\n- Title: Write the notes\n\
                 - Repository: {repository}\n- Branch: {branch}-2\n- Base ref: main\n\n\
                 # Plan\n\n1. Write the notes\n\n# Extra instructions\n\n\
                 {REJECTED}\nWrong approach.",
                repository = f.repository_name,
            )
        );
    }

    #[tokio::test]
    async fn reject_refuses_a_worktree_with_uncommitted_changes_and_names_the_count() {
        let f = Fixture::new().await;
        let cli = FakeCli::new();
        let id = f.main_task.clone();
        cli.replays(&id, "success", 0);
        f.run(&cli, &id).await.expect("the run");
        let reviewed = f.reload(&id).await;
        let reviewed_worktree = f.harness.worktree_path(&id).await;
        let checkout = PathBuf::from(reviewed_worktree.clone().expect("a worktree"));

        std::fs::write(checkout.join("one.txt"), "x\n").expect("a stray file");
        let one = review::reject(f.ctx(), Some(f.machine()), &id, "No.")
            .await
            .expect_err("dirty");
        assert_eq!(
            one.to_string(),
            "\"Write the notes\" has 1 uncommitted change in its worktree, committed nowhere \
             else. Removing it would discard it for good — commit the work, or remove the \
             worktree under Settings → Storage, before rejecting."
        );

        std::fs::write(checkout.join("two.txt"), "y\n").expect("another stray file");
        let two = review::reject(f.ctx(), Some(f.machine()), &id, "No.")
            .await
            .expect_err("dirty");
        assert_eq!(
            two.to_string(),
            "\"Write the notes\" has 2 uncommitted changes in its worktree, committed nowhere \
             else. Removing it would discard them for good — commit the work, or remove the \
             worktree under Settings → Storage, before rejecting."
        );

        let after = f.reload(&id).await;
        assert!(checkout.exists());
        assert_eq!(f.harness.worktree_path(&id).await, reviewed_worktree);
        assert_eq!(after.branch, reviewed.branch);
        assert_eq!(after.column, BoardColumn::InReview);
        assert_eq!(after.extra_instructions, reviewed.extra_instructions);

        // Request changes keeps the worktree, so the same state is allowed.
        review::request_changes(f.ctx(), &id, "Tidy up.")
            .await
            .expect("uncommitted changes are kept, not discarded");
    }

    #[tokio::test]
    async fn rejecting_without_a_machine_leaves_the_worktree_on_disk() {
        // A server's call until task 054: the worktree's removal is this
        // machine's half, so with no machine it is skipped — and so is the
        // dirty-worktree refusal that guards it. The board write stands alone.
        let f = Fixture::new().await;
        let cli = FakeCli::new();
        let id = f.main_task.clone();
        cli.replays(&id, "success", 0);
        f.run(&cli, &id).await.expect("the run");
        let reviewed = f.reload(&id).await;
        let reviewed_worktree = f.harness.worktree_path(&id).await;
        let checkout = PathBuf::from(reviewed_worktree.clone().expect("a worktree"));
        std::fs::write(checkout.join("stray.txt"), "x\n").expect("a stray file");

        let outcome = review::reject(f.ctx(), None, &id, "Wrong approach.")
            .await
            .expect("no machine, so nothing on disk to refuse over");

        assert_eq!(outcome.task.column, BoardColumn::Ready);
        assert_eq!(outcome.task.branch, None);
        assert_eq!(outcome.set_aside_branch, reviewed.branch);
        assert!(checkout.exists(), "the directory is left on disk");
        assert!(checkout.join("stray.txt").exists(), "and so is its work");
        assert_eq!(f.harness.worktree_path(&id).await, reviewed_worktree);
    }

    #[tokio::test]
    async fn a_rejected_task_whose_worktree_was_already_removed_is_rejected_without_git_errors() {
        let f = Fixture::new().await;
        let cli = FakeCli::new();
        let id = f.main_task.clone();
        cli.replays(&id, "success", 0);
        f.run(&cli, &id).await.expect("the run");
        let checkout = PathBuf::from(f.harness.worktree_path(&id).await.expect("a worktree"));
        std::fs::remove_dir_all(&checkout).expect("remove the directory behind the app's back");

        let outcome = review::reject(f.ctx(), Some(f.machine()), &id, "Start over.")
            .await
            .expect("nothing on disk is nothing to lose");

        assert_eq!(f.harness.worktree_path(&id).await, None);
        assert_eq!(outcome.task.branch, None);
        assert_eq!(outcome.task.column, BoardColumn::Ready);
    }

    #[tokio::test]
    async fn approving_with_auto_cleanup_on_removes_the_worktree_exactly_as_a_drag_to_done_does() {
        let f = Fixture::new().await;
        worktree::set_auto_cleanup(f.machine(), AutoCleanup::OnDoneAcknowledged)
            .await
            .expect("enable auto cleanup");
        let cli = FakeCli::new();
        let id = f.main_task.clone();
        // No commits: automatic removal never forces past unpushed work.
        cli.replays(&id, "success", 0);
        f.run(&cli, &id).await.expect("the run");
        let checkout = PathBuf::from(f.harness.worktree_path(&id).await.expect("a worktree"));
        assert!(checkout.exists());

        let approved = review::approve(f.ctx(), Some(f.machine()), &id)
            .await
            .expect("approve");

        assert_eq!(approved.column, BoardColumn::Done);
        assert!(!checkout.exists());
        assert_eq!(f.harness.worktree_path(&id).await, None);
    }

    #[tokio::test]
    async fn a_refused_reject_leaves_the_worktree_and_extra_instructions_unchanged() {
        let f = Fixture::new().await;
        let cli = FakeCli::new();
        let id = f.main_task.clone();
        cli.commits_a_file_on_attempt(&id, 1, "notes.txt", "Write notes", "success", 0);
        f.run(&cli, &id).await.expect("the run");
        f.set(&id, "plan = NULL, extra_instructions = 'Keep it small.'")
            .await;
        let before = f.reload(&id).await;
        let before_worktree = f.harness.worktree_path(&id).await;
        let checkout = PathBuf::from(before_worktree.clone().expect("a worktree"));

        let error = review::reject(f.ctx(), Some(f.machine()), &id, "Wrong approach.")
            .await
            .expect_err("no plan, so it cannot go back to ready");

        assert_eq!(
            error.to_string(),
            "cannot put \"Write the notes\" in ready without a plan"
        );
        let after = f.reload(&id).await;
        assert!(checkout.exists(), "no git ran before the refusal");
        assert_eq!(f.harness.worktree_path(&id).await, before_worktree);
        assert_eq!(after.branch, before.branch);
        assert_eq!(after.extra_instructions.as_deref(), Some("Keep it small."));
        assert_eq!(after.column, BoardColumn::InReview);
    }

    impl Fixture {
        async fn run(&self, cli: &FakeCli, task_id: &str) -> rimaia_core::Result<Run> {
            let config = RunnerConfig {
                program: cli.program(),
                ..RunnerConfig::default()
            };
            let board = self.harness.board(&self.paths, &config);
            let claim = claim_run(board.as_ref(), task_id, RunTrigger::Queued, false).await?;
            tokio::time::timeout(
                TEST_TIMEOUT,
                run_task(
                    board.as_ref(),
                    self.machine(),
                    &self.paths,
                    &config,
                    claim,
                    RunRequest {
                        cancel: CancelSignal::new(),
                        in_flight: None,
                    },
                ),
            )
            .await
            .expect("a run must finish inside the test timeout")
        }
    }

    fn is_ancestor(dir: &Path, commit: &str, of: &str) -> bool {
        Command::new("git")
            .current_dir(dir)
            .args(["merge-base", "--is-ancestor", commit, of])
            .status()
            .expect("run git merge-base")
            .success()
    }

    fn branch_exists(dir: &Path, branch: &str) -> bool {
        Command::new("git")
            .current_dir(dir)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .status()
            .expect("run git show-ref")
            .success()
    }
}

// ---------------------------------------------------------------------------
// Scaffolding
// ---------------------------------------------------------------------------

/// A registered, opted-in repository with one ready task, and the app data
/// directory its worktrees live under.
struct Fixture {
    harness: TestContext,
    #[allow(dead_code)]
    source: TempRepo,
    _data: TempDir,
    #[allow(dead_code)]
    paths: AppPaths,
    repository_id: String,
    #[allow(dead_code)]
    repository_name: String,
    #[allow(dead_code)]
    main_task: String,
}

impl Fixture {
    async fn new() -> Self {
        let harness = TestContext::new().await;
        let source = TempRepo::init();
        let data = tempfile::Builder::new()
            .prefix("rimaia-data-")
            .tempdir()
            .expect("temp dir for the app data directory");
        let paths = AppPaths::new(data.path());
        paths.create_all().expect("the app data directories");
        let registered = repo::register(
            &harness.context,
            harness.machine(),
            &paths.worktrees_dir(),
            NewRepository {
                path: source.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register the test repository");
        repo::set_allow_unattended_runs(&harness.context, harness.machine(), &registered.id, true)
            .await
            .expect("ADR-0012's per-repository opt-in");

        let mut fixture = Self {
            harness,
            source,
            _data: data,
            paths,
            repository_id: registered.id,
            repository_name: registered.name,
            main_task: String::new(),
        };
        fixture.main_task = fixture.task("Write the notes", BoardColumn::Ready).await;
        fixture
    }

    fn ctx(&self) -> &ServiceContext {
        &self.harness.context
    }

    /// This machine, as the shell hands it to the board services that react on
    /// it (task 041).
    fn machine(&self) -> &rimaia_core::machine::MachineContext {
        self.harness.machine()
    }

    async fn task(&self, title: &str, column: BoardColumn) -> String {
        self.task_in(&self.repository_id, title, column).await
    }

    #[allow(dead_code)]
    async fn task_with_plan(&self, title: &str) -> String {
        self.task(title, BoardColumn::Ready).await
    }

    async fn task_in(&self, repository_id: &str, title: &str, column: BoardColumn) -> String {
        tasks::create_task(
            self.ctx(),
            NewTask {
                repository_id: repository_id.to_string(),
                title: title.to_string(),
                plan: Some("1. Write the notes".to_string()),
                extra_instructions: None,
                column: Some(column),
                links: vec![],
            },
        )
        .await
        .expect("create a task")
        .id
    }

    /// A ready task that depends on `on`.
    async fn dependent(&self, title: &str, on: &str) -> String {
        let id = self.task(title, BoardColumn::Ready).await;
        tasks::set_task_dependencies(self.ctx(), &id, &[on.to_string()])
            .await
            .expect("add the edge");
        id
    }

    /// A second repository that has not opted into unattended runs. Seeded by
    /// row because nothing about it touches git.
    async fn unopted_repository(&self) -> String {
        let id = rimaia_core::db::new_id();
        sqlx::query(
            "INSERT INTO repositories
               (id, team_id, name, path, default_branch, worktree_root, allow_unattended_runs,
                created_at)
             VALUES (?1, ?3, 'unopted', '/tmp/rimaia-unopted', 'main', '/tmp/rimaia-worktrees', 0,
                     ?2)",
        )
        .bind(&id)
        .bind(test_epoch())
        .bind(&self.harness.solo.team_id)
        .execute(&self.ctx().pool)
        .await
        .expect("seed a repository");
        id
    }

    /// Sets columns directly, for the states the services do not offer a door
    /// to (a task `queued` while `in_review`, say).
    async fn set(&self, task_id: &str, assignments: &str) {
        sqlx::query(&format!("UPDATE tasks SET {assignments} WHERE id = ?1"))
            .bind(task_id)
            .execute(&self.ctx().pool)
            .await
            .expect("set task columns");
    }

    async fn reload(&self, task_id: &str) -> Task {
        tasks::get_task(self.ctx(), task_id)
            .await
            .expect("read the task back")
            .task
    }

    /// Everything a refused action must not have touched.
    async fn snapshot(&self, task_id: &str) -> Task {
        self.reload(task_id).await
    }

    async fn column(&self, column: BoardColumn) -> Vec<Task> {
        tasks::list_tasks(
            self.ctx(),
            TaskFilter {
                repository_id: Some(self.repository_id.clone()),
                column: Some(column),
                ..TaskFilter::default()
            },
        )
        .await
        .expect("list a column")
        .into_iter()
        .map(|summary| summary.task)
        .collect()
    }

    async fn decide(&self, verdict: Verdict, task_id: &str, note: &str) -> rimaia_core::Result<()> {
        match verdict {
            Verdict::Approve => review::approve(self.ctx(), Some(self.machine()), task_id)
                .await
                .map(drop),
            Verdict::RequestChanges => review::request_changes(self.ctx(), task_id, note)
                .await
                .map(drop),
            Verdict::Reject => review::reject(self.ctx(), Some(self.machine()), task_id, note)
                .await
                .map(drop),
        }
    }

    async fn marker(&self) -> Option<DateTime<Utc>> {
        review::digest::seen_through(self.ctx())
            .await
            .expect("read the marker")
    }

    fn drain_events(&mut self) -> Vec<ChangeEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.harness.changes.try_recv() {
            events.push(event);
        }
        events
    }

    async fn run_row(&self, row: RunRow) {
        let ended = row.ended_at;
        sqlx::query(
            "INSERT INTO runs
               (id, task_id, attempt, status, session_id, prompt, started_at, ended_at,
                exit_class, error_message, cost_usd, log_path, pr_url, base_ref, base_sha, head_sha,
                kind)
             VALUES (?1, ?2, ?3, ?4, 'session', 'prompt', ?5, ?6, ?7, ?8, ?9, '/tmp/none.jsonl',
                     ?10, ?11, ?12, ?13, ?14)",
        )
        .bind(format!("run-{}-{}", row.task_id, row.attempt))
        .bind(&row.task_id)
        .bind(row.attempt)
        .bind(row.status)
        .bind(row.started_at)
        .bind(ended)
        .bind(row.exit_class)
        .bind(&row.error)
        .bind(row.cost)
        .bind(&row.pr_url)
        .bind(&row.base_ref)
        .bind(&row.base_sha)
        .bind(&row.head_sha)
        .bind(row.kind)
        .execute(&self.ctx().pool)
        .await
        .expect("insert a run row");
    }

    /// One finding on `review_run_id`, written directly: the digest counts
    /// rows, and how a reviewer writes them is `review::findings`' own test.
    async fn finding(&self, task_id: &str, review_run_id: &str, ordinal: i64, status: &str) {
        sqlx::query(
            "INSERT INTO review_findings
               (id, task_id, review_run_id, ordinal, severity, title, body, status, resolution,
                created_at)
             VALUES (?1, ?2, ?3, ?4, 'high', 'A finding', 'Why it matters.', ?5,
                     CASE ?5 WHEN 'open' THEN NULL ELSE 'Done.' END, ?6)",
        )
        .bind(rimaia_core::db::new_id())
        .bind(task_id)
        .bind(review_run_id)
        .bind(ordinal)
        .bind(status)
        .bind(test_epoch())
        .execute(&self.ctx().pool)
        .await
        .expect("insert a finding");
    }
}

/// One `runs` row, written directly: the subject of these tests is what the
/// digest and `built_on` read off rows, not how a run produces them.
struct RunRow {
    task_id: String,
    attempt: i64,
    kind: RunKind,
    status: &'static str,
    exit_class: Option<&'static str>,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    cost: Option<f64>,
    error: Option<String>,
    pr_url: Option<String>,
    base_ref: Option<String>,
    base_sha: Option<String>,
    head_sha: Option<String>,
}

impl RunRow {
    fn new(task_id: &str, attempt: i64) -> Self {
        Self {
            task_id: task_id.to_string(),
            attempt,
            kind: RunKind::Implementation,
            status: "succeeded",
            exit_class: Some("success"),
            started_at: test_epoch() - Duration::minutes(60),
            ended_at: Some(test_epoch() - Duration::minutes(30)),
            cost: Some(1.0),
            error: None,
            pr_url: None,
            base_ref: None,
            base_sha: None,
            head_sha: None,
        }
    }

    fn kind(mut self, kind: RunKind) -> Self {
        self.kind = kind;
        self
    }

    /// Minutes relative to the test epoch.
    fn window(mut self, started: i64, ended: Option<i64>) -> Self {
        self.started_at = test_epoch() + Duration::minutes(started);
        self.ended_at = ended.map(|minutes| test_epoch() + Duration::minutes(minutes));
        self
    }

    fn ended_at(mut self, at: DateTime<Utc>) -> Self {
        self.ended_at = Some(at);
        self
    }

    fn status(mut self, status: &'static str, class: Option<&'static str>) -> Self {
        self.status = status;
        self.exit_class = class;
        self
    }

    fn cost(mut self, cost: Option<f64>) -> Self {
        self.cost = cost;
        self
    }

    fn error(mut self, message: &str) -> Self {
        self.error = Some(message.to_string());
        self
    }

    fn pr(mut self, url: &str) -> Self {
        self.pr_url = Some(url.to_string());
        self
    }

    fn base(mut self, base_ref: &str, base_sha: Option<&str>, head_sha: Option<&str>) -> Self {
        self.base_ref = Some(base_ref.to_string());
        self.base_sha = base_sha.map(str::to_string);
        self.head_sha = head_sha.map(str::to_string);
        self
    }
}

fn counts(nonzero: &[(DigestOutcome, i64)]) -> std::collections::BTreeMap<DigestOutcome, i64> {
    DigestOutcome::ALL
        .into_iter()
        .map(|outcome| {
            let count = nonzero
                .iter()
                .find(|(candidate, _)| *candidate == outcome)
                .map_or(0, |(_, count)| *count);
            (outcome, count)
        })
        .collect()
}

/// Runs git in `dir` and returns trimmed stdout, panicking with both streams on
/// failure.
#[allow(dead_code)]
fn git<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("could not run git in {}: {error}", dir.display()));
    if !output.status.success() {
        panic!(
            "git failed in {}\n{}\n{}",
            dir.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
