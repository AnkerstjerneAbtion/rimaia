//! Closing a row without landing its task, for a test that arranges history
//! rather than exercising the loop (task 035).
//!
//! `runner::outcome::finish_run` closes every kind since task 021 and lands
//! the task by ADR-0017's exits. A test that needs a review or fix row closed
//! *and* the task left where the test put it — a row in the middle of a
//! history it is building — writes it here, directly. Nothing here is a
//! second way to finish a run.
//!
//! This writes the row only. A test that needs the task waiting then calls
//! `tasks::set_run_state(ctx, task_id, RunState::WaitingRetry)`, which is
//! ADR-0011's legal `running → waiting_retry` edge, so the task side still goes
//! through the one function that owns it.

use chrono::{DateTime, Utc};

use crate::context::ServiceContext;
use crate::db::{ExitClass, RunKind, RunStatus};
use crate::review::{FindingSeverity, FindingStatus};

/// Closes `run_id` as ended now (from `ctx.clock`), with `status`,
/// `exit_class` and `resume_after`. Panics on a run that does not exist.
pub async fn close_run(
    ctx: &ServiceContext,
    run_id: &str,
    status: RunStatus,
    exit_class: ExitClass,
    resume_after: Option<DateTime<Utc>>,
) {
    let ended_at = ctx.clock.now();
    let closed = sqlx::query!(
        r#"UPDATE runs SET ended_at = ?1, status = ?2, exit_class = ?3, resume_after = ?4
            WHERE id = ?5"#,
        ended_at,
        status,
        exit_class,
        resume_after,
        run_id,
    )
    .execute(&ctx.pool)
    .await
    .expect("closing a run row");
    assert_eq!(closed.rows_affected(), 1, "no run with id {run_id}");
}

/// A row to arrange in a task's history without running anything (task 037).
pub struct SeededRow<'a> {
    pub kind: RunKind,
    /// Rows of one phase share a session (D29 point 3).
    pub session_id: &'a str,
    pub head_sha: &'a str,
    pub status: RunStatus,
    pub exit_class: Option<ExitClass>,
    /// Whether the row, a review, called `record_review_findings`.
    pub recorded: bool,
}

impl<'a> SeededRow<'a> {
    /// A row that ended well.
    pub fn succeeded(kind: RunKind, session_id: &'a str, head_sha: &'a str) -> Self {
        Self {
            kind,
            session_id,
            head_sha,
            status: RunStatus::Succeeded,
            exit_class: Some(ExitClass::Success),
            recorded: kind == RunKind::Review,
        }
    }
}

/// Appends `row` to `task_id`'s history as the next attempt, and returns its
/// id. Writes the row only: the task's column and run state stay where the
/// test left them, as [`close_run`] does.
pub async fn seed_row(ctx: &ServiceContext, task_id: &str, row: SeededRow<'_>) -> String {
    let id = crate::db::new_id();
    let now = ctx.clock.now();
    let ended_at = (row.status != RunStatus::Running).then_some(now);
    let recorded_at = row.recorded.then_some(now);
    sqlx::query(
        "INSERT INTO runs (id, task_id, attempt, kind, status, session_id, prompt, started_at,
                           ended_at, exit_class, head_sha, findings_recorded_at)
         VALUES (?1, ?2,
                 (SELECT coalesce(max(attempt), 0) + 1 FROM runs WHERE task_id = ?2),
                 ?3, ?4, ?5, 'a prompt', ?6, ?7, ?8, ?9, ?10)",
    )
    .bind(&id)
    .bind(task_id)
    .bind(row.kind)
    .bind(row.status)
    .bind(row.session_id)
    .bind(now)
    .bind(ended_at)
    .bind(row.exit_class)
    .bind(row.head_sha)
    .bind(recorded_at)
    .execute(&ctx.pool)
    .await
    .expect("seeding a run row");
    id
}

/// A finding on `review_run_id`'s report, in whatever state the test wants,
/// bypassing the writers' checks. `resolution` is required for a rejection.
pub struct SeededFinding<'a> {
    pub review_run_id: &'a str,
    pub ordinal: i64,
    pub severity: FindingSeverity,
    pub title: &'a str,
    pub status: FindingStatus,
    pub resolution: Option<&'a str>,
    pub resolved_by_run_id: Option<&'a str>,
}

/// Writes `finding` for `task_id` with the fingerprint the writer would have
/// computed, and returns its id.
pub async fn seed_finding(
    ctx: &ServiceContext,
    task_id: &str,
    finding: SeededFinding<'_>,
) -> String {
    let id = crate::db::new_id();
    let now = ctx.clock.now();
    let file = "src/lib.rs";
    let resolved_at = (finding.status != FindingStatus::Open).then_some(now);
    sqlx::query(
        "INSERT INTO review_findings
            (id, task_id, review_run_id, ordinal, severity, title, body, file, line,
             fingerprint, status, resolution, resolved_by_run_id, created_at, resolved_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'Explained.', ?7, 12, ?8, ?9, ?10, ?11, ?12, ?13)",
    )
    .bind(&id)
    .bind(task_id)
    .bind(finding.review_run_id)
    .bind(finding.ordinal)
    .bind(finding.severity)
    .bind(finding.title)
    .bind(file)
    .bind(crate::review::findings::fingerprint(
        Some(file),
        finding.title,
    ))
    .bind(finding.status)
    .bind(finding.resolution)
    .bind(finding.resolved_by_run_id)
    .bind(now)
    .bind(resolved_at)
    .execute(&ctx.pool)
    .await
    .expect("seeding a finding");
    id
}
