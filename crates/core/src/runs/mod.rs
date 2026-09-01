//! Run history: per-task and global listings, one run's detail, and log
//! housekeeping (task 015, ADR-0013).
//!
//! This module is reads (and, for [`prune_logs`], deletes on disk) — it is
//! not a third writer of the `runs` table. [`crate::runner::outcome`] is
//! still the only thing that inserts or updates a row (ADR-0006), and the
//! diff and commits a run detail view opens with are
//! [`crate::worktree::diff_summary`]'s, re-read fresh rather than duplicated
//! here: every attempt of a task shares one branch (ADR-0005), so there is
//! one diff to show regardless of which attempt a reviewer opened, and it is
//! the branch's current state, not a snapshot frozen at that attempt's end.
//!
//! # "Marked, not trusted" without a new column
//!
//! ADR-0013 says a `runs` row pointing at a missing transcript "is marked,
//! not trusted" the same way `startup::survey`'s `missing_worktrees` is — but
//! the seam contract caps the migration count and this task adds none. So
//! there is no persisted flag: [`get_run`] and [`list_runs`] both compute
//! `log_available` fresh, with `tokio::fs::try_exists`, on every read. That
//! satisfies the rule the same way [`crate::worktree::WorktreeStatus::exists`]
//! does for a worktree — a fact that can change between two reads has no
//! business being cached in a row nobody re-validates.
//!
//! # Every new query here is hand-built SQL, not `query_as!`
//!
//! [`crate::tasks::list_tasks`] already explains why a dynamic filter has to
//! be: the macro needs a query fixed at compile time. This module also uses
//! the hand-built form for its *fixed*-shape queries — a deliberate,
//! narrower choice than that precedent, recorded here rather than at each
//! call site. `query_as!`/`query!` are checked against `.sqlx`'s committed
//! cache in CI (`SQLX_OFFLINE=true`), regenerated with `cargo sqlx prepare`;
//! every query in this module decodes through [`sqlx::FromRow`] instead
//! (`Run` and [`Task`](crate::db::Task) already derive it for exactly this
//! reason — see [`crate::tasks::service::TaskSummary`]'s own hand-rolled impl
//! for the precedent of mixing a derived one with extra joined columns).

pub mod transcript;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::sqlite::SqliteRow;
use sqlx::{FromRow, Row};

use crate::context::ServiceContext;
use crate::db::{Run, RunStatus};
use crate::error::{Error, Result};
use crate::paths::AppPaths;
use crate::worktree::{self, DiffSummary};

// ---------------------------------------------------------------------------
// Listing — per task, and the global filtered view
// ---------------------------------------------------------------------------

/// One run as the global history view renders it: the row, plus the task
/// title and repository name a list spanning every repository needs to be
/// readable. A per-task list has no need of either — it already knows which
/// task and which repository it is looking at — so [`list_runs_for_task`]
/// returns a bare [`Run`] instead.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunListEntry {
    #[serde(flatten)]
    pub run: Run,
    pub task_title: String,
    pub repository_id: String,
    pub repository_name: String,
    /// Computed fresh on every read — see this module's own doc on why there
    /// is no stored flag to read instead.
    pub log_available: bool,
}

/// Hand-written for the reason [`crate::tasks::service::TaskSummary`]'s own
/// impl is: `Run::from_row` reads only its own fifteen columns by name and
/// ignores whatever else the row carries, which is exactly the three joined
/// columns this query adds.
impl<'r> FromRow<'r, SqliteRow> for RunListEntry {
    fn from_row(row: &'r SqliteRow) -> sqlx::Result<Self> {
        Ok(RunListEntry {
            run: Run::from_row(row)?,
            task_title: row.try_get("task_title")?,
            repository_id: row.try_get("repository_id")?,
            repository_name: row.try_get("repository_name")?,
            // Filled in by `list_runs` after the query — deciding it needs an
            // `await`, which `FromRow` cannot perform.
            log_available: false,
        })
    }
}

/// What [`list_runs`] filters on. A field left `None` matches everything;
/// combining fields narrows the result, mirroring
/// [`crate::tasks::types::TaskFilter`]'s own contract.
#[derive(Debug, Clone, Default)]
pub struct RunFilter {
    pub repository_id: Option<String>,
    pub status: Option<RunStatus>,
    /// Matches a run started at or after this instant.
    pub since: Option<DateTime<Utc>>,
    /// Matches a run started at or before this instant.
    pub until: Option<DateTime<Utc>>,
}

/// The global read's join, up to its `WHERE`, which [`list_runs`] appends its
/// optional filters to — the same shape
/// [`crate::tasks::service::TASK_SUMMARY_SELECT`] uses for the board.
///
/// Inner joins throughout: a run's `task_id` and a task's `repository_id` are
/// both `NOT NULL` foreign keys (ADR-0003), so a run with no task or a task
/// with no repository is not a row this view is ever asked to explain.
const RUN_LIST_SELECT: &str = r#"
SELECT r.*,
       t.title AS task_title,
       t.repository_id AS repository_id,
       rep.name AS repository_name
  FROM runs r
  JOIN tasks t ON t.id = r.task_id
  JOIN repositories rep ON rep.id = t.repository_id
 WHERE 1 = 1"#;

/// Every run matching `filter`, newest first — the global Runs view's
/// history list, filterable by repository, outcome and date range.
pub async fn list_runs(ctx: &ServiceContext, filter: RunFilter) -> Result<Vec<RunListEntry>> {
    let mut sql = String::from(RUN_LIST_SELECT);
    if filter.repository_id.is_some() {
        sql.push_str(" AND rep.id = ?");
    }
    if filter.status.is_some() {
        sql.push_str(" AND r.status = ?");
    }
    if filter.since.is_some() {
        sql.push_str(" AND r.started_at >= ?");
    }
    if filter.until.is_some() {
        sql.push_str(" AND r.started_at <= ?");
    }
    sql.push_str(" ORDER BY r.started_at DESC");

    let mut query = sqlx::query_as::<_, RunListEntry>(&sql);
    if let Some(repository_id) = filter.repository_id {
        query = query.bind(repository_id);
    }
    if let Some(status) = filter.status {
        query = query.bind(status);
    }
    if let Some(since) = filter.since {
        query = query.bind(since);
    }
    if let Some(until) = filter.until {
        query = query.bind(until);
    }

    let mut entries = query.fetch_all(&ctx.pool).await?;
    for entry in &mut entries {
        entry.log_available = log_file_exists(&entry.run.log_path).await;
    }
    Ok(entries)
}

/// Every run of `task_id`, newest attempt first — the task detail panel's
/// history list.
pub async fn list_runs_for_task(ctx: &ServiceContext, task_id: &str) -> Result<Vec<Run>> {
    let runs =
        sqlx::query_as::<_, Run>("SELECT * FROM runs WHERE task_id = ?1 ORDER BY attempt DESC")
            .bind(task_id)
            .fetch_all(&ctx.pool)
            .await?;
    Ok(runs)
}

// ---------------------------------------------------------------------------
// One run's detail — ADR-0013's ordering, in one read
// ---------------------------------------------------------------------------

/// What a run detail view opens on, in ADR-0013's order: the run's own
/// outcome (on [`run`](Self::run)), then the branch's diff and commits, then
/// the PR link (`run.pr_url`) and the exact prompt (`run.prompt`) — all
/// already on the row. The transcript itself is read separately, page by
/// page, through [`transcript::read_page`]; a 50MB file has no business
/// riding along on this struct.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunDetail {
    #[serde(flatten)]
    pub run: Run,
    pub diff: DiffSummary,
    pub log_available: bool,
}

/// The full detail read for one run.
pub async fn get_run(ctx: &ServiceContext, run_id: &str) -> Result<RunDetail> {
    let run = fetch_run(ctx, run_id).await?;
    let diff = worktree::diff_summary(ctx, &run.task_id).await?;
    let log_available = log_file_exists(&run.log_path).await;

    Ok(RunDetail {
        run,
        diff,
        log_available,
    })
}

/// The bare row, for a caller that needs only `log_path` (or another single
/// column) and would otherwise pay for [`get_run`]'s `diff_summary` git calls
/// just to discard the result — the Tauri shell's transcript and "open raw
/// log" commands are exactly this caller.
pub async fn get_run_row(ctx: &ServiceContext, run_id: &str) -> Result<Run> {
    fetch_run(ctx, run_id).await
}

async fn fetch_run(ctx: &ServiceContext, run_id: &str) -> Result<Run> {
    sqlx::query_as::<_, Run>("SELECT * FROM runs WHERE id = ?1")
        .bind(run_id)
        .fetch_optional(&ctx.pool)
        .await?
        .ok_or_else(|| Error::not_found(format!("no run with id {run_id}")))
}

/// True only for a clean "not found" — [`crate::startup::survey`]'s own
/// `path_is_missing` draws the identical distinction and for the identical
/// reason: a stat that failed because a network volume has not mounted yet
/// is not a transcript that was deleted.
async fn log_file_exists(log_path: &str) -> bool {
    !matches!(tokio::fs::try_exists(log_path).await, Ok(false))
}

// ---------------------------------------------------------------------------
// Housekeeping — total size and pruning (ADR-0013's "Retention")
// ---------------------------------------------------------------------------

/// Total bytes on disk across every task's run logs, for Settings' storage
/// report alongside worktree size.
///
/// Walks `paths.runs_dir()` rather than summing `runs.log_path` from the
/// database: a transcript belongs to its task's own subdirectory regardless
/// of whether the row that named it still exists, and disk usage is a
/// question about the filesystem, not about SQLite.
pub async fn total_log_size(paths: &AppPaths) -> u64 {
    let mut total = 0u64;
    let Ok(mut task_dirs) = tokio::fs::read_dir(paths.runs_dir()).await else {
        // No `runs` directory yet, or it cannot be read — either way there is
        // nothing to report, and this is a size, not a fallible operation.
        return 0;
    };

    while let Ok(Some(task_dir)) = task_dirs.next_entry().await {
        if !matches!(task_dir.file_type().await, Ok(file_type) if file_type.is_dir()) {
            continue;
        }
        let Ok(mut files) = tokio::fs::read_dir(task_dir.path()).await else {
            continue;
        };
        while let Ok(Some(file)) = files.next_entry().await {
            if let Ok(metadata) = file.metadata().await {
                if metadata.is_file() {
                    total += metadata.len();
                }
            }
        }
    }

    total
}

/// What [`prune_logs`] removes: every closed run older than an age, or every
/// log belonging to one task. Nothing else — there is deliberately no "prune
/// everything", because that button has no undo and ADR-0013's Retention
/// section names exactly these two.
#[derive(Debug, Clone)]
pub enum PruneCriterion {
    /// Runs whose `started_at` is more than this many days before
    /// [`ServiceContext::clock`]'s current instant — resolved against the
    /// clock inside [`prune_logs`] itself, rather than a `DateTime` the
    /// caller computed, so a test controls "now" the same way every other
    /// service test does (`CLAUDE.md`'s "fake the clock" rule) instead of
    /// reaching around it with a literal cutoff nothing validates. A run
    /// still in flight (`ended_at IS NULL`) is never a candidate regardless
    /// of how long ago it started — its transcript is the only record of
    /// work that has not finished yet.
    OlderThanDays(i64),
    Task(String),
}

/// What one prune actually removed, so Settings can report it and refresh
/// [`total_log_size`] against a number that agrees with what just happened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneResult {
    pub runs_pruned: u64,
    pub bytes_freed: u64,
}

/// Deletes transcript and stderr files matching `criterion`, leaving every
/// `runs` row untouched.
///
/// The row survives on purpose. A pruned run's outcome, diff and commits are
/// still real history — only its evidence file is gone — and its
/// `log_path` no longer resolving is exactly the "log unavailable" state
/// [`get_run`] already renders rather than errors on. Pruning is simply the
/// deliberate, user-requested version of the same condition startup
/// reconciliation finds by accident.
pub async fn prune_logs(ctx: &ServiceContext, criterion: PruneCriterion) -> Result<PruneResult> {
    let log_paths: Vec<String> = match criterion {
        PruneCriterion::OlderThanDays(days) => {
            let cutoff = ctx.clock.now() - chrono::Duration::days(days);
            sqlx::query_scalar(
                "SELECT log_path FROM runs WHERE started_at < ?1 AND ended_at IS NOT NULL",
            )
            .bind(cutoff)
            .fetch_all(&ctx.pool)
            .await?
        }
        PruneCriterion::Task(task_id) => {
            // The same `ended_at IS NOT NULL` guard the by-age branch carries,
            // and for the identical reason: an unfinished run's transcript is
            // the only record of work still in progress. Without it, "Prune
            // this task's logs" clicked during a run deletes the file the
            // runner is writing to — and this is the criterion the panel
            // exposes as a button, so it is the reachable one.
            sqlx::query_scalar(
                "SELECT log_path FROM runs WHERE task_id = ?1 AND ended_at IS NOT NULL",
            )
            .bind(task_id)
            .fetch_all(&ctx.pool)
            .await?
        }
    };

    let mut result = PruneResult::default();
    for log_path in log_paths {
        result.bytes_freed += remove_log_files(Path::new(&log_path)).await;
        result.runs_pruned += 1;
    }
    Ok(result)
}

/// Removes one run's transcript and, if it was ever written, its stderr log
/// beside it — `runner::events::stderr_path`'s own naming, re-derived from
/// `log_path` rather than imported, because this module only has the stored
/// path string and not the `AppPaths`/task id/run id triple that function
/// takes.
async fn remove_log_files(log_path: &Path) -> u64 {
    let mut freed = remove_file_if_exists(log_path).await;
    if let Some(stderr_path) = sibling_stderr_path(log_path) {
        freed += remove_file_if_exists(&stderr_path).await;
    }
    freed
}

fn sibling_stderr_path(log_path: &Path) -> Option<PathBuf> {
    let stem = log_path.file_stem()?.to_str()?;
    Some(log_path.with_file_name(format!("{stem}.stderr.log")))
}

async fn remove_file_if_exists(path: &Path) -> u64 {
    let size = tokio::fs::metadata(path)
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    match tokio::fs::remove_file(path).await {
        Ok(()) => size,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{new_id, ExitClass};
    use crate::testing::{TempRepo, TestContext};
    use pretty_assertions::assert_eq;

    /// A registered repository whose `path` points nowhere, seeded directly —
    /// the same shortcut [`crate::tasks::service`]'s own `seed_repository`
    /// takes, and safe for exactly the tests that take it: the listing and
    /// pruning reads below are pure SQLite and never invoke git, so
    /// `repo::register` would drag a real checkout into a test that never
    /// looks at one.
    ///
    /// [`get_run`] is *not* one of those tests. It calls
    /// [`worktree::diff_summary`], which runs git against this path, so those
    /// tests use [`seed_repository_at`] with a real [`TempRepo`] instead —
    /// `CLAUDE.md`'s "never fake git" rule, which a path git rightly refuses
    /// is the other side of.
    async fn seed_repository(ctx: &ServiceContext, name: &str) -> String {
        seed_repository_at(ctx, name, &format!("/tmp/{name}")).await
    }

    async fn seed_repository_at(ctx: &ServiceContext, name: &str, path: &str) -> String {
        let id = new_id();
        sqlx::query(
            "INSERT INTO repositories (id, name, path, default_branch, worktree_root,
                allow_unattended_runs, created_at)
             VALUES (?1, ?2, ?3, 'main', '/tmp/rimaia-worktrees', 0, ?4)",
        )
        .bind(&id)
        .bind(name)
        .bind(path)
        .bind(ctx.clock.now())
        .execute(&ctx.pool)
        .await
        .expect("seed a repository");
        id
    }

    async fn seed_task(ctx: &ServiceContext, repository_id: &str, title: &str) -> String {
        seed_task_on_branch(ctx, repository_id, title, None).await
    }

    async fn seed_task_on_branch(
        ctx: &ServiceContext,
        repository_id: &str,
        title: &str,
        branch: Option<&str>,
    ) -> String {
        let id = new_id();
        sqlx::query(
            "INSERT INTO tasks
                (id, repository_id, title, board_column, position, run_state, branch,
                 created_at, updated_at)
             VALUES (?1, ?2, ?3, 'ready', 1.0, 'idle', ?4, ?5, ?5)",
        )
        .bind(&id)
        .bind(repository_id)
        .bind(title)
        .bind(branch)
        .bind(ctx.clock.now())
        .execute(&ctx.pool)
        .await
        .expect("seed a task");
        id
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_run(
        ctx: &ServiceContext,
        task_id: &str,
        attempt: i64,
        status: RunStatus,
        exit_class: Option<ExitClass>,
        started_at: DateTime<Utc>,
        ended: bool,
        log_path: &str,
    ) -> String {
        let id = new_id();
        let ended_at = ended.then_some(started_at);
        sqlx::query(
            "INSERT INTO runs
                (id, task_id, attempt, status, session_id, prompt, started_at, ended_at,
                 exit_class, log_path)
             VALUES (?1, ?2, ?3, ?4, ?5, 'do the thing', ?6, ?7, ?8, ?9)",
        )
        .bind(&id)
        .bind(task_id)
        .bind(attempt)
        .bind(status)
        .bind(&id)
        .bind(started_at)
        .bind(ended_at)
        .bind(exit_class)
        .bind(log_path)
        .execute(&ctx.pool)
        .await
        .expect("seed a run");
        id
    }

    #[tokio::test]
    async fn list_runs_for_task_orders_newest_attempt_first() {
        let h = TestContext::new().await;
        let repository_id = seed_repository(&h.context, "repo").await;
        let task_id = seed_task(&h.context, &repository_id, "a task").await;
        let now = h.context.clock.now();

        seed_run(
            &h.context,
            &task_id,
            1,
            RunStatus::Failed,
            Some(ExitClass::Fatal),
            now,
            true,
            "/tmp/missing-1.jsonl",
        )
        .await;
        let second = seed_run(
            &h.context,
            &task_id,
            2,
            RunStatus::Succeeded,
            Some(ExitClass::Success),
            now,
            true,
            "/tmp/missing-2.jsonl",
        )
        .await;

        let runs = list_runs_for_task(&h.context, &task_id)
            .await
            .expect("list runs");

        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, second, "attempt 2 is newest and comes first");
        assert_eq!(runs[0].attempt, 2);
        assert_eq!(runs[1].attempt, 1);
    }

    #[tokio::test]
    async fn list_runs_filters_by_repository_status_and_date_range() {
        let h = TestContext::new().await;
        let first_repo = seed_repository(&h.context, "first").await;
        let second_repo = seed_repository(&h.context, "second").await;
        let first_task = seed_task(&h.context, &first_repo, "in the first repo").await;
        let second_task = seed_task(&h.context, &second_repo, "in the second repo").await;

        let early: DateTime<Utc> = "2026-08-01T00:00:00Z".parse().expect("literal timestamp");
        let late: DateTime<Utc> = "2026-08-20T00:00:00Z".parse().expect("literal timestamp");

        seed_run(
            &h.context,
            &first_task,
            1,
            RunStatus::Succeeded,
            Some(ExitClass::Success),
            early,
            true,
            "/tmp/a.jsonl",
        )
        .await;
        let matching = seed_run(
            &h.context,
            &first_task,
            2,
            RunStatus::Succeeded,
            Some(ExitClass::Success),
            late,
            true,
            "/tmp/b.jsonl",
        )
        .await;
        seed_run(
            &h.context,
            &second_task,
            1,
            RunStatus::Succeeded,
            Some(ExitClass::Success),
            late,
            true,
            "/tmp/c.jsonl",
        )
        .await;
        seed_run(
            &h.context,
            &first_task,
            3,
            RunStatus::Failed,
            Some(ExitClass::Fatal),
            late,
            true,
            "/tmp/d.jsonl",
        )
        .await;

        let entries = list_runs(
            &h.context,
            RunFilter {
                repository_id: Some(first_repo.clone()),
                status: Some(RunStatus::Succeeded),
                since: Some("2026-08-10T00:00:00Z".parse().expect("literal timestamp")),
                until: None,
            },
        )
        .await
        .expect("list runs");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].run.id, matching);
        assert_eq!(entries[0].repository_id, first_repo);
        assert_eq!(entries[0].task_title, "in the first repo");
        assert!(
            !entries[0].log_available,
            "the seeded log path was never written to disk"
        );
    }

    #[tokio::test]
    async fn get_run_reports_the_log_as_unavailable_when_the_file_is_gone() {
        let h = TestContext::new().await;
        // A real repository, because `get_run` runs git against its path —
        // this is the acceptance criterion "a run whose log file was deleted
        // shows a clear 'log unavailable' state rather than erroring", and it
        // only means anything if everything *except* the log is intact.
        let source = TempRepo::init();
        let repository_id = seed_repository_at(
            &h.context,
            "repo",
            source.path().to_str().expect("temp path is UTF-8"),
        )
        .await;
        let task_id = seed_task(&h.context, &repository_id, "a task").await;
        let run_id = seed_run(
            &h.context,
            &task_id,
            1,
            RunStatus::Succeeded,
            Some(ExitClass::Success),
            h.context.clock.now(),
            true,
            "/tmp/definitely-does-not-exist-rimaia.jsonl",
        )
        .await;

        let detail = get_run(&h.context, &run_id).await.expect("get_run");

        assert!(!detail.log_available);
        assert_eq!(detail.run.id, run_id);
    }

    #[tokio::test]
    async fn get_run_carries_the_branchs_real_diff_and_commits_beside_the_log() {
        // ADR-0013's ordering in one read: the diff and the commits come
        // first, and they are git's, not a stored snapshot. Asserted against a
        // real repository with one real commit on the task's branch — a mocked
        // git would only prove the mock works (`CLAUDE.md`).
        let h = TestContext::new().await;
        let source = TempRepo::init().branch("rimaia/a-task").commit(
            "src/parser.rs",
            "// parser\n",
            "Add the parser",
        );
        let repository_id = seed_repository_at(
            &h.context,
            "repo",
            source.path().to_str().expect("temp path is UTF-8"),
        )
        .await;
        let task_id =
            seed_task_on_branch(&h.context, &repository_id, "a task", Some("rimaia/a-task")).await;

        let dir = tempfile::tempdir().expect("temp dir for a transcript");
        let log_path = dir.path().join("run-1.jsonl");
        std::fs::write(&log_path, "{}\n").expect("write a transcript");

        let run_id = seed_run(
            &h.context,
            &task_id,
            1,
            RunStatus::Succeeded,
            Some(ExitClass::Success),
            h.context.clock.now(),
            true,
            log_path.to_str().expect("temp path is UTF-8"),
        )
        .await;

        let detail = get_run(&h.context, &run_id).await.expect("get_run");

        assert!(detail.log_available);
        assert_eq!(detail.diff.diff.files_changed, 1);
        assert_eq!(detail.diff.diff.insertions, 1);
        assert_eq!(
            detail
                .diff
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec!["src/parser.rs"]
        );
        assert_eq!(detail.diff.commits.len(), 1);
        assert_eq!(detail.diff.commits[0].subject, "Add the parser");
    }

    #[tokio::test]
    async fn total_log_size_sums_every_file_under_every_tasks_directory() {
        let dir = tempfile::tempdir().expect("temp runs dir");
        let paths = AppPaths::new(dir.path());
        // Under `runs_dir()`, not under the data dir itself: a transcript lives
        // at `<data>/runs/<task-id>/<run-id>.jsonl` (ADR-0013), and seeding it
        // one level up would assert nothing about the directory the production
        // walk actually opens.
        let task_dir = paths.runs_dir().join("task-1");
        std::fs::create_dir_all(&task_dir).expect("create a task directory");
        std::fs::write(task_dir.join("run-1.jsonl"), "12345").expect("write a transcript");
        std::fs::write(task_dir.join("run-1.stderr.log"), "12").expect("write a stderr log");

        let size = total_log_size(&paths).await;

        assert_eq!(size, 7);
    }

    #[tokio::test]
    async fn total_log_size_of_a_directory_that_does_not_exist_yet_is_zero() {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::new(dir.path().join("never-created"));

        assert_eq!(total_log_size(&paths).await, 0);
    }

    #[tokio::test]
    async fn pruning_by_age_removes_only_closed_runs_started_before_the_cutoff() {
        let h = TestContext::new().await;
        let repository_id = seed_repository(&h.context, "repo").await;
        let task_id = seed_task(&h.context, &repository_id, "a task").await;

        let dir = tempfile::tempdir().expect("temp dir");
        let old_log = dir.path().join("old.jsonl");
        let new_log = dir.path().join("new.jsonl");
        std::fs::write(&old_log, "old transcript").expect("write old log");
        std::fs::write(&new_log, "new transcript").expect("write new log");

        // `TestContext::new` starts the clock at `test_epoch`,
        // 2026-08-20T02:00:00Z — ten days back is 2026-08-10T02:00:00Z.
        let old_started: DateTime<Utc> = "2026-08-01T00:00:00Z".parse().expect("literal timestamp");
        let new_started: DateTime<Utc> = "2026-08-20T00:00:00Z".parse().expect("literal timestamp");

        seed_run(
            &h.context,
            &task_id,
            1,
            RunStatus::Succeeded,
            Some(ExitClass::Success),
            old_started,
            true,
            old_log.to_str().expect("temp path is UTF-8"),
        )
        .await;
        seed_run(
            &h.context,
            &task_id,
            2,
            RunStatus::Succeeded,
            Some(ExitClass::Success),
            new_started,
            true,
            new_log.to_str().expect("temp path is UTF-8"),
        )
        .await;

        let result = prune_logs(&h.context, PruneCriterion::OlderThanDays(10))
            .await
            .expect("prune");

        assert_eq!(result.runs_pruned, 1);
        assert_eq!(result.bytes_freed, "old transcript".len() as u64);
        assert!(!old_log.exists());
        assert!(new_log.exists(), "a run started after the cutoff survives");
    }

    #[tokio::test]
    async fn pruning_by_age_never_touches_a_run_still_in_flight() {
        let h = TestContext::new().await;
        let repository_id = seed_repository(&h.context, "repo").await;
        let task_id = seed_task(&h.context, &repository_id, "a task").await;

        let dir = tempfile::tempdir().expect("temp dir");
        let in_flight_log = dir.path().join("in-flight.jsonl");
        std::fs::write(&in_flight_log, "still going").expect("write log");

        let very_old: DateTime<Utc> = "2020-01-01T00:00:00Z".parse().expect("literal timestamp");
        seed_run(
            &h.context,
            &task_id,
            1,
            RunStatus::Running,
            None,
            very_old,
            false,
            in_flight_log.to_str().expect("temp path is UTF-8"),
        )
        .await;

        // A cutoff just one day back from the test clock's `test_epoch` is
        // still decades after `very_old` — if the `ended_at IS NOT NULL`
        // guard were missing, this would prune it.
        let result = prune_logs(&h.context, PruneCriterion::OlderThanDays(1))
            .await
            .expect("prune");

        assert_eq!(result.runs_pruned, 0);
        assert!(in_flight_log.exists());
    }

    #[tokio::test]
    async fn pruning_by_task_removes_its_finished_logs_and_spares_a_run_in_flight() {
        let h = TestContext::new().await;
        let repository_id = seed_repository(&h.context, "repo").await;
        let task_id = seed_task(&h.context, &repository_id, "a task").await;
        let other_task_id = seed_task(&h.context, &repository_id, "another task").await;

        let dir = tempfile::tempdir().expect("temp dir");
        let this_task_log = dir.path().join("this-task.jsonl");
        let other_task_log = dir.path().join("other-task.jsonl");
        std::fs::write(&this_task_log, "mine").expect("write log");
        std::fs::write(&other_task_log, "not mine").expect("write log");

        let now = h.context.clock.now();
        seed_run(
            &h.context,
            &task_id,
            1,
            RunStatus::Succeeded,
            Some(ExitClass::Success),
            now,
            true,
            this_task_log.to_str().expect("temp path is UTF-8"),
        )
        .await;
        seed_run(
            &h.context,
            &other_task_id,
            1,
            RunStatus::Succeeded,
            Some(ExitClass::Success),
            now,
            true,
            other_task_log.to_str().expect("temp path is UTF-8"),
        )
        .await;

        // A second attempt on the *same* task, still running — `ended_at` NULL.
        // This is the case the panel's button can reach mid-run, so it is the
        // one worth pinning: the transcript being written to right now is the
        // only record of work that has not finished.
        let in_flight_log = dir.path().join("in-flight.jsonl");
        std::fs::write(&in_flight_log, "still going").expect("write log");
        seed_run(
            &h.context,
            &task_id,
            2,
            RunStatus::Running,
            None,
            now,
            false,
            in_flight_log.to_str().expect("temp path is UTF-8"),
        )
        .await;

        let result = prune_logs(&h.context, PruneCriterion::Task(task_id))
            .await
            .expect("prune");

        assert_eq!(result.runs_pruned, 1);
        assert!(!this_task_log.exists());
        assert!(
            in_flight_log.exists(),
            "a run still in flight keeps the transcript it is writing to",
        );
        assert!(other_task_log.exists(), "another task's log is untouched");
    }
}
