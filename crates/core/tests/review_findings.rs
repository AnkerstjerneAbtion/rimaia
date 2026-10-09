//! What a reviewer found, and what a fixer did about it (task 035, ADR-0017,
//! seam-contract D28 part 6 and D30 point 7).
//!
//! Run rows are opened through `start_run`, the one writer of `runs`, and the
//! review and fix rows closed through `testing::runs::close_run`, because
//! `finish_run` refuses those kinds until task 021. The clock is the harness's
//! `TestClock`, which returns one instant until a test moves it, and nothing
//! sleeps.

use chrono::{DateTime, Duration, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::db::{BoardColumn, ExitClass, RunKind, RunStatus};
use rimaia_core::review::findings::{self, recorded_at};
use rimaia_core::review::{
    FindingResolution, FindingSeverity, FindingStatus, NewReviewFinding, ReviewFinding,
};
use rimaia_core::runner::outcome::{start_run, NewRun};
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::{self, test_epoch, TestContext};
use rimaia_core::{AppPaths, ChangeEvent, ErrorCode, ServiceContext};
use tempfile::TempDir;

#[tokio::test]
async fn a_clean_review_is_an_explicit_empty_record_call() {
    let f = Fixture::new().await;
    let task = f.task("Reviewed").await;
    let clean = f.open(&task, RunKind::Review).await;
    let silent = f.open(&task, RunKind::Review).await;

    let recorded = findings::record(f.ctx(), &task, &clean, vec![])
        .await
        .expect("a clean review records nothing, explicitly");

    assert_eq!(recorded, vec![]);
    assert_eq!(
        recorded_at(f.ctx(), &clean)
            .await
            .expect("read the witness"),
        Some(test_epoch()),
        "the call is witnessed even though it wrote no row",
    );
    assert_eq!(f.list(&task).await, vec![]);
    assert_eq!(
        recorded_at(f.ctx(), &silent)
            .await
            .expect("read the witness"),
        None,
        "a review that never called has nothing recorded",
    );
}

#[tokio::test]
async fn a_second_record_call_for_the_same_review_is_refused() {
    let f = Fixture::new().await;
    let task = f.task("Reviewed").await;
    let review = f.open(&task, RunKind::Review).await;
    let first = findings::record(f.ctx(), &task, &review, vec![finding("First", None)])
        .await
        .expect("the first call");
    let witnessed = recorded_at(f.ctx(), &review).await.expect("read");

    f.harness.clock.advance(Duration::minutes(5));
    let error = findings::record(f.ctx(), &task, &review, vec![finding("Second", None)])
        .await
        .expect_err("a review records once");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        format!("review {review} has already recorded its findings; a review records once"),
    );
    assert_eq!(
        f.list(&task).await,
        first,
        "the first call's rows are untouched"
    );
    assert_eq!(
        recorded_at(f.ctx(), &review).await.expect("read"),
        witnessed,
        "and so is its timestamp",
    );
}

#[tokio::test]
async fn findings_are_refused_from_a_run_that_is_not_a_running_review_of_this_task() {
    let f = Fixture::new().await;
    let task = f.task("Reviewed").await;
    let other = f.task("Another").await;

    let implementation = f.open(&task, RunKind::Implementation).await;
    let finished = f.open(&task, RunKind::Review).await;
    f.close(&finished).await;
    let elsewhere = f.open(&other, RunKind::Review).await;

    for (run, expected) in [
        (
            &implementation,
            format!("run {implementation} is not a review, so it cannot record review findings"),
        ),
        (
            &finished,
            format!("review {finished} has already ended, so it can no longer record findings"),
        ),
        (
            &elsewhere,
            format!("review {elsewhere} is not a review of task {task}"),
        ),
    ] {
        let error = findings::record(f.ctx(), &task, run, vec![finding("A finding", None)])
            .await
            .expect_err("refused");
        assert_eq!(error.code(), ErrorCode::Invalid);
        assert_eq!(error.to_string(), expected);
        assert_eq!(
            recorded_at(f.ctx(), run).await.expect("read"),
            None,
            "a refusal witnesses nothing",
        );
    }
    assert_eq!(f.list(&task).await, vec![]);
    assert_eq!(f.list(&other).await, vec![]);
}

#[tokio::test]
async fn a_finding_needs_a_title_a_body_and_a_file_for_its_line() {
    let f = Fixture::new().await;
    let task = f.task("Reviewed").await;
    let review = f.open(&task, RunKind::Review).await;

    let blank_title = NewReviewFinding {
        title: "  ".to_string(),
        ..finding("A finding", None)
    };
    let blank_body = NewReviewFinding {
        body: "\n".to_string(),
        ..finding("A finding", None)
    };
    let line_without_file = NewReviewFinding {
        line: Some(4),
        ..finding("A finding", None)
    };

    for (bad, expected) in [
        (
            blank_title,
            "finding 1 has a blank title; every finding needs one",
        ),
        (
            blank_body,
            "finding 1 has a blank body; every finding needs one",
        ),
        (line_without_file, "finding 1 names a line but no file"),
    ] {
        let error = findings::record(
            f.ctx(),
            &task,
            &review,
            vec![finding("Fine", Some("src/lib.rs")), bad],
        )
        .await
        .expect_err("refused before anything is written");
        assert_eq!(error.code(), ErrorCode::Invalid);
        assert_eq!(error.to_string(), expected);
    }
    assert_eq!(f.list(&task).await, vec![]);
    assert_eq!(recorded_at(f.ctx(), &review).await.expect("read"), None);
}

#[tokio::test]
async fn a_fix_run_resolves_a_finding_and_says_what_it_did() {
    let mut f = Fixture::new().await;
    let task = f.task("Reviewed").await;
    let review = f.open(&task, RunKind::Review).await;
    let recorded = findings::record(f.ctx(), &task, &review, vec![finding("Fix me", None)])
        .await
        .expect("record");
    f.close(&review).await;
    let fix = f.open(&task, RunKind::Fix).await;
    f.drain();

    f.harness.clock.advance(Duration::minutes(3));
    let resolved = findings::resolve(
        f.ctx(),
        &task,
        &recorded[0].id,
        &fix,
        FindingResolution::Fixed {
            note: Some("  Checked the length first.  ".to_string()),
        },
    )
    .await
    .expect("resolve");

    assert_eq!(resolved.status, FindingStatus::Fixed);
    assert_eq!(
        resolved.resolution.as_deref(),
        Some("Checked the length first.")
    );
    assert_eq!(resolved.resolved_by_run_id.as_deref(), Some(fix.as_str()));
    assert_eq!(
        resolved.resolved_at,
        Some(test_epoch() + Duration::minutes(3))
    );
    assert_eq!(f.list(&task).await, vec![resolved]);
    assert!(
        f.drain().contains(&ChangeEvent::tasks([task.clone()])),
        "get_task's readers hear about it",
    );
}

#[tokio::test]
async fn rejecting_a_finding_requires_a_reason() {
    let f = Fixture::new().await;
    let task = f.task("Reviewed").await;
    let (finding_id, fix) = f.one_open_finding(&task).await;

    for blank in ["", "   "] {
        let error = findings::resolve(
            f.ctx(),
            &task,
            &finding_id,
            &fix,
            FindingResolution::Rejected {
                reason: blank.to_string(),
            },
        )
        .await
        .expect_err("a rejection says why");
        assert_eq!(error.code(), ErrorCode::Invalid);
        assert_eq!(
            error.to_string(),
            "a finding can only be rejected with a reason saying why"
        );
    }

    let rejected = findings::resolve(
        f.ctx(),
        &task,
        &finding_id,
        &fix,
        FindingResolution::Rejected {
            reason: "The reviewer misread the loop.".to_string(),
        },
    )
    .await
    .expect("a rejection with a reason");
    assert_eq!(rejected.status, FindingStatus::Rejected);
    assert_eq!(
        rejected.resolution.as_deref(),
        Some("The reviewer misread the loop.")
    );
}

#[tokio::test]
async fn a_resolved_finding_cannot_be_resolved_again() {
    let f = Fixture::new().await;
    let task = f.task("Reviewed").await;
    let (finding_id, fix) = f.one_open_finding(&task).await;
    let resolved = findings::resolve(
        f.ctx(),
        &task,
        &finding_id,
        &fix,
        FindingResolution::Fixed { note: None },
    )
    .await
    .expect("the first resolution");

    let error = findings::resolve(
        f.ctx(),
        &task,
        &finding_id,
        &fix,
        FindingResolution::Rejected {
            reason: "Changed my mind.".to_string(),
        },
    )
    .await
    .expect_err("a finding is resolved once");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        format!("finding {finding_id} has already been resolved")
    );
    assert_eq!(f.list(&task).await, vec![resolved]);
}

#[tokio::test]
async fn a_fix_run_cannot_resolve_another_tasks_finding() {
    let f = Fixture::new().await;
    let task = f.task("Reviewed").await;
    let other = f.task("Another").await;
    let (finding_id, _) = f.one_open_finding(&task).await;
    let other_fix = f.open(&other, RunKind::Fix).await;
    let before = f.list(&task).await;

    // Named as its own task: the finding is not on it.
    let error = findings::resolve(
        f.ctx(),
        &other,
        &finding_id,
        &other_fix,
        FindingResolution::Fixed { note: None },
    )
    .await
    .expect_err("another task's finding");
    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        format!("finding {finding_id} is not a finding on task {other}")
    );

    // Named as the finding's task: the fix is not that task's.
    let error = findings::resolve(
        f.ctx(),
        &task,
        &finding_id,
        &other_fix,
        FindingResolution::Fixed { note: None },
    )
    .await
    .expect_err("another task's fix");
    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        format!("fix {other_fix} is not a fix of task {task}")
    );

    assert_eq!(f.list(&task).await, before);
}

#[tokio::test]
async fn only_a_running_fix_resolves_a_finding() {
    let f = Fixture::new().await;
    let task = f.task("Reviewed").await;
    let (finding_id, fix) = f.one_open_finding(&task).await;
    let review = f.open(&task, RunKind::Review).await;
    f.close(&fix).await;

    for (run, expected) in [
        (
            &review,
            format!("run {review} is not a fix, so it cannot resolve review findings"),
        ),
        (
            &fix,
            format!("fix {fix} has already ended, so it can no longer resolve findings"),
        ),
    ] {
        let error = findings::resolve(
            f.ctx(),
            &task,
            &finding_id,
            run,
            FindingResolution::Fixed { note: None },
        )
        .await
        .expect_err("refused");
        assert_eq!(error.code(), ErrorCode::Invalid);
        assert_eq!(error.to_string(), expected);
    }
    assert_eq!(f.list(&task).await[0].status, FindingStatus::Open);
}

#[tokio::test]
async fn findings_list_in_review_order_then_in_the_order_the_reviewer_gave_them() {
    // The harness clock returns one instant for every call, ids are UUIDs, and
    // the titles are given in an order neither would sort into.
    let f = Fixture::new().await;
    let task = f.task("Reviewed twice").await;
    let first = f.open(&task, RunKind::Review).await;
    findings::record(
        f.ctx(),
        &task,
        &first,
        vec![finding("Zeta", None), finding("Alpha", Some("src/a.rs"))],
    )
    .await
    .expect("the first review");
    f.close(&first).await;
    let fix = f.open(&task, RunKind::Fix).await;
    f.close(&fix).await;
    let second = f.open(&task, RunKind::Review).await;
    findings::record(
        f.ctx(),
        &task,
        &second,
        vec![
            finding("Mu", None),
            finding("Beta", None),
            finding("Omega", None),
        ],
    )
    .await
    .expect("the second review");

    let listed = f.list(&task).await;

    let titles: Vec<&str> = listed.iter().map(|found| found.title.as_str()).collect();
    assert_eq!(titles, vec!["Zeta", "Alpha", "Mu", "Beta", "Omega"]);
    let ordinals: Vec<(String, i64)> = listed
        .iter()
        .map(|found| (found.review_run_id.clone(), found.ordinal))
        .collect();
    assert_eq!(
        ordinals,
        vec![
            (first.clone(), 0),
            (first.clone(), 1),
            (second.clone(), 0),
            (second.clone(), 1),
            (second.clone(), 2),
        ]
    );
    assert!(listed.iter().all(|found| found.created_at == test_epoch()));
    // Computed by the store from the file and title, never supplied by the
    // reviewer (task 021).
    let prints: Vec<Option<&str>> = listed
        .iter()
        .map(|found| found.fingerprint.as_deref())
        .collect();
    assert_eq!(
        prints,
        vec![
            Some("|zeta"),
            Some("src/a.rs|alpha"),
            Some("|mu"),
            Some("|beta"),
            Some("|omega"),
        ]
    );

    let open = findings::list(f.ctx(), &task, Some(FindingStatus::Open))
        .await
        .expect("list the open ones");
    assert_eq!(open, listed);
    let fixed = findings::list(f.ctx(), &task, Some(FindingStatus::Fixed))
        .await
        .expect("list the fixed ones");
    assert_eq!(fixed, vec![]);
}

#[test]
fn fingerprint_ignores_line_case_and_whitespace() {
    // The line is left out because a fix moves lines; case and whitespace
    // because a second reviewer phrases the same title its own way.
    assert_eq!(
        findings::fingerprint(Some("  src/Retry.rs "), "The retry\n never   STOPS "),
        "src/retry.rs|the retry never stops",
    );
    assert_eq!(
        findings::fingerprint(Some("src/retry.rs"), "The retry never stops"),
        findings::fingerprint(Some("src/retry.rs"), "the RETRY never stops"),
    );
    assert_eq!(
        findings::fingerprint(None, "About the whole change"),
        "|about the whole change",
        "a finding with no file has an empty file half",
    );
    assert_ne!(
        findings::fingerprint(Some("src/a.rs"), "Same title"),
        findings::fingerprint(Some("src/b.rs"), "Same title"),
    );
}

#[tokio::test]
async fn a_finding_a_fix_rejected_is_stored_rejected_when_raised_again() {
    let f = Fixture::new().await;
    let task = f.task("Reviewed twice").await;
    let first = f.open(&task, RunKind::Review).await;
    let raised = findings::record(
        f.ctx(),
        &task,
        &first,
        vec![NewReviewFinding {
            line: Some(12),
            ..finding("Unchecked unwrap", Some("src/lib.rs"))
        }],
    )
    .await
    .expect("the first review");
    f.close(&first).await;
    let fix = f.open(&task, RunKind::Fix).await;
    findings::resolve(
        f.ctx(),
        &task,
        &raised[0].id,
        &fix,
        FindingResolution::Rejected {
            reason: "The list is never empty here.".to_string(),
        },
    )
    .await
    .expect("the fixer rejects it");
    f.close(&fix).await;

    // The same finding, on another line and in another case.
    let second = f.open(&task, RunKind::Review).await;
    let again = findings::record(
        f.ctx(),
        &task,
        &second,
        vec![
            NewReviewFinding {
                line: Some(40),
                ..finding("UNCHECKED  unwrap", Some("src/lib.rs"))
            },
            finding("Something new", None),
        ],
    )
    .await
    .expect("the second review");

    assert_eq!(again[0].status, FindingStatus::Rejected);
    assert_eq!(
        again[0].resolution,
        Some(format!(
            "Rejected earlier as {}: The list is never empty here.",
            raised[0].id
        ))
    );
    assert_eq!(again[0].resolved_by_run_id, None, "no fix run resolved it");
    assert_eq!(again[0].resolved_at, Some(test_epoch()));
    assert_eq!(again[1].status, FindingStatus::Open);
    assert_eq!(again[1].resolution, None);
}

#[tokio::test]
async fn deleting_a_task_deletes_its_findings_and_a_deleted_fix_unlinks_its_resolution() {
    // D28's `ON DELETE` clauses, asserted rather than assumed.
    let f = Fixture::new().await;
    let task = f.task("Reviewed").await;
    let (finding_id, fix) = f.one_open_finding(&task).await;
    findings::resolve(
        f.ctx(),
        &task,
        &finding_id,
        &fix,
        FindingResolution::Fixed { note: None },
    )
    .await
    .expect("resolve");

    sqlx::query("DELETE FROM runs WHERE id = ?1")
        .bind(&fix)
        .execute(&f.ctx().pool)
        .await
        .expect("delete the fix run's row");
    let after = f.list(&task).await;
    assert_eq!(after.len(), 1, "the finding outlives the fix run");
    assert_eq!(after[0].resolved_by_run_id, None);
    assert_eq!(after[0].status, FindingStatus::Fixed);

    tasks::delete_task(f.ctx(), &task)
        .await
        .expect("delete the task");
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM review_findings")
        .fetch_one(&f.ctx().pool)
        .await
        .expect("count findings");
    assert_eq!(left, 0);
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

fn finding(title: &str, file: Option<&str>) -> NewReviewFinding {
    NewReviewFinding {
        severity: FindingSeverity::Medium,
        title: title.to_string(),
        body: format!("{title}, explained."),
        file: file.map(str::to_string),
        line: file.map(|_| 7),
    }
}

struct Fixture {
    harness: TestContext,
    _data: TempDir,
    paths: AppPaths,
    repository_id: String,
}

impl Fixture {
    async fn new() -> Self {
        let harness = TestContext::new().await;
        let data = tempfile::Builder::new()
            .prefix("rimaia-findings-")
            .tempdir()
            .expect("a temporary app data directory");
        let paths = AppPaths::new(data.path());
        let repository_id = rimaia_core::db::new_id();
        sqlx::query(
            "INSERT INTO repositories
               (id, name, path, default_branch, worktree_root, allow_unattended_runs, created_at)
             VALUES (?1, 'rimaia', '/tmp/rimaia', 'main', '/tmp/rimaia-worktrees', 0, ?2)",
        )
        .bind(&repository_id)
        .bind(test_epoch())
        .execute(&harness.context.pool)
        .await
        .expect("seed a repository");
        Self {
            harness,
            _data: data,
            paths,
            repository_id,
        }
    }

    fn ctx(&self) -> &ServiceContext {
        &self.harness.context
    }

    async fn task(&self, title: &str) -> String {
        tasks::create_task(
            self.ctx(),
            NewTask {
                repository_id: self.repository_id.clone(),
                title: title.to_string(),
                plan: Some("1. Do it".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a task")
        .id
    }

    async fn open(&self, task_id: &str, kind: RunKind) -> String {
        start_run(
            self.ctx(),
            &self.paths,
            NewRun {
                task_id: task_id.to_string(),
                kind,
                session_id: rimaia_core::db::new_id(),
                prompt: "a prompt".to_string(),
                base_ref: None,
                base_sha: None,
            },
        )
        .await
        .expect("open a run row")
        .id
    }

    async fn close(&self, run_id: &str) {
        testing::runs::close_run(
            self.ctx(),
            run_id,
            RunStatus::Succeeded,
            ExitClass::Success,
            None::<DateTime<Utc>>,
        )
        .await;
    }

    async fn list(&self, task_id: &str) -> Vec<ReviewFinding> {
        findings::list(self.ctx(), task_id, None)
            .await
            .expect("list the findings")
    }

    /// One open finding from a closed review, and a running fix to resolve it.
    async fn one_open_finding(&self, task_id: &str) -> (String, String) {
        let review = self.open(task_id, RunKind::Review).await;
        let recorded =
            findings::record(self.ctx(), task_id, &review, vec![finding("Fix me", None)])
                .await
                .expect("record");
        self.close(&review).await;
        let fix = self.open(task_id, RunKind::Fix).await;
        (recorded[0].id.clone(), fix)
    }

    fn drain(&mut self) -> Vec<ChangeEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.harness.changes.try_recv() {
            events.push(event);
        }
        events
    }
}
