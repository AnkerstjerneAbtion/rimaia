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
//! "what did that leave behind" — a task whose worktree record no longer
//! resolves, a run whose transcript no longer resolves (ADR-0013's "reconciled
//! at startup like worktrees: a runs row pointing at a missing file is marked,
//! not trusted").
//!
//! A task a crash left `running` is not a finding of the survey's any more.
//! From task 043 each runner reconciles the leases it held
//! (`scheduler::reconcile`), so a launch never sweeps another runner's tasks
//! (ADR-0031 point 5).
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

use std::collections::HashMap;

use serde::Serialize;

use crate::context::ServiceContext;
use crate::error::{Error, Result};
use crate::machine::MachineContext;
use crate::paths::AppPaths;
use crate::runner::events::transcript_path;

/// What a crash may have left behind, as of one call to [`survey`].
///
/// Every field is a list of ids, not rows: whoever acts on a finding wants the
/// whole row and already has a service of its own to fetch and interpret it
/// with, so handing back a partial `Task` or `Run` here would just be a second,
/// staler copy.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationReport {
    /// Ids of tasks whose worktree record on this machine names a directory
    /// that no longer resolves to anything on disk.
    pub missing_worktrees: Vec<String>,
    /// Ids of runs whose transcript, at the path derived from the run's ids
    /// (ADR-0013), no longer resolves to a file.
    pub missing_run_logs: Vec<String>,
}

impl ReconciliationReport {
    /// True when there is nothing to report: the previous exit was clean.
    pub fn is_empty(&self) -> bool {
        self.missing_worktrees.is_empty() && self.missing_run_logs.is_empty()
    }
}

/// Surveys the database and the filesystem for state a previous run left
/// behind, and logs a summary. Reports and repairs nothing — see the module
/// docs for why that split is deliberate rather than provisional.
///
/// Only the context's teams' rows: the shell builds its context before it
/// surveys, so a board that holds another team reports none of its tasks.
///
/// Worktrees are read from this machine's records and transcripts from the
/// path their ids derive (task 066); no board column holds either any more.
pub async fn survey(
    ctx: &ServiceContext,
    machine: &MachineContext,
    paths: &AppPaths,
) -> Result<ReconciliationReport> {
    let report = ReconciliationReport {
        missing_worktrees: missing_worktrees(ctx, machine).await?,
        missing_run_logs: missing_run_logs(ctx, paths).await?,
    };

    // The one useful thing a stub can do on its own: put what it found where
    // the user reads it the next morning, even before anything acts on it.
    if !report.is_empty() {
        tracing::warn!(
            missing_worktrees = report.missing_worktrees.len(),
            missing_run_logs = report.missing_run_logs.len(),
            "startup reconciliation found state a previous run left behind",
        );
    }

    Ok(report)
}

/// Tasks whose worktree record names a directory that is gone.
///
/// Only records of tasks the context's teams hold: a record whose task the
/// board no longer has is invisible here, as a deleted task's directory always
/// was (task 066's Out of scope), and another team's is not this context's to
/// report.
async fn missing_worktrees(ctx: &ServiceContext, machine: &MachineContext) -> Result<Vec<String>> {
    let records: HashMap<String, String> = machine
        .store
        .list_worktrees()
        .await?
        .into_iter()
        .map(|record| (record.task_id, record.path))
        .collect();
    if records.is_empty() {
        return Ok(Vec::new());
    }

    let scope = ctx.scope.json();
    let task_ids = serde_json::to_string(&records.keys().collect::<Vec<_>>())
        .map_err(|error| Error::internal(format!("could not encode task ids: {error}")))?;
    let held = sqlx::query_scalar!(
        "SELECT id FROM tasks
          WHERE id IN (SELECT value FROM json_each(?1))
            AND team_id IN (SELECT value FROM json_each(?2))
          ORDER BY id",
        task_ids,
        scope,
    )
    .fetch_all(&ctx.pool)
    .await?;

    let mut missing = Vec::new();
    for task_id in held {
        if let Some(path) = records.get(&task_id) {
            if path_is_missing(path).await {
                missing.push(task_id);
            }
        }
    }
    Ok(missing)
}

/// Runs whose transcript is not at the path its ids derive. Every kind, as
/// D29 point 6 gives this reader.
async fn missing_run_logs(ctx: &ServiceContext, paths: &AppPaths) -> Result<Vec<String>> {
    let scope = ctx.scope.json();
    let candidates = sqlx::query!(
        "SELECT r.id, r.task_id FROM runs r JOIN tasks t ON t.id = r.task_id
          WHERE t.team_id IN (SELECT value FROM json_each(?1))",
        scope,
    )
    .fetch_all(&ctx.pool)
    .await?;

    let mut missing = Vec::new();
    for candidate in candidates {
        let transcript = transcript_path(paths, &candidate.task_id, &candidate.id);
        if matches!(tokio::fs::try_exists(&transcript).await, Ok(false)) {
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
    use crate::db::RunState;
    use crate::machine::{Checkout, WorktreeRecord};
    use crate::testing::TestContext;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    /// A board, a machine store and a data directory, as the shell surveys.
    struct Harness {
        test: TestContext,
        data: TempDir,
        paths: AppPaths,
    }

    impl Harness {
        async fn new() -> Self {
            let data = tempfile::tempdir().expect("a temp data directory");
            let paths = AppPaths::new(data.path());
            Self {
                test: TestContext::new().await,
                data,
                paths,
            }
        }

        async fn survey(&self) -> ReconciliationReport {
            survey(&self.test.context, self.test.machine(), &self.paths)
                .await
                .expect("survey succeeds")
        }

        fn pool(&self) -> &sqlx::SqlitePool {
            &self.test.context.pool
        }

        /// A repository on the board and a checkout of it on this machine.
        async fn repository(&self) -> String {
            let id = crate::db::new_id();
            let team_id = &self.test.solo.team_id;
            sqlx::query!(
                "INSERT INTO repositories (id, team_id, name, default_branch, created_at)
                 VALUES (?, ?, 'rimaia', 'main', ?)",
                id,
                team_id,
                NOW,
            )
            .execute(self.pool())
            .await
            .expect("insert a repository fixture");
            self.test
                .machine()
                .store
                .insert_checkout(&Checkout {
                    repository_id: id.clone(),
                    path: self
                        .data
                        .path()
                        .join("clone")
                        .to_string_lossy()
                        .into_owned(),
                    worktree_root: self.paths.worktrees_dir().to_string_lossy().into_owned(),
                    max_concurrency: 1,
                    unattended_consent: false,
                    on_archive: crate::db::OnArchive::None,
                    on_archive_script: None,
                    credential_login: None,
                    credential_label: None,
                    credential_added_at: None,
                    created_at: crate::testing::test_epoch(),
                })
                .await
                .expect("insert a checkout");
            id
        }

        async fn task(&self, repository_id: &str, run_state: RunState) -> String {
            let id = crate::db::new_id();
            let team_id = &self.test.solo.team_id;
            sqlx::query!(
                "INSERT INTO tasks
                    (id, team_id, repository_id, title, board_column, position, run_state,
                     created_at, updated_at)
                 VALUES (?, ?, ?, 'a task', 'ready', 1.0, ?, ?, ?)",
                id,
                team_id,
                repository_id,
                run_state,
                NOW,
                NOW,
            )
            .execute(self.pool())
            .await
            .expect("insert a task fixture");
            id
        }

        async fn record_worktree(&self, task_id: &str, repository_id: &str, path: &str) {
            self.test
                .machine()
                .store
                .record_worktree(&WorktreeRecord {
                    task_id: task_id.to_string(),
                    repository_id: repository_id.to_string(),
                    path: path.to_string(),
                    fenced_at: None,
                })
                .await
                .expect("record a worktree");
        }

        async fn run(&self, task_id: &str) -> String {
            let id = crate::db::new_id();
            sqlx::query!(
                "INSERT INTO runs (id, task_id, attempt, status, session_id, prompt, started_at)
                 VALUES (?, ?, 1, 'running', ?, 'do the thing', ?)",
                id,
                task_id,
                id,
                NOW,
            )
            .execute(self.pool())
            .await
            .expect("insert a run fixture");
            id
        }

        fn write_transcript(&self, task_id: &str, run_id: &str) {
            let transcript = transcript_path(&self.paths, task_id, run_id);
            std::fs::create_dir_all(transcript.parent().expect("a run directory"))
                .expect("create the run directory");
            std::fs::write(&transcript, "{}\n").expect("write a transcript");
        }
    }

    #[tokio::test]
    async fn a_deleted_worktree_directory_is_reported() {
        let h = Harness::new().await;
        let repository_id = h.repository().await;

        let worktrees = tempfile::tempdir().expect("temp dir for a worktree");
        let worktree_path = worktrees.path().join("task-1");
        std::fs::create_dir(&worktree_path).expect("create the worktree directory");
        let worktree_path = worktree_path
            .to_str()
            .expect("temp path is UTF-8")
            .to_string();

        let task_id = h.task(&repository_id, RunState::Idle).await;
        h.record_worktree(&task_id, &repository_id, &worktree_path)
            .await;
        std::fs::remove_dir(&worktree_path).expect("delete the worktree, simulating a crash");

        let report = h.survey().await;

        assert_eq!(report.missing_worktrees, vec![task_id]);
        assert!(report.missing_run_logs.is_empty());
    }

    #[tokio::test]
    async fn a_worktree_that_still_exists_is_not_reported() {
        let h = Harness::new().await;
        let repository_id = h.repository().await;

        let worktrees = tempfile::tempdir().expect("temp dir for a worktree");
        let worktree_path = worktrees.path().join("task-1");
        std::fs::create_dir(&worktree_path).expect("create the worktree directory");

        let task_id = h.task(&repository_id, RunState::Idle).await;
        h.record_worktree(
            &task_id,
            &repository_id,
            worktree_path.to_str().expect("temp path is UTF-8"),
        )
        .await;

        let report = h.survey().await;

        assert!(report.missing_worktrees.is_empty());
    }

    #[tokio::test]
    async fn a_worktree_record_whose_task_the_board_no_longer_has_is_ignored() {
        // `delete_task` removes no worktree record, and a record whose task is
        // gone is invisible here, as its directory always was (task 066).
        let h = Harness::new().await;
        let repository_id = h.repository().await;
        h.record_worktree("a-deleted-task", &repository_id, "/nowhere/at/all")
            .await;

        let report = h.survey().await;

        assert!(report.missing_worktrees.is_empty());
    }

    #[tokio::test]
    async fn a_run_with_a_missing_transcript_is_reported() {
        let h = Harness::new().await;
        let repository_id = h.repository().await;
        let task_id = h.task(&repository_id, RunState::Idle).await;
        // Never written: the run's row exists, its transcript never made it to
        // disk, or was lost after the fact — the survey cannot tell which, and
        // does not need to.
        let run_id = h.run(&task_id).await;

        let report = h.survey().await;

        assert_eq!(report.missing_run_logs, vec![run_id]);
        assert!(report.missing_worktrees.is_empty());
    }

    #[tokio::test]
    async fn a_transcript_at_its_derived_path_is_not_missing() {
        let h = Harness::new().await;
        let repository_id = h.repository().await;
        let task_id = h.task(&repository_id, RunState::Idle).await;
        let run_id = h.run(&task_id).await;
        h.write_transcript(&task_id, &run_id);

        let report = h.survey().await;

        assert!(report.missing_run_logs.is_empty());
    }

    #[tokio::test]
    async fn a_clean_database_surveys_to_an_empty_report() {
        let h = Harness::new().await;

        let report = h.survey().await;

        assert!(report.is_empty());
    }

    #[tokio::test]
    async fn the_survey_changes_nothing_it_reports() {
        // The test that keeps the stub a stub: surveying a board with a task
        // stuck `running` must not itself move it. `set_run_state` (task 004)
        // is the only writer of `run_state`, and the lease reconcile is what
        // settles that task — see the module docs.
        let h = Harness::new().await;
        let repository_id = h.repository().await;
        let task_id = h.task(&repository_id, RunState::Running).await;

        h.survey().await;

        let run_state: RunState = sqlx::query_scalar!(
            r#"SELECT run_state AS "run_state: RunState" FROM tasks WHERE id = ?"#,
            task_id
        )
        .fetch_one(h.pool())
        .await
        .expect("read the task back");

        assert_eq!(
            run_state,
            RunState::Running,
            "a read-only survey must not transition run_state itself"
        );
    }

    /// The one instant every fixture below is stamped with, in the spelling sqlx
    /// writes for a bound `DateTime<Utc>` — a numeric offset, never `Z` (see the
    /// migration's header). Nothing here reads a timestamp back, but a fixture
    /// in a spelling production never produces is a habit, and the habit is what
    /// eventually mixes the two in a column that sorts by them.
    const NOW: &str = "2026-08-20T12:00:00+00:00";
}
