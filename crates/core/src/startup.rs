//! Work done once, at process start, before the window opens.
//!
//! Named for *when* it runs rather than for what it currently checks: today
//! that is [`survey`] alone, but this is also the natural later home for task
//! 006's first-launch settings seed — another thing that only makes sense once,
//! before anything else touches the pool.
//!
//! # What `survey` is, and deliberately is not
//!
//! Three writers share this database (ADR-0003), and a desktop app does not get
//! a clean shutdown for free: the window can be force-quit, the process killed,
//! the machine put to sleep mid-`fsync`. [`survey`] answers, on the next launch,
//! "what did that leave behind" — a task whose `run_state` never left
//! `running`, a task whose `worktree_path` no longer resolves, a run whose
//! `log_path` no longer resolves (ADR-0011's "runs left running by a crash are
//! marked interrupted and offered for resume", ADR-0013's "reconciled at
//! startup like worktrees: a runs row pointing at a missing file is marked, not
//! trusted").
//!
//! It does not act on any of that. **This module reads and reports; it does not
//! write.** That is not a shortcut this stage of the project is taking — it is
//! the correct shape permanently. What to *do* about a task stuck `running` is
//! a run-state transition, and task 004 ships the one function allowed to make
//! one, `set_run_state`; a second writer of `run_state` — even a well-meaning
//! one in a startup hook — is exactly the bug ADR-0006 names: the same
//! invariant enforced in two places eventually enforces two different
//! invariants. Recreating or clearing a vanished worktree is task 007's
//! business, because it also has to decide whether the branch survived.
//! Marking or backfilling a run whose transcript vanished is task 008's,
//! because it owns what a `runs` row means once a process is gone. `survey`
//! hands each of those tasks a list of ids to act on and takes no position on
//! what the right action is.
//!
//! # Why `tokio::fs::try_exists`, and why an error is not a "yes it's missing"
//!
//! The scheduler shares this runtime, so a filesystem check here has to be
//! `tokio::fs::try_exists` rather than `std::path::Path::exists()` — the
//! survey itself is small, but reaching for the blocking call is a habit worth
//! not starting on the runtime a background scheduler is about to depend on.
//!
//! `try_exists` also draws the distinction this module needs and
//! `Path::exists()` cannot: it returns `Ok(false)` only for a clean "not
//! found", and `Err` for everything else — permission denied, an unmounted
//! network volume, a path this process simply cannot stat right now. Only the
//! clean `Ok(false)` counts as missing here. Treating a stat *failure* as
//! "missing" would report a worktree that is still there, on a volume that
//! merely has not mounted yet, and send the user chasing a repair for nothing.

use serde::Serialize;
use sqlx::SqlitePool;

use crate::db::RunState;
use crate::error::Result;

/// What a crash may have left behind, as of one call to [`survey`].
///
/// Every field is a list of ids, not rows: whoever acts on a finding wants the
/// whole row and already has a service of its own to fetch and interpret it
/// with, so handing back a partial `Task` or `Run` here would just be a second,
/// staler copy.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationReport {
    /// Ids of tasks whose `run_state` was still `running` when this process
    /// started — the state a task is left in only by a crash, since nothing in
    /// the MVP transitions a task *out* of `running` except a run finishing.
    pub tasks_left_running: Vec<String>,
    /// Ids of tasks whose `worktree_path` is set but no longer resolves to
    /// anything on disk.
    pub missing_worktrees: Vec<String>,
    /// Ids of runs whose `log_path` no longer resolves to a transcript file.
    pub missing_run_logs: Vec<String>,
}

impl ReconciliationReport {
    /// True when there is nothing to report: the previous exit was clean.
    pub fn is_empty(&self) -> bool {
        self.tasks_left_running.is_empty()
            && self.missing_worktrees.is_empty()
            && self.missing_run_logs.is_empty()
    }
}

/// Surveys the database and the filesystem for state a previous run left
/// behind, and logs a summary. Reports and repairs nothing — see the module
/// docs for why that split is deliberate rather than provisional.
pub async fn survey(pool: &SqlitePool) -> Result<ReconciliationReport> {
    let report = ReconciliationReport {
        tasks_left_running: tasks_left_running(pool).await?,
        missing_worktrees: missing_worktrees(pool).await?,
        missing_run_logs: missing_run_logs(pool).await?,
    };

    // The one useful thing a stub can do on its own: put what it found where
    // the user reads it the next morning, even before anything acts on it.
    if !report.is_empty() {
        tracing::warn!(
            tasks_left_running = report.tasks_left_running.len(),
            missing_worktrees = report.missing_worktrees.len(),
            missing_run_logs = report.missing_run_logs.len(),
            "startup reconciliation found state a previous run left behind",
        );
    }

    Ok(report)
}

async fn tasks_left_running(pool: &SqlitePool) -> Result<Vec<String>> {
    let ids = sqlx::query_scalar!(
        "SELECT id FROM tasks WHERE run_state = ?",
        RunState::Running
    )
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

/// One row of the `worktree_path IS NOT NULL` query below. Local to this
/// module: nothing outside it needs a task's id paired with only its worktree
/// path.
struct WorktreeCandidate {
    id: String,
    // The `IS NOT NULL` in the query is invisible to sqlx's static analysis,
    // which reads nullability off the column, not the WHERE clause — hence the
    // `!` override rather than an `Option<String>` this code would have to
    // unwrap every time regardless.
    worktree_path: String,
}

async fn missing_worktrees(pool: &SqlitePool) -> Result<Vec<String>> {
    let candidates = sqlx::query_as!(
        WorktreeCandidate,
        r#"SELECT id, worktree_path AS "worktree_path!" FROM tasks WHERE worktree_path IS NOT NULL"#
    )
    .fetch_all(pool)
    .await?;

    let mut missing = Vec::new();
    for candidate in candidates {
        if path_is_missing(&candidate.worktree_path).await {
            missing.push(candidate.id);
        }
    }
    Ok(missing)
}

/// One row of the `runs` scan below: an id paired with the transcript path
/// recorded on it. `log_path` is `NOT NULL` in the schema, so no override is
/// needed the way [`WorktreeCandidate`] needs one.
struct RunCandidate {
    id: String,
    log_path: String,
}

async fn missing_run_logs(pool: &SqlitePool) -> Result<Vec<String>> {
    let candidates = sqlx::query_as!(RunCandidate, "SELECT id, log_path FROM runs")
        .fetch_all(pool)
        .await?;

    let mut missing = Vec::new();
    for candidate in candidates {
        if path_is_missing(&candidate.log_path).await {
            missing.push(candidate.id);
        }
    }
    Ok(missing)
}

/// True only for a clean "not found" — see the module docs for why a stat
/// failure is deliberately not treated the same way.
async fn path_is_missing(path: &str) -> bool {
    matches!(tokio::fs::try_exists(path).await, Ok(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rimaia_core::testing::test_pool;

    #[tokio::test]
    async fn a_task_left_running_is_reported() {
        let pool = test_pool().await;
        let repository_id = insert_repository(&pool).await;
        let task_id = insert_task(&pool, &repository_id, RunState::Running, None).await;

        let report = survey(&pool).await.expect("survey succeeds");

        assert_eq!(report.tasks_left_running, vec![task_id]);
        assert!(report.missing_worktrees.is_empty());
        assert!(report.missing_run_logs.is_empty());
    }

    #[tokio::test]
    async fn a_deleted_worktree_directory_is_reported() {
        let pool = test_pool().await;
        let repository_id = insert_repository(&pool).await;

        let worktrees = tempfile::tempdir().expect("temp dir for a worktree");
        let worktree_path = worktrees.path().join("task-1");
        std::fs::create_dir(&worktree_path).expect("create the worktree directory");
        let worktree_path = worktree_path
            .to_str()
            .expect("temp path is UTF-8")
            .to_string();

        let task_id =
            insert_task(&pool, &repository_id, RunState::Idle, Some(&worktree_path)).await;
        std::fs::remove_dir(&worktree_path).expect("delete the worktree, simulating a crash");

        let report = survey(&pool).await.expect("survey succeeds");

        assert_eq!(report.missing_worktrees, vec![task_id]);
        assert!(report.tasks_left_running.is_empty());
        assert!(report.missing_run_logs.is_empty());
    }

    #[tokio::test]
    async fn a_worktree_that_still_exists_is_not_reported() {
        let pool = test_pool().await;
        let repository_id = insert_repository(&pool).await;

        let worktrees = tempfile::tempdir().expect("temp dir for a worktree");
        let worktree_path = worktrees.path().join("task-1");
        std::fs::create_dir(&worktree_path).expect("create the worktree directory");

        insert_task(
            &pool,
            &repository_id,
            RunState::Idle,
            Some(worktree_path.to_str().expect("temp path is UTF-8")),
        )
        .await;

        let report = survey(&pool).await.expect("survey succeeds");

        assert!(report.missing_worktrees.is_empty());
    }

    #[tokio::test]
    async fn a_run_with_a_missing_transcript_is_reported() {
        let pool = test_pool().await;
        let repository_id = insert_repository(&pool).await;
        let task_id = insert_task(&pool, &repository_id, RunState::Idle, None).await;

        let runs = tempfile::tempdir().expect("temp dir for a transcript");
        // Never written: the run's row exists, its transcript never made it to
        // disk, or was lost after the fact — the survey cannot tell which, and
        // does not need to.
        let log_path = runs.path().join("run-1.jsonl");
        let run_id = insert_run(
            &pool,
            &task_id,
            log_path.to_str().expect("temp path is UTF-8"),
        )
        .await;

        let report = survey(&pool).await.expect("survey succeeds");

        assert_eq!(report.missing_run_logs, vec![run_id]);
        assert!(report.tasks_left_running.is_empty());
        assert!(report.missing_worktrees.is_empty());
    }

    #[tokio::test]
    async fn a_clean_database_surveys_to_an_empty_report() {
        let pool = test_pool().await;

        let report = survey(&pool).await.expect("survey succeeds");

        assert!(report.is_empty());
    }

    #[tokio::test]
    async fn the_survey_changes_nothing_it_reports() {
        // The test that keeps the stub a stub: finding a task stuck `running`
        // must not itself move it. `set_run_state` (task 004) is the only
        // writer of `run_state` — see the module docs.
        let pool = test_pool().await;
        let repository_id = insert_repository(&pool).await;
        let task_id = insert_task(&pool, &repository_id, RunState::Running, None).await;

        survey(&pool).await.expect("survey succeeds");

        let run_state: RunState = sqlx::query_scalar!(
            r#"SELECT run_state AS "run_state: RunState" FROM tasks WHERE id = ?"#,
            task_id
        )
        .fetch_one(&pool)
        .await
        .expect("read the task back");

        assert_eq!(
            run_state,
            RunState::Running,
            "a read-only survey must not transition run_state itself"
        );
    }

    async fn insert_repository(pool: &SqlitePool) -> String {
        let id = crate::db::new_id();
        sqlx::query!(
            "INSERT INTO repositories
                (id, name, path, default_branch, worktree_root, allow_unattended_runs, created_at)
             VALUES (?, 'rimaia', '/tmp/rimaia', 'main', '/tmp/rimaia/worktrees', 0, '2026-08-20T12:00:00Z')",
            id,
        )
        .execute(pool)
        .await
        .expect("insert a repository fixture");
        id
    }

    async fn insert_task(
        pool: &SqlitePool,
        repository_id: &str,
        run_state: RunState,
        worktree_path: Option<&str>,
    ) -> String {
        let id = crate::db::new_id();
        sqlx::query!(
            "INSERT INTO tasks
                (id, repository_id, title, board_column, position, run_state, worktree_path,
                 created_at, updated_at)
             VALUES (?, ?, 'a task', 'ready', 1.0, ?, ?, '2026-08-20T12:00:00Z', '2026-08-20T12:00:00Z')",
            id,
            repository_id,
            run_state,
            worktree_path,
        )
        .execute(pool)
        .await
        .expect("insert a task fixture");
        id
    }

    async fn insert_run(pool: &SqlitePool, task_id: &str, log_path: &str) -> String {
        let id = crate::db::new_id();
        sqlx::query!(
            "INSERT INTO runs
                (id, task_id, attempt, status, session_id, prompt, started_at, log_path)
             VALUES (?, ?, 1, 'running', ?, 'do the thing', '2026-08-20T12:00:00Z', ?)",
            id,
            task_id,
            id,
            log_path,
        )
        .execute(pool)
        .await
        .expect("insert a run fixture");
        id
    }
}
