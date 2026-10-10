//! The overnight digest: what the queue did since the reviewer last finished a
//! review (task 034, seam-contract D29 point 8).
//!
//! **One entry per task, never per run.** A task's outcome comes from its newest
//! row in the window, so a failure that was retried and then succeeded is one
//! `Completed` entry that says it took two runs. Rows only: no git, no worktree,
//! because in team mode the board has no worktree (ADR-0033 point 7).
//!
//! A task's newest row may be a review or a fix (ADR-0017). The entry says
//! which in `last_run_kind`, and its loop numbers are derived from the rows in
//! `review_loop`, never stored (D29 point 8). The totals stay over runs, of
//! every kind, as D29 point 7 counts spend.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::context::{ScopedTx, ServiceContext};
use crate::db::{settings, BoardColumn, RunKind, RunState, RunStatus};
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::machine::MachineContext;
use crate::review::findings;
use crate::review_loop;
use crate::scheduler::selection::{skip_reason, SkipReason};
use crate::tasks::dependencies::compare_dependency_order;
use crate::tasks::{list_tasks, TaskFilter, TaskSummary};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Serialize;

/// How far back a digest looks when no review has ever been finished.
pub const DIGEST_DEFAULT_WINDOW: Duration = Duration::hours(24);

/// The instant through which the reviewer has seen the queue's work. An RFC 3339
/// timestamp, owned by this module in D3's shape and stored by `db::settings`.
/// Its placement is User (D28 part 4).
pub const REVIEW_DIGEST_SEEN_THROUGH: &str = "review_digest_seen_through";

/// What a task did in the window. Declared in attention-first order, which is
/// also the order the digest is returned in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DigestOutcome {
    Failed,
    Blocked,
    WaitingRetry,
    Interrupted,
    Cancelled,
    Running,
    Completed,
    Skipped,
}

impl DigestOutcome {
    pub const ALL: [DigestOutcome; 8] = [
        DigestOutcome::Failed,
        DigestOutcome::Blocked,
        DigestOutcome::WaitingRetry,
        DigestOutcome::Interrupted,
        DigestOutcome::Cancelled,
        DigestOutcome::Running,
        DigestOutcome::Completed,
        DigestOutcome::Skipped,
    ];
}

/// One task's night. Carries no plan text (D16.6).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DigestEntry {
    pub task_id: String,
    pub title: String,
    pub repository_id: String,
    pub column: BoardColumn,
    pub outcome: DigestOutcome,
    /// How many of the task's rows ended in the window. `0` for a context entry
    /// and for an entry whose only row is still open. The two numbers below
    /// cover the same rows.
    pub runs: i64,
    pub run_seconds: Option<i64>,
    /// `None` when there are no such rows **or when any of them has no recorded
    /// cost**: a sum that silently leaves one out understates it (D18).
    pub cost_usd: Option<f64>,
    pub last_run_id: Option<String>,
    pub error_message: Option<String>,
    pub pr_url: Option<String>,
    /// `Blocked` entries only: the dependency in the way.
    pub blocking_title: Option<String>,
    /// `Skipped` entries only.
    pub skip_reason: Option<SkipReason>,
    /// The kind of the newest row, the one [`outcome`](Self::outcome) is taken
    /// from. `None` for an entry with no row in the window. A succeeded review
    /// is `Completed` with `Some(Review)`; what that means for the card is task
    /// 021's.
    pub last_run_kind: Option<RunKind>,
    /// Where the task's review loop stands, or `None` when it has none.
    pub review_loop: Option<DigestLoop>,
}

/// A task's review loop, derived from its rows (D29 point 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DigestLoop {
    /// Review phases after the task's newest implementation phase: which loop
    /// the task is on. A review resumed after a usage limit counts once.
    pub reviews_since_implementation: u32,
    /// The task's `open` findings, of every loop.
    pub open_findings: u32,
}

/// Run totals over every row that ended in the window, and entry counts.
///
/// The two are different things: `counts` is per entry, because `Blocked` and
/// `Skipped` entries have no runs to count. Open runs are in no run total.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DigestTotals {
    pub runs: i64,
    pub run_seconds: i64,
    /// Earliest `started_at` to latest `ended_at` among those rows.
    pub span_seconds: Option<i64>,
    pub cost_usd: f64,
    /// Rows whose cost was never recorded. Counted, never summed as zero (D18).
    pub runs_without_cost: i64,
    /// Entries per outcome, every variant present.
    pub counts: BTreeMap<DigestOutcome, i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Digest {
    /// Exclusive.
    pub since: DateTime<Utc>,
    /// Inclusive: the instant the digest was read. Callers pass it to
    /// [`mark_seen`], so a run that ends between the read and the mark is never
    /// skipped.
    pub until: DateTime<Utc>,
    pub entries: Vec<DigestEntry>,
    pub totals: DigestTotals,
}

/// The context's actor's marker, or `None` when they have not finished a
/// review. An unreadable value reads as absent, as every settings accessor
/// tolerates a hand-edited row.
pub async fn seen_through(ctx: &ServiceContext) -> Result<Option<DateTime<Utc>>> {
    Ok(parse_marker(
        settings::get_user(ctx, REVIEW_DIGEST_SEEN_THROUGH).await?,
    ))
}

fn parse_marker(stored: Option<String>) -> Option<DateTime<Utc>> {
    let raw = stored?;
    match DateTime::parse_from_rfc3339(raw.trim()) {
        Ok(at) => Some(at.with_timezone(&Utc)),
        Err(_) => {
            tracing::warn!(
                value = raw,
                "unreadable review_digest_seen_through; ignoring it"
            );
            None
        }
    }
}

/// Stores `max(current, to)` as the context's actor's marker, through the
/// caller's transaction, and returns what is stored. Nothing moves the marker
/// backwards. Does not publish.
///
/// Always the actor's row: a verdict never moves the marker of whoever
/// triggered the run or owns the card.
pub(crate) async fn advance_marker(
    ctx: &ServiceContext,
    tx: &mut ScopedTx,
    to: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    let current = parse_marker(settings::get_user_in(ctx, tx, REVIEW_DIGEST_SEEN_THROUGH).await?);
    let stored = current.map_or(to, |current| current.max(to));
    settings::set_user_in(
        ctx,
        tx,
        REVIEW_DIGEST_SEEN_THROUGH,
        &stored.to_rfc3339_opts(SecondsFormat::AutoSi, true),
    )
    .await?;
    Ok(stored)
}

/// Marks the digest seen through `through`, which callers take from the digest
/// they showed. Never moves the marker backwards, and refuses an instant that
/// has not happened yet.
#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), user_id = ctx.actor.as_str()))]
pub async fn mark_seen(ctx: &ServiceContext, through: DateTime<Utc>) -> Result<DateTime<Utc>> {
    if through > ctx.clock.now() {
        return Err(Error::invalid(
            "the review digest cannot be marked seen through a time that has not happened yet",
        ));
    }

    // The marker is the actor's user setting, but a change event needs a team
    // until task 048's `Audience::User`, and this call names no entity to take
    // one from: it asks for the context's one team, before anything is
    // written, so a scope of several is refused untouched.
    let team_id = ctx.scope.sole()?.clone();

    // Read and write in one transaction, so two concurrent calls cannot
    // interleave and leave the smaller value behind.
    let mut tx = ctx.begin_immediate().await?;
    let stored = advance_marker(ctx, &mut tx, through).await?;
    tx.commit().await?;

    ctx.publish(ChangeEvent::settings(team_id));
    Ok(stored)
}

/// The window's rows, as the digest reads them.
#[derive(Debug)]
struct WindowRun {
    id: String,
    task_id: String,
    attempt: i64,
    kind: RunKind,
    status: RunStatus,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    cost_usd: Option<f64>,
    error_message: Option<String>,
    pr_url: Option<String>,
}

impl WindowRun {
    fn ended(&self) -> bool {
        self.status != RunStatus::Running && self.ended_at.is_some()
    }
}

/// What the queue did in `(since, until]`, plus what is still running.
///
/// `until` is now; `since` is the marker, or [`DIGEST_DEFAULT_WINDOW`] before it.
///
/// Entity-less, so it reads one team's queue (ADR-0035 point 2): the team whose
/// `in_review` a verdict empties when it advances the marker. The window's
/// start is the actor's own marker.
///
/// "Skipped: unattended runs not allowed" is this runner's consent, which only
/// a caller holding a machine can read (task 066): the shell and the solo MCP
/// server pass `Some`, as they do for the machine reactions. With `None`, a
/// server's call until task 045 puts consent on the board, no task is reported
/// as skipped for consent; the digest never claims a refusal it cannot see.
pub async fn digest(ctx: &ServiceContext, machine: Option<&MachineContext>) -> Result<Digest> {
    let team_id = ctx.scope.sole()?.clone();
    let until = ctx.clock.now();
    let since = seen_through(ctx)
        .await?
        .unwrap_or(until - DIGEST_DEFAULT_WINDOW);

    let rows = sqlx::query_as!(
        WindowRun,
        r#"SELECT r.id AS "id!", r.task_id, r.attempt, r.kind AS "kind: RunKind",
                  r.status AS "status: RunStatus",
                  r.started_at AS "started_at: DateTime<Utc>",
                  r.ended_at AS "ended_at: DateTime<Utc>",
                  r.cost_usd, r.error_message, r.pr_url
             FROM runs r JOIN tasks t ON t.id = r.task_id
            WHERE t.team_id = ?3
              AND t.archived_at IS NULL
              AND (r.status = 'running'
                   OR (r.ended_at IS NOT NULL AND r.ended_at > ?1 AND r.ended_at <= ?2))
            ORDER BY r.task_id, r.attempt"#,
        since,
        until,
        team_id,
    )
    .fetch_all(&ctx.pool)
    .await?;

    // The board read already excludes archived tasks, and carries the blocked
    // flag and `blocking_title` the context entries need.
    let tasks = list_tasks(ctx, TaskFilter::default()).await?;
    let task_by_id: HashMap<&str, &TaskSummary> = tasks
        .iter()
        .map(|summary| (summary.task.id.as_str(), summary))
        .collect();

    let mut rows_by_task: HashMap<&str, Vec<&WindowRun>> = HashMap::new();
    for row in &rows {
        rows_by_task
            .entry(row.task_id.as_str())
            .or_default()
            .push(row);
    }

    let mut entries: Vec<(DigestEntry, &TaskSummary)> = Vec::new();
    for (task_id, task_rows) in &rows_by_task {
        let Some(summary) = task_by_id.get(task_id) else {
            continue;
        };
        entries.push((run_backed_entry(summary, task_rows), summary));
    }

    // Context entries say what the night did not reach. They are gated on there
    // being something it did reach: neither kind depends on the window, so
    // without the gate a repository that never opted in would put an entry in
    // every digest for good, and the marker could never empty it.
    if !entries.is_empty() {
        let consented: Option<BTreeSet<String>> = match machine {
            Some(machine) => Some(crate::machine::consented_repositories(machine).await?),
            None => None,
        };
        for summary in &tasks {
            if summary.task.column != BoardColumn::Ready
                || rows_by_task.contains_key(summary.task.id.as_str())
            {
                continue;
            }
            if let Some(entry) = context_entry(summary, consented.as_ref(), until) {
                entries.push((entry, summary));
            }
        }
    }

    let loops = review_loops(ctx).await?;
    for (entry, _) in &mut entries {
        entry.review_loop = loops.get(entry.task_id.as_str()).copied();
    }

    entries.sort_by(|(left, left_task), (right, right_task)| -> Ordering {
        left.outcome
            .cmp(&right.outcome)
            .then_with(|| compare_dependency_order(&left_task.task, &right_task.task))
    });

    let totals = totals(&rows, &entries);
    Ok(Digest {
        since,
        until,
        entries: entries.into_iter().map(|(entry, _)| entry).collect(),
        totals,
    })
}

fn run_backed_entry(summary: &TaskSummary, rows: &[&WindowRun]) -> DigestEntry {
    let newest = rows
        .iter()
        .max_by_key(|row| row.attempt)
        .expect("a task with window rows has a newest one");

    let outcome = match (newest.status, summary.task.run_state) {
        (RunStatus::Running, _) => DigestOutcome::Running,
        (RunStatus::Succeeded, _) => DigestOutcome::Completed,
        (RunStatus::Failed | RunStatus::Interrupted, RunState::WaitingRetry) => {
            DigestOutcome::WaitingRetry
        }
        (RunStatus::Failed, _) => DigestOutcome::Failed,
        (RunStatus::Interrupted, _) => DigestOutcome::Interrupted,
        (RunStatus::Cancelled, _) => DigestOutcome::Cancelled,
    };

    let ended: Vec<&&WindowRun> = rows.iter().filter(|row| row.ended()).collect();
    let run_seconds = (!ended.is_empty()).then(|| ended.iter().map(|row| seconds(row)).sum());
    let cost_usd = if ended.is_empty() || ended.iter().any(|row| row.cost_usd.is_none()) {
        None
    } else {
        Some(ended.iter().filter_map(|row| row.cost_usd).sum())
    };

    DigestEntry {
        task_id: summary.task.id.clone(),
        title: summary.task.title.clone(),
        repository_id: summary.task.repository_id.clone(),
        column: summary.task.column,
        outcome,
        runs: ended.len() as i64,
        run_seconds,
        cost_usd,
        last_run_id: Some(newest.id.clone()),
        error_message: newest.error_message.clone(),
        pr_url: newest.pr_url.clone(),
        blocking_title: None,
        skip_reason: None,
        last_run_kind: Some(newest.kind),
        review_loop: None,
    }
}

/// Every unarchived task with a review loop to report, keyed by task id.
///
/// Over the task's whole history rather than the window: which loop a task is
/// on does not depend on when the reviewer last looked. Two reads for the
/// digest, not two per entry. The loop number counts review **phases** after
/// the newest implementation phase (D29 point 8 as task 021 amends it), through
/// the same builder `ReviewLoopSummary` uses, so the two cannot disagree about
/// a review that hit a limit and resumed.
async fn review_loops(ctx: &ServiceContext) -> Result<HashMap<String, DigestLoop>> {
    let rows_by_task = findings::loop_rows_on_the_board(ctx).await?;
    let scope = ctx.scope.json();
    let open = sqlx::query!(
        r#"SELECT f.task_id AS "task_id!", count(*) AS "open!: i64"
             FROM review_findings f JOIN tasks t ON t.id = f.task_id
            WHERE t.team_id IN (SELECT value FROM json_each(?1))
              AND t.archived_at IS NULL AND f.status = 'open'
            GROUP BY f.task_id"#,
        scope,
    )
    .fetch_all(&ctx.pool)
    .await?;
    let open: HashMap<String, u32> = open
        .into_iter()
        .map(|row| (row.task_id, u32::try_from(row.open).unwrap_or(u32::MAX)))
        .collect();

    let mut loops = HashMap::new();
    for task_id in rows_by_task.keys().chain(open.keys()) {
        let reviews = rows_by_task
            .get(task_id)
            .and_then(|rows| review_loop::current_loop(rows))
            .map_or(0, |current| current.reviews());
        let open_findings = open.get(task_id).copied().unwrap_or(0);
        if reviews > 0 || open_findings > 0 {
            loops.insert(
                task_id.clone(),
                DigestLoop {
                    reviews_since_implementation: reviews,
                    open_findings,
                },
            );
        }
    }
    Ok(loops)
}

/// A `ready` task the night did not touch, and why not. `skip_reason` is called,
/// not re-implemented.
fn context_entry(
    summary: &TaskSummary,
    consented: Option<&BTreeSet<String>>,
    now: DateTime<Utc>,
) -> Option<DigestEntry> {
    let (outcome, blocking_title, skip) = if summary.blocked_by_incomplete {
        (DigestOutcome::Blocked, summary.blocking_title.clone(), None)
    } else {
        let consented =
            consented.is_none_or(|consented| consented.contains(&summary.task.repository_id));
        match skip_reason(summary, consented, now) {
            Some(
                reason @ (SkipReason::UnattendedRunsNotAllowed
                | SkipReason::NeedsAttention
                | SkipReason::WaitingForRetry),
            ) => (DigestOutcome::Skipped, None, Some(reason)),
            _ => return None,
        }
    };

    Some(DigestEntry {
        task_id: summary.task.id.clone(),
        title: summary.task.title.clone(),
        repository_id: summary.task.repository_id.clone(),
        column: summary.task.column,
        outcome,
        runs: 0,
        run_seconds: None,
        cost_usd: None,
        last_run_id: None,
        error_message: None,
        pr_url: None,
        blocking_title,
        skip_reason: skip,
        last_run_kind: None,
        review_loop: None,
    })
}

fn seconds(row: &WindowRun) -> i64 {
    row.ended_at
        .map_or(0, |ended| (ended - row.started_at).num_seconds())
}

fn totals(rows: &[WindowRun], entries: &[(DigestEntry, &TaskSummary)]) -> DigestTotals {
    let ended: Vec<&WindowRun> = rows.iter().filter(|row| row.ended()).collect();

    let mut counts: BTreeMap<DigestOutcome, i64> = DigestOutcome::ALL
        .into_iter()
        .map(|outcome| (outcome, 0))
        .collect();
    for (entry, _) in entries {
        *counts.entry(entry.outcome).or_default() += 1;
    }

    let earliest = ended.iter().map(|row| row.started_at).min();
    let latest = ended.iter().filter_map(|row| row.ended_at).max();

    DigestTotals {
        runs: ended.len() as i64,
        run_seconds: ended.iter().map(|row| seconds(row)).sum(),
        span_seconds: earliest
            .zip(latest)
            .map(|(from, to)| (to - from).num_seconds()),
        cost_usd: ended.iter().filter_map(|row| row.cost_usd).sum(),
        runs_without_cost: ended.iter().filter(|row| row.cost_usd.is_none()).count() as i64,
        counts,
    }
}
