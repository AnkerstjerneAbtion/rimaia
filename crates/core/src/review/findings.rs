//! What a reviewer found, and what a fixer did about it (ADR-0017, task 035).
//!
//! The only writer of `review_findings` and the only reader of
//! `runs.findings_recorded_at`. The MCP tools and the Tauri command are thin
//! adapters over these four functions (ADR-0006).
//!
//! # A clean review is an explicit call
//!
//! A review run records its findings once, with `findings: []` when it found
//! nothing. [`record`] sets `findings_recorded_at` in the same transaction as
//! the rows, including when there are none, so "the review called and found
//! nothing" and "the review never called" are different rows (seam-contract
//! D30 point 7 and its 2026-09-30 amendment). [`recorded_at`] is that witness;
//! task 021 reads it to tell a clean review from a failed one.
//!
//! # Who may write
//!
//! The run id is always the caller's grant, never a request argument, so a run
//! cannot write under another run's id. Here it is checked against the row: a
//! review records only from a running review of the task it names, and a fix
//! resolves only from a running fix of the finding's own task.
//!
//! # The fingerprint, and a rejection that stands (task 021)
//!
//! [`fingerprint`] is "the same finding again": the file and the title,
//! normalised, and never the line, because a fix moves lines. [`record`] is
//! the one writer of `review_findings`, so it is computed there and nowhere
//! else. A reviewer never supplies it: a finding that says which earlier
//! finding it repeats is a reviewer grading its own novelty.
//!
//! A finding whose fingerprint matches one a fix run already rejected on the
//! same task is stored `rejected`, pointing at that rejection, rather than
//! raised again as new. It is kept rather than dropped, so the history shows
//! that the reviewer raised it again, and it never blocks and never reaches a
//! fixer (ADR-0017's amendment of 2026-10-09).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::clock::Clock;
use crate::context::{ScopedTx, ServiceContext};
use crate::db::{new_id, ExitClass, RunKind, RunStatus};
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::review_loop::LoopRow;
use crate::tasks::team_of_task;

/// How much a finding matters, in D28's `CHECK` spelling.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    sqlx::Type,
    schemars::JsonSchema,
)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Critical,
    High,
    Medium,
    Low,
}

impl FindingSeverity {
    /// Whether this severity meets `threshold`, ordered
    /// `critical > high > medium > low`: what "blocking" means against a
    /// loop's `blocking_severity` (ADR-0017).
    pub const fn is_at_least(self, threshold: FindingSeverity) -> bool {
        self.rank() >= threshold.rank()
    }

    const fn rank(self) -> u8 {
        match self {
            FindingSeverity::Critical => 3,
            FindingSeverity::High => 2,
            FindingSeverity::Medium => 1,
            FindingSeverity::Low => 0,
        }
    }
}

/// Where a finding stands, in D28's `CHECK` spelling.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    sqlx::Type,
    schemars::JsonSchema,
)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    Open,
    Fixed,
    Rejected,
}

/// One finding as a reviewer reports it.
///
/// `camelCase`, and every field is one word, so `camelCase` and D16.1's
/// `snake_case` spell them identically and the MCP tool reuses this type as its
/// argument element. A field with two words, if one is ever added, gets its own
/// projection in `mcp/requests.rs` instead.
///
/// `deny_unknown_fields`, as every tool argument is (`mcp::requests`): a
/// reviewer that sends a `fingerprint` is told so rather than having it
/// dropped in silence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewReviewFinding {
    pub severity: FindingSeverity,
    /// One line naming the problem. Must not be blank.
    pub title: String,
    /// What is wrong and why it matters. Must not be blank.
    pub body: String,
    /// Repository-relative path, or absent for the change as a whole.
    #[serde(default)]
    pub file: Option<String>,
    /// A line in `file`. Only meaningful with a file.
    #[serde(default)]
    pub line: Option<i64>,
}

/// One stored finding.
///
/// `Deserialize` because a fix phase is composed from the findings the board
/// hands over in its `RunContext` (seam-contract D31 point 6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFinding {
    pub id: String,
    pub task_id: String,
    pub review_run_id: String,
    /// The finding's index in the call that recorded it, from 0. What orders
    /// one review's findings, because ids say nothing about order and a test
    /// clock gives a whole call one timestamp.
    pub ordinal: i64,
    pub severity: FindingSeverity,
    pub title: String,
    pub body: String,
    pub file: Option<String>,
    pub line: Option<i64>,
    /// [`fingerprint`] of the file and title. `None` only on a row recorded
    /// before task 021 computed it.
    pub fingerprint: Option<String>,
    pub status: FindingStatus,
    /// What the fix run did, or why it declined. Set by [`resolve`].
    pub resolution: Option<String>,
    /// `None` while open, and again if the fix run's row is deleted (D28's
    /// `ON DELETE SET NULL`).
    pub resolved_by_run_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// What a fix run did with one finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingResolution {
    Fixed {
        note: Option<String>,
    },
    /// A rejection always says why: a finding declined without a reason reads
    /// in the morning exactly like one that was ignored.
    Rejected {
        reason: String,
    },
}

/// Records `findings` as `review_run_id`'s whole report, in one transaction.
///
/// Refused unless the run is a running review of `task_id` that has not
/// recorded before, and refused for a blank title or body, or a line with no
/// file. `findings: []` is a clean review, and still sets the witness.
#[tracing::instrument(
    skip_all,
    fields(
        source = ctx.source.as_str(),
        user_id = ctx.actor.as_str(),
        task_id = %task_id,
    )
)]
pub async fn record(
    ctx: &ServiceContext,
    task_id: &str,
    review_run_id: &str,
    findings: Vec<NewReviewFinding>,
) -> Result<Vec<ReviewFinding>> {
    // `BEGIN IMMEDIATE`: the run's checks decide whether anything is written,
    // and a second call racing this one must see the first's witness.
    let mut tx = ctx.begin_immediate().await?;
    let recorded = record_within(
        &mut tx,
        ctx.clock.as_ref(),
        task_id,
        review_run_id,
        findings,
    )
    .await?;
    let team_id = team_of_task(&mut tx, task_id).await?;
    tx.commit().await?;
    ctx.publish(ChangeEvent::tasks(team_id, [task_id.to_string()]));
    Ok(recorded)
}

/// [`record`]'s checks and writes, inside a `BEGIN IMMEDIATE` transaction the
/// caller holds, commits and announces.
///
/// The board port's `record_review_findings` holds the transaction its fence
/// read the lease in (task 043), so a fenced reviewer records nothing.
pub(crate) async fn record_within(
    tx: &mut ScopedTx,
    clock: &dyn Clock,
    task_id: &str,
    review_run_id: &str,
    findings: Vec<NewReviewFinding>,
) -> Result<Vec<ReviewFinding>> {
    for (index, finding) in findings.iter().enumerate() {
        validate_finding(index, finding)?;
    }

    let now = clock.now();
    let run = fetch_writer_run(tx, review_run_id).await?;
    if run.kind != RunKind::Review {
        return Err(Error::invalid(format!(
            "run {review_run_id} is not a review, so it cannot record review findings"
        )));
    }
    if run.task_id != task_id {
        return Err(Error::invalid(format!(
            "review {review_run_id} is not a review of task {task_id}"
        )));
    }
    if run.status != RunStatus::Running {
        return Err(Error::invalid(format!(
            "review {review_run_id} has already ended, so it can no longer record findings"
        )));
    }
    if run.findings_recorded_at.is_some() {
        return Err(Error::invalid(format!(
            "review {review_run_id} has already recorded its findings; a review records once"
        )));
    }

    for (ordinal, finding) in findings.iter().enumerate() {
        let id = new_id();
        let ordinal = ordinal as i64;
        let fingerprint = fingerprint(finding.file.as_deref(), &finding.title);
        let (status, resolution, resolved_at) =
            match standing_rejection(tx, task_id, &fingerprint).await? {
                Some(rejection) => (
                    FindingStatus::Rejected,
                    Some(format!(
                        "Rejected earlier as {}: {}",
                        rejection.id, rejection.reason
                    )),
                    Some(now),
                ),
                None => (FindingStatus::Open, None, None),
            };
        sqlx::query!(
            r#"INSERT INTO review_findings
                (id, task_id, review_run_id, ordinal, severity, title, body, file, line,
                 fingerprint, status, resolution, created_at, resolved_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"#,
            id,
            task_id,
            review_run_id,
            ordinal,
            finding.severity,
            finding.title,
            finding.body,
            finding.file,
            finding.line,
            fingerprint,
            status,
            resolution,
            now,
            resolved_at,
        )
        .execute(&mut **tx)
        .await?;
    }

    sqlx::query!(
        "UPDATE runs SET findings_recorded_at = ?1 WHERE id = ?2",
        now,
        review_run_id,
    )
    .execute(&mut **tx)
    .await?;

    let recorded = sqlx::query_as!(
        ReviewFinding,
        r#"SELECT id AS "id!", task_id, review_run_id, ordinal,
                  severity AS "severity: FindingSeverity", title, body, file, line, fingerprint,
                  status AS "status: FindingStatus", resolution, resolved_by_run_id,
                  created_at AS "created_at: DateTime<Utc>",
                  resolved_at AS "resolved_at: DateTime<Utc>"
             FROM review_findings WHERE review_run_id = ?1 ORDER BY ordinal"#,
        review_run_id,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(recorded)
}

/// Resolves one open finding of `task_id` from `fix_run_id`, a running fix of
/// the same task.
#[tracing::instrument(
    skip_all,
    fields(
        source = ctx.source.as_str(),
        user_id = ctx.actor.as_str(),
        task_id = %task_id,
    )
)]
pub async fn resolve(
    ctx: &ServiceContext,
    task_id: &str,
    finding_id: &str,
    fix_run_id: &str,
    resolution: FindingResolution,
) -> Result<ReviewFinding> {
    // Before any SQL: D28's table `CHECK` is the backstop, not the message.
    let (status, resolution) = match resolution {
        FindingResolution::Fixed { note } => (
            FindingStatus::Fixed,
            note.map(|note| note.trim().to_string())
                .filter(|note| !note.is_empty()),
        ),
        FindingResolution::Rejected { reason } => {
            let reason = reason.trim();
            if reason.is_empty() {
                return Err(Error::invalid(
                    "a finding can only be rejected with a reason saying why",
                ));
            }
            (FindingStatus::Rejected, Some(reason.to_string()))
        }
    };

    let now = ctx.clock.now();
    let mut tx = ctx.begin_immediate().await?;

    let finding = fetch_finding(&mut tx, finding_id).await?;
    if finding.task_id != task_id {
        return Err(Error::invalid(format!(
            "finding {finding_id} is not a finding on task {task_id}"
        )));
    }
    if finding.status != FindingStatus::Open {
        return Err(Error::invalid(format!(
            "finding {finding_id} has already been resolved"
        )));
    }

    let run = fetch_writer_run(&mut tx, fix_run_id).await?;
    if run.kind != RunKind::Fix {
        return Err(Error::invalid(format!(
            "run {fix_run_id} is not a fix, so it cannot resolve review findings"
        )));
    }
    if run.task_id != task_id {
        return Err(Error::invalid(format!(
            "fix {fix_run_id} is not a fix of task {task_id}"
        )));
    }
    if run.status != RunStatus::Running {
        return Err(Error::invalid(format!(
            "fix {fix_run_id} has already ended, so it can no longer resolve findings"
        )));
    }

    sqlx::query!(
        r#"UPDATE review_findings
              SET status = ?1, resolution = ?2, resolved_by_run_id = ?3, resolved_at = ?4
            WHERE id = ?5"#,
        status,
        resolution,
        fix_run_id,
        now,
        finding_id,
    )
    .execute(&mut *tx)
    .await?;

    let resolved = fetch_finding(&mut tx, finding_id).await?;
    let team_id = team_of_task(&mut tx, task_id).await?;
    tx.commit().await?;
    ctx.publish(ChangeEvent::tasks(team_id, [task_id.to_string()]));
    Ok(resolved)
}

/// `task_id`'s findings, optionally of one status, in review order and then in
/// the order the reviewer gave them. A finding inherits its team through its
/// task (ADR-0029 point 1), so only a task in the context's scope has any.
pub async fn list(
    ctx: &ServiceContext,
    task_id: &str,
    status: Option<FindingStatus>,
) -> Result<Vec<ReviewFinding>> {
    let scope = ctx.scope.json();
    let findings = sqlx::query_as!(
        ReviewFinding,
        r#"SELECT f.id AS "id!", f.task_id, f.review_run_id, f.ordinal,
                  f.severity AS "severity: FindingSeverity", f.title, f.body, f.file, f.line,
                  f.fingerprint, f.status AS "status: FindingStatus", f.resolution,
                  f.resolved_by_run_id,
                  f.created_at AS "created_at: DateTime<Utc>",
                  f.resolved_at AS "resolved_at: DateTime<Utc>"
             FROM review_findings f
             JOIN runs r ON r.id = f.review_run_id
             JOIN tasks t ON t.id = f.task_id
            WHERE f.task_id = ?1 AND (?2 IS NULL OR f.status = ?2)
              AND t.team_id IN (SELECT value FROM json_each(?3))
            ORDER BY r.attempt, f.ordinal"#,
        task_id,
        status,
        scope,
    )
    .fetch_all(&ctx.pool)
    .await?;
    Ok(findings)
}

/// `task_id`'s rows as the review loop reads them, oldest first, every kind
/// (task 021).
///
/// Here rather than in `review_loop`, because it reads
/// `findings_recorded_at` and this module stays that column's one reader: a
/// phase has recorded when any of its rows has it set (D30 point 7).
pub async fn loop_rows(ctx: &ServiceContext, task_id: &str) -> Result<Vec<LoopRow>> {
    let scope = ctx.scope.json();
    let rows = sqlx::query!(
        r#"SELECT r.id AS "id!", r.kind AS "kind!: RunKind", r.attempt,
                  r.status AS "status!: RunStatus", r.exit_class AS "exit_class: ExitClass",
                  r.session_id AS "session_id!", r.head_sha,
                  r.findings_recorded_at IS NOT NULL AS "findings_recorded!: bool"
             FROM runs r JOIN tasks t ON t.id = r.task_id
            WHERE r.task_id = ?1 AND t.team_id IN (SELECT value FROM json_each(?2))
            ORDER BY r.attempt"#,
        task_id,
        scope,
    )
    .fetch_all(&ctx.pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| LoopRow {
            id: row.id,
            kind: row.kind,
            attempt: row.attempt,
            status: row.status,
            exit_class: row.exit_class,
            session_id: row.session_id,
            head_sha: row.head_sha,
            findings_recorded: row.findings_recorded,
        })
        .collect())
}

/// [`loop_rows`] for every task still on the board in the context's scope,
/// keyed by task id, in one read: the digest's batched form.
pub async fn loop_rows_on_the_board(ctx: &ServiceContext) -> Result<HashMap<String, Vec<LoopRow>>> {
    let scope = ctx.scope.json();
    let rows = sqlx::query!(
        r#"SELECT r.task_id AS "task_id!", r.id AS "id!", r.kind AS "kind!: RunKind", r.attempt,
                  r.status AS "status!: RunStatus", r.exit_class AS "exit_class: ExitClass",
                  r.session_id AS "session_id!", r.head_sha,
                  r.findings_recorded_at IS NOT NULL AS "findings_recorded!: bool"
             FROM runs r JOIN tasks t ON t.id = r.task_id
            WHERE t.archived_at IS NULL AND t.team_id IN (SELECT value FROM json_each(?1))
            ORDER BY r.task_id, r.attempt"#,
        scope,
    )
    .fetch_all(&ctx.pool)
    .await?;

    let mut by_task: HashMap<String, Vec<LoopRow>> = HashMap::new();
    for row in rows {
        by_task.entry(row.task_id).or_default().push(LoopRow {
            id: row.id,
            kind: row.kind,
            attempt: row.attempt,
            status: row.status,
            exit_class: row.exit_class,
            session_id: row.session_id,
            head_sha: row.head_sha,
            findings_recorded: row.findings_recorded,
        });
    }
    Ok(by_task)
}

/// [`loop_rows`] for the listed tasks, keyed by task id, in one read: what a
/// board read needs to give every card its loop summary (task 037).
///
/// `task_ids` travel as one JSON array bound once, so the statement is the
/// same text for one task or five hundred and never meets SQLite's cap on
/// bound parameters. A task with no rows has no entry.
pub async fn loop_rows_for(
    ctx: &ServiceContext,
    task_ids: &[String],
) -> Result<HashMap<String, Vec<LoopRow>>> {
    let ids = ids_as_json(task_ids)?;
    let scope = ctx.scope.json();
    let rows = sqlx::query!(
        r#"SELECT r.task_id AS "task_id!", r.id AS "id!", r.kind AS "kind!: RunKind", r.attempt,
                  r.status AS "status!: RunStatus", r.exit_class AS "exit_class: ExitClass",
                  r.session_id AS "session_id!", r.head_sha,
                  r.findings_recorded_at IS NOT NULL AS "findings_recorded!: bool"
             FROM runs r JOIN tasks t ON t.id = r.task_id
            WHERE r.task_id IN (SELECT value FROM json_each(?1))
              AND t.team_id IN (SELECT value FROM json_each(?2))
            ORDER BY r.task_id, r.attempt"#,
        ids,
        scope,
    )
    .fetch_all(&ctx.pool)
    .await?;

    let mut by_task: HashMap<String, Vec<LoopRow>> = HashMap::new();
    for row in rows {
        by_task.entry(row.task_id).or_default().push(LoopRow {
            id: row.id,
            kind: row.kind,
            attempt: row.attempt,
            status: row.status,
            exit_class: row.exit_class,
            session_id: row.session_id,
            head_sha: row.head_sha,
            findings_recorded: row.findings_recorded,
        });
    }
    Ok(by_task)
}

/// [`list`] for the listed tasks, keyed by task id and in each task's own
/// review order, in one read (task 037). Shaped like [`loop_rows_for`], for
/// the same reasons.
pub async fn list_for(
    ctx: &ServiceContext,
    task_ids: &[String],
) -> Result<HashMap<String, Vec<ReviewFinding>>> {
    let ids = ids_as_json(task_ids)?;
    let scope = ctx.scope.json();
    let findings = sqlx::query_as!(
        ReviewFinding,
        r#"SELECT f.id AS "id!", f.task_id, f.review_run_id, f.ordinal,
                  f.severity AS "severity: FindingSeverity", f.title, f.body, f.file, f.line,
                  f.fingerprint, f.status AS "status: FindingStatus", f.resolution,
                  f.resolved_by_run_id,
                  f.created_at AS "created_at: DateTime<Utc>",
                  f.resolved_at AS "resolved_at: DateTime<Utc>"
             FROM review_findings f
             JOIN runs r ON r.id = f.review_run_id
             JOIN tasks t ON t.id = f.task_id
            WHERE f.task_id IN (SELECT value FROM json_each(?1))
              AND t.team_id IN (SELECT value FROM json_each(?2))
            ORDER BY f.task_id, r.attempt, f.ordinal"#,
        ids,
        scope,
    )
    .fetch_all(&ctx.pool)
    .await?;

    let mut by_task: HashMap<String, Vec<ReviewFinding>> = HashMap::new();
    for finding in findings {
        by_task
            .entry(finding.task_id.clone())
            .or_default()
            .push(finding);
    }
    Ok(by_task)
}

fn ids_as_json(ids: &[String]) -> Result<String> {
    serde_json::to_string(ids)
        .map_err(|error| Error::internal(format!("task ids did not serialize: {error}")))
}

/// When `review_run_id` recorded its findings: set means the review called,
/// `None` means it did not (D30 point 7). Never inferred from a count of rows,
/// because a clean review's call writes none.
pub async fn recorded_at(
    ctx: &ServiceContext,
    review_run_id: &str,
) -> Result<Option<DateTime<Utc>>> {
    let scope = ctx.scope.json();
    let row = sqlx::query!(
        r#"SELECT r.findings_recorded_at AS "findings_recorded_at: DateTime<Utc>"
             FROM runs r JOIN tasks t ON t.id = r.task_id
            WHERE r.id = ?1 AND t.team_id IN (SELECT value FROM json_each(?2))"#,
        review_run_id,
        scope,
    )
    .fetch_optional(&ctx.pool)
    .await?
    .ok_or_else(|| Error::not_found(format!("no run with id {review_run_id}")))?;
    Ok(row.findings_recorded_at)
}

/// "The same finding again": the file and the title, each normalised, joined
/// by `|`. The line is left out on purpose, because a fix moves lines. A
/// finding about the change as a whole has an empty file half.
///
/// A normalised string rather than a hash (seam-contract D6: no new
/// dependency), so a row stays legible in the `sqlite3` CLI.
pub fn fingerprint(file: Option<&str>, title: &str) -> String {
    format!(
        "{}|{}",
        normalise(file.unwrap_or_default()),
        normalise(title)
    )
}

/// Trimmed, lowercased, and every run of whitespace collapsed to one space.
fn normalise(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A fix run's rejection a new finding repeats.
struct Rejection {
    id: String,
    reason: String,
}

/// The first rejection on `task_id` with this fingerprint, if any.
///
/// The first, in review order, because a finding carried over from it is
/// itself stored `rejected`; pointing at the carry-over would nest one "Rejected
/// earlier as" inside another, where the original names the fixer's reason.
async fn standing_rejection(
    tx: &mut ScopedTx,
    task_id: &str,
    fingerprint: &str,
) -> Result<Option<Rejection>> {
    let row = sqlx::query!(
        r#"SELECT f.id AS "id!", f.resolution AS "resolution!"
             FROM review_findings f
             JOIN runs r ON r.id = f.review_run_id
            WHERE f.task_id = ?1 AND f.fingerprint = ?2 AND f.status = 'rejected'
            ORDER BY r.attempt, f.ordinal
            LIMIT 1"#,
        task_id,
        fingerprint,
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|row| Rejection {
        id: row.id,
        reason: row.resolution,
    }))
}

fn validate_finding(index: usize, finding: &NewReviewFinding) -> Result<()> {
    if finding.title.trim().is_empty() {
        return Err(Error::invalid(format!(
            "finding {index} has a blank title; every finding needs one"
        )));
    }
    if finding.body.trim().is_empty() {
        return Err(Error::invalid(format!(
            "finding {index} has a blank body; every finding needs one"
        )));
    }
    if finding.line.is_some() && finding.file.is_none() {
        return Err(Error::invalid(format!(
            "finding {index} names a line but no file"
        )));
    }
    Ok(())
}

/// The columns of a run that decide whether it may write here.
struct WriterRun {
    task_id: String,
    kind: RunKind,
    status: RunStatus,
    findings_recorded_at: Option<DateTime<Utc>>,
}

/// A run by id, joined to its task in the transaction's scope: a run of
/// another team is as missing as one never opened.
async fn fetch_writer_run(tx: &mut ScopedTx, run_id: &str) -> Result<WriterRun> {
    let scope = tx.scope().json();
    sqlx::query_as!(
        WriterRun,
        r#"SELECT r.task_id, r.kind AS "kind: RunKind", r.status AS "status: RunStatus",
                  r.findings_recorded_at AS "findings_recorded_at: DateTime<Utc>"
             FROM runs r JOIN tasks t ON t.id = r.task_id
            WHERE r.id = ?1 AND t.team_id IN (SELECT value FROM json_each(?2))"#,
        run_id,
        scope,
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| Error::not_found(format!("no run with id {run_id}")))
}

/// A finding by id, joined to its task in the transaction's scope: another
/// team's finding is as missing as one never recorded.
async fn fetch_finding(tx: &mut ScopedTx, finding_id: &str) -> Result<ReviewFinding> {
    let scope = tx.scope().json();
    sqlx::query_as!(
        ReviewFinding,
        r#"SELECT f.id AS "id!", f.task_id, f.review_run_id, f.ordinal,
                  f.severity AS "severity: FindingSeverity", f.title, f.body, f.file, f.line,
                  f.fingerprint, f.status AS "status: FindingStatus", f.resolution,
                  f.resolved_by_run_id,
                  f.created_at AS "created_at: DateTime<Utc>",
                  f.resolved_at AS "resolved_at: DateTime<Utc>"
             FROM review_findings f JOIN tasks t ON t.id = f.task_id
            WHERE f.id = ?1 AND t.team_id IN (SELECT value FROM json_each(?2))"#,
        finding_id,
        scope,
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| Error::not_found(format!("no finding with id {finding_id}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn severities_and_statuses_use_the_schema_spelling() {
        assert_eq!(
            serde_json::to_value([
                FindingSeverity::Critical,
                FindingSeverity::High,
                FindingSeverity::Medium,
                FindingSeverity::Low,
            ])
            .expect("serializes"),
            json!(["critical", "high", "medium", "low"]),
        );
        assert_eq!(
            serde_json::to_value([
                FindingStatus::Open,
                FindingStatus::Fixed,
                FindingStatus::Rejected,
            ])
            .expect("serializes"),
            json!(["open", "fixed", "rejected"]),
        );
    }

    #[test]
    fn a_new_finding_reads_the_same_in_camel_and_snake_case() {
        let wire = json!({
            "severity": "high",
            "title": "Unchecked unwrap",
            "body": "Panics on an empty list.",
            "file": "src/lib.rs",
            "line": 12,
        });
        let finding: NewReviewFinding = serde_json::from_value(wire.clone()).expect("parses");
        assert_eq!(serde_json::to_value(&finding).expect("serializes"), wire);
    }
}
