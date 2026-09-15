//! Archiving, unarchiving and the on-archive cleanup (task 030, ADR-0025,
//! seam-contract D26).
//!
//! Real repositories in `TempDir`s, real `git worktree add`, real subprocesses.
//! Nothing here is mocked, for the reason `tests/worktree_cleanup.rs` gives at
//! its own head: a mocked git proves the mock works, and the half of this
//! feature most likely to break is the half that touches the filesystem.
//!
//! **Every path contains a space on purpose.** `TempRepo`'s work tree is `work
//! tree`, the worktree root is `my repo`, and the fixture scripts live in a
//! directory called `my scripts` — so an argument vector that ever became a
//! shell string fails here rather than on somebody's `~/My Projects/...`.
//!
//! The timeout test advances the injected clock rather than sleeping, which is
//! CLAUDE.md's rule and the reason `SCRIPT_TIMEOUT` is read through
//! `ctx.clock` in the first place.

use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use rimaia_core::archive::OnArchiveOutcome;
use rimaia_core::db::{BoardColumn, OnArchive, Repository, RunState, Task};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::tasks::{self, ArchiveFilter, NewTask, TaskFilter};
use rimaia_core::testing::{TempRepo, TestContext};
use rimaia_core::{worktree, ServiceContext};

/// The last component of the fixture's `worktree_root`. The space is
/// load-bearing — see the module docs.
const WORKTREE_ROOT_DIR: &str = "my repo";

// ---------------------------------------------------------------------------
// What archiving does to the board
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_archived_task_leaves_the_board_and_the_archive_view_finds_it() {
    let f = Fixture::new().await;
    let kept = f.task("Still working on this").await;
    let gone = f.task("Finished with this").await;

    f.archive(&gone.id).await;

    assert_eq!(f.board_titles().await, vec!["Still working on this"]);
    assert_eq!(f.archived_titles().await, vec!["Finished with this"]);
    // The default filter is what every other caller gets, so the card being
    // absent from the board is the same fact as it being absent from the queue.
    assert_eq!(f.all_titles().await.len(), 2);
    assert_eq!(kept.id, f.board_ids().await[0]);
}

#[tokio::test]
async fn unarchiving_returns_the_card_to_the_column_it_was_in() {
    let f = Fixture::new().await;
    let task = f.task_in("Review this", BoardColumn::InReview).await;

    f.archive(&task.id).await;
    tasks::unarchive_task(f.ctx(), &task.id)
        .await
        .expect("unarchive");

    let back = f.reload(&task.id).await;
    assert_eq!(back.column, BoardColumn::InReview);
    assert!(back.archived_at.is_none());
    assert_eq!(f.archived_titles().await, Vec::<String>::new());
}

#[tokio::test]
async fn archiving_a_task_twice_is_idempotent_rather_than_an_error() {
    // A bulk archive over a selection that overlaps what is already archived
    // must not report N refusals for it.
    let f = Fixture::new().await;
    let task = f.task("Finished with this").await;

    let first = f.archive(&task.id).await;
    let second = f.archive(&task.id).await;

    assert_eq!(first.archived_at, second.archived_at);
    assert_eq!(second.cleanup, OnArchiveOutcome::Nothing);
}

// ---------------------------------------------------------------------------
// The guard, and the queue
// ---------------------------------------------------------------------------

#[tokio::test]
async fn archiving_a_running_task_is_refused() {
    let f = Fixture::new().await;
    let task = f.task("Mid-flight").await;
    f.set_run_state(&task.id, RunState::Queued).await;
    f.set_run_state(&task.id, RunState::Running).await;

    let error = tasks::archive_task(f.ctx(), &task.id)
        .await
        .expect_err("a running task must not be archived");

    assert!(
        error.to_string().contains("Cancel the run first"),
        "{error}"
    );
    assert!(f.reload(&task.id).await.archived_at.is_none());
}

#[tokio::test]
async fn archiving_a_waiting_retry_task_is_refused() {
    // `waiting_retry` means "a process is about to be", which is
    // seam-contract D20 point 1's reasoning reached one level up.
    let f = Fixture::new().await;
    let task = f.task("Coming back at 06:12").await;
    f.set_run_state(&task.id, RunState::Queued).await;
    f.set_run_state(&task.id, RunState::Running).await;
    f.set_run_state(&task.id, RunState::WaitingRetry).await;

    let error = tasks::archive_task(f.ctx(), &task.id)
        .await
        .expect_err("a task waiting to retry must not be archived");

    assert!(error.to_string().contains("waiting to retry"), "{error}");
}

#[tokio::test]
async fn archiving_a_queued_task_removes_it_from_the_next_selection() {
    // Asserted against the scheduler rather than against `list_tasks`, because
    // the claim is about the queue and the queue is what would have run it
    // tonight (seam-contract D26.1).
    let f = Fixture::new().await;
    repo::set_allow_unattended_runs(f.ctx(), &f.repository.id, true)
        .await
        .expect("opt the fixture repository in");
    let stays = f.task("Run this one").await;
    let goes = f.task("Not any more").await;

    let before = rimaia_core::scheduler::selection::plan(f.ctx())
        .await
        .expect("plan");
    assert_eq!(before.len(), 2);

    f.archive(&goes.id).await;

    let after = rimaia_core::scheduler::selection::plan(f.ctx())
        .await
        .expect("plan");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].task_id, stays.id);
}

// ---------------------------------------------------------------------------
// What archiving keeps
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_archived_task_keeps_its_runs_its_links_and_its_dependency_edges() {
    // The whole point of archiving existing at all: `delete_task` cascades to
    // every one of these, and ADR-0022 spends its argument on the first.
    let f = Fixture::new().await;
    let blocker = f.task("Land the parser").await;
    let dependent = f.task("Use the parser").await;
    tasks::set_task_dependencies(f.ctx(), &dependent.id, std::slice::from_ref(&blocker.id))
        .await
        .expect("set the edge");
    tasks::add_task_link(
        f.ctx(),
        &blocker.id,
        rimaia_core::tasks::NewTaskLink {
            label: "Asana".to_string(),
            url: "https://app.asana.com/0/0/1".to_string(),
        },
    )
    .await
    .expect("add a link");
    let run_id = f.record_a_run(&blocker.id).await;

    f.archive(&blocker.id).await;

    let detail = tasks::get_task(f.ctx(), &blocker.id)
        .await
        .expect("an archived task is still readable");
    assert_eq!(detail.links.len(), 1);
    assert!(detail.task.archived_at.is_some());
    assert!(
        rimaia_core::runs::get_run(f.ctx(), &run_id).await.is_ok(),
        "archiving must not touch the runs table",
    );

    // ADR-0008: a dependency is satisfied by a successful run, not by a human
    // tidying the board. The dependent stays blocked, and still names it.
    let summaries = f.board().await;
    let still_blocked = summaries
        .iter()
        .find(|summary| summary.task.id == dependent.id)
        .expect("the dependent is still on the board");
    assert!(still_blocked.blocked_by_incomplete);
    assert_eq!(
        still_blocked.blocking_title.as_deref(),
        Some("Land the parser"),
        "a blocked card must still name an archived blocker, or it is stuck with no way to see why",
    );
}

// ---------------------------------------------------------------------------
// Bulk
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_bulk_archive_reports_refusals_instead_of_aborting() {
    let f = Fixture::new().await;
    let first = f.task("One").await;
    let running = f.task("Two, mid-flight").await;
    let third = f.task("Three").await;
    f.set_run_state(&running.id, RunState::Queued).await;
    f.set_run_state(&running.id, RunState::Running).await;

    let report = tasks::archive_tasks(
        f.ctx(),
        &[first.id.clone(), running.id.clone(), third.id.clone()],
    )
    .await
    .expect("a bulk archive reports rather than erroring");

    assert_eq!(report.archived.len(), 2);
    assert_eq!(report.refused.len(), 1);
    assert_eq!(report.refused[0].task_id, running.id);
    assert_eq!(report.refused[0].title, "Two, mid-flight");
    assert!(
        report.refused[0].reason.contains("Cancel the run first"),
        "the bulk refusal must carry the same sentence the single call would have raised: {}",
        report.refused[0].reason,
    );
    assert_eq!(f.board_titles().await, vec!["Two, mid-flight"]);
}

#[tokio::test]
async fn a_bulk_archive_of_nothing_is_refused_rather_than_reporting_an_empty_success() {
    let f = Fixture::new().await;

    let error = tasks::archive_tasks(f.ctx(), &[])
        .await
        .expect_err("an empty selection is a caller mistake");

    assert!(error.to_string().contains("at least one task"), "{error}");
}

// ---------------------------------------------------------------------------
// The cleanup: the `remove_worktree` preset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_remove_worktree_preset_reclaims_the_checkout() {
    let f = Fixture::new().await;
    f.set_on_archive(OnArchive::RemoveWorktree, None).await;
    let task = f.task("Finished with this").await;
    let prepared = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    assert!(Path::new(&prepared.path).exists());

    let archived = f.archive(&task.id).await;

    match archived.cleanup {
        OnArchiveOutcome::WorktreeRemoved { bytes_freed } => assert!(bytes_freed > 0),
        other => panic!("expected the worktree to be removed, got {other:?}"),
    }
    assert!(!Path::new(&prepared.path).exists());
    assert!(f.reload(&task.id).await.worktree_path.is_none());
}

#[tokio::test]
async fn the_remove_worktree_preset_refuses_a_dirty_worktree_and_archives_anyway() {
    // Both halves of ADR-0025 point 6 in one test: the guard holds, *and* the
    // refusal cannot undo an archive that has already committed.
    let f = Fixture::new().await;
    f.set_on_archive(OnArchive::RemoveWorktree, None).await;
    let task = f.task("Has a stray file in it").await;
    let prepared = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    tokio::fs::write(Path::new(&prepared.path).join("scratch.txt"), "notes")
        .await
        .expect("dirty the worktree");

    let archived = f.archive(&task.id).await;

    match &archived.cleanup {
        OnArchiveOutcome::Failed { reason } => assert!(
            reason.contains("uncommitted change"),
            "the refusal must name what it is protecting: {reason}",
        ),
        other => panic!("expected the dirty-worktree guard to refuse, got {other:?}"),
    }
    assert!(
        Path::new(&prepared.path).exists(),
        "a refused cleanup leaves the directory alone",
    );
    assert!(
        f.reload(&task.id).await.archived_at.is_some(),
        "the archive committed before the cleanup ran and may not be undone by it",
    );
}

#[tokio::test]
async fn a_task_that_never_ran_has_nothing_to_clean_up() {
    let f = Fixture::new().await;
    f.set_on_archive(OnArchive::RemoveWorktree, None).await;
    let task = f.task("Never started").await;

    let archived = f.archive(&task.id).await;

    assert_eq!(archived.cleanup, OnArchiveOutcome::Nothing);
}

// ---------------------------------------------------------------------------
// The cleanup: a script
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn a_script_runs_with_the_documented_environment_and_the_repository_as_its_cwd() {
    let f = Fixture::new().await;
    let receipt = f.scripts.path().join("receipt.txt");
    let script = f
        .script(
            "cleanup.sh",
            &format!(
                "#!/bin/sh\n\
                 {{\n\
                   echo \"task=$RIMAIA_TASK_ID\"\n\
                   echo \"title=$RIMAIA_TASK_TITLE\"\n\
                   echo \"repo=$RIMAIA_REPOSITORY_PATH\"\n\
                   echo \"branch=$RIMAIA_BRANCH\"\n\
                   echo \"worktree=$RIMAIA_WORKTREE_PATH\"\n\
                   echo \"cwd=$(pwd)\"\n\
                   echo \"claude=${{CLAUDE_CODE_SESSION_ID:-stripped}}\"\n\
                 }} > '{}'\n",
                receipt.display(),
            ),
        )
        .await;
    f.set_on_archive(OnArchive::Script, Some(script)).await;
    let task = f.task("Tear down the container").await;
    let prepared = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");

    // Set on *this* process, so the test proves the strip rather than the
    // absence of a variable nobody set.
    std::env::set_var("CLAUDE_CODE_SESSION_ID", "must-not-be-inherited");
    let archived = f.archive(&task.id).await;
    std::env::remove_var("CLAUDE_CODE_SESSION_ID");

    assert!(
        matches!(
            archived.cleanup,
            OnArchiveOutcome::ScriptRan {
                exit_code: Some(0),
                ..
            }
        ),
        "{:?}",
        archived.cleanup,
    );

    let written = tokio::fs::read_to_string(&receipt)
        .await
        .expect("the script wrote its receipt");
    assert!(written.contains(&format!("task={}", task.id)), "{written}");
    assert!(
        written.contains("title=Tear down the container"),
        "{written}"
    );
    assert!(
        written.contains(&format!("branch={}", prepared.branch)),
        "{written}"
    );
    assert!(
        written.contains(&format!("worktree={}", prepared.path)),
        "{written}"
    );
    assert!(
        written.contains("claude=stripped"),
        "CLAUDE_* is process identity, not user config, and must not reach the script: {written}",
    );
    // The repository, never the worktree — the script may be deleting the
    // worktree, and a process whose cwd has gone is in a state nothing good
    // comes of. Compared by canonical path, since macOS resolves `/var` to
    // `/private/var`.
    let cwd = written
        .lines()
        .find_map(|line| line.strip_prefix("cwd="))
        .expect("the script reported its cwd");
    assert_eq!(
        canonical(Path::new(cwd)),
        canonical(Path::new(&f.repository.path)),
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_script_that_fails_is_reported_and_the_task_stays_archived() {
    let f = Fixture::new().await;
    let script = f
        .script(
            "angry.sh",
            "#!/bin/sh\necho 'the volume is still mounted' >&2\nexit 3\n",
        )
        .await;
    f.set_on_archive(OnArchive::Script, Some(script)).await;
    let task = f.task("Tear down the container").await;

    let archived = f.archive(&task.id).await;

    match &archived.cleanup {
        OnArchiveOutcome::ScriptRan { exit_code, output } => {
            assert_eq!(*exit_code, Some(3));
            assert!(output.contains("the volume is still mounted"), "{output}");
        }
        other => panic!("expected the script to have run and failed, got {other:?}"),
    }
    assert!(archived.cleanup.needs_attention());
    assert!(
        f.reload(&task.id).await.archived_at.is_some(),
        "a script's exit code may not put a committed archive in doubt",
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_script_that_never_exits_is_killed_at_the_timeout() {
    // The injected clock, not `sleep`: CLAUDE.md forbids one in a test, and a
    // two-minute timeout asserted by waiting two minutes is a test nobody runs.
    let f = Fixture::new().await;
    let script = f
        .script("forever.sh", "#!/bin/sh\nwhile true; do sleep 1; done\n")
        .await;
    f.set_on_archive(OnArchive::Script, Some(script)).await;
    let task = f.task("Hangs forever").await;

    let clock = f.harness.clock.clone();
    let ticker = tokio::spawn(async move {
        // Both deadlines, with a yield between: the timeout, and then the grace
        // period before `KILL`. Advanced repeatedly because the archive has
        // work to do before it starts waiting, and a single early jump would
        // resolve nothing.
        for _ in 0..40 {
            tokio::task::yield_now().await;
            clock.advance(chrono::Duration::minutes(5));
        }
    });

    let archived = tasks::archive_task(f.ctx(), &task.id)
        .await
        .expect("the archive itself succeeds regardless of the script");
    ticker.abort();

    match &archived.cleanup {
        // Killed by a signal, so there is no exit code — which is exactly what
        // `None` means on this variant.
        OnArchiveOutcome::ScriptRan { exit_code, .. } => assert_eq!(*exit_code, None),
        other => panic!("expected the script to have been stopped, got {other:?}"),
    }
    assert!(archived.cleanup.needs_attention());
}

// ---------------------------------------------------------------------------
// Validating the script path, when it is written
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_script_mode_without_a_path_is_refused() {
    let f = Fixture::new().await;

    let error = rimaia_core::archive::set_repository_on_archive(
        f.ctx(),
        &f.repository.id,
        OnArchive::Script,
        None,
    )
    .await
    .expect_err("`script` with nothing to run is not one of the three states");

    assert!(error.to_string().contains("needs the path"), "{error}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_script_path_that_is_not_executable_is_refused_at_write_time() {
    let f = Fixture::new().await;
    let path = f.scripts.path().join("not executable.sh");
    tokio::fs::write(&path, "#!/bin/sh\nexit 0\n")
        .await
        .expect("write");

    let error = rimaia_core::archive::set_repository_on_archive(
        f.ctx(),
        &f.repository.id,
        OnArchive::Script,
        Some(path.to_string_lossy().into_owned()),
    )
    .await
    .expect_err("a file with no executable bit must be refused");

    assert!(error.to_string().contains("chmod +x"), "{error}");
    assert_eq!(f.reload_repository().await.on_archive, OnArchive::None);
}

#[cfg(unix)]
#[tokio::test]
async fn switching_away_from_script_clears_the_stored_path() {
    // So a later switch back cannot silently re-arm a command nobody has
    // looked at again.
    let f = Fixture::new().await;
    let script = f.script("cleanup.sh", "#!/bin/sh\nexit 0\n").await;
    f.set_on_archive(OnArchive::Script, Some(script)).await;
    assert!(f.reload_repository().await.on_archive_script.is_some());

    f.set_on_archive(OnArchive::RemoveWorktree, None).await;

    let repository = f.reload_repository().await;
    assert_eq!(repository.on_archive, OnArchive::RemoveWorktree);
    assert_eq!(repository.on_archive_script, None);
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

struct Fixture {
    harness: TestContext,
    /// Held only for its `Drop`.
    _source: TempRepo,
    _worktrees: tempfile::TempDir,
    _data: tempfile::TempDir,
    scripts: tempfile::TempDir,
    paths: rimaia_core::AppPaths,
    repository: Repository,
}

impl Fixture {
    async fn new() -> Self {
        let source = TempRepo::init();
        let worktrees = tempfile::Builder::new()
            .prefix("rimaia-worktrees-")
            .tempdir()
            .expect("temp dir for the worktree root");
        // A space, for the reason the module docs give.
        let scripts = tempfile::Builder::new()
            .prefix("my scripts ")
            .tempdir()
            .expect("temp dir for the fixture scripts");
        let data = tempfile::Builder::new()
            .prefix("rimaia-data-")
            .tempdir()
            .expect("temp dir for the data directory");
        let paths = rimaia_core::AppPaths::new(data.path());
        let root = worktrees.path().join(WORKTREE_ROOT_DIR);
        let harness = TestContext::new().await;
        let repository = repo::register(
            &harness.context,
            worktrees.path(),
            NewRepository {
                path: source
                    .path()
                    .to_str()
                    .expect("test paths are UTF-8")
                    .to_string(),
                name: None,
                worktree_root: Some(root.to_str().expect("test paths are UTF-8").to_string()),
            },
        )
        .await
        .expect("register the fixture repository");

        Self {
            harness,
            _source: source,
            _worktrees: worktrees,
            _data: data,
            scripts,
            paths,
            repository,
        }
    }

    fn ctx(&self) -> &ServiceContext {
        &self.harness.context
    }

    async fn task(&self, title: &str) -> Task {
        self.task_in(title, BoardColumn::Ready).await
    }

    async fn task_in(&self, title: &str, column: BoardColumn) -> Task {
        tasks::create_task(
            self.ctx(),
            NewTask {
                repository_id: self.repository.id.clone(),
                title: title.to_string(),
                plan: Some("## Steps\n1. do the thing\n".to_string()),
                extra_instructions: None,
                column: Some(column),
                links: Vec::new(),
            },
        )
        .await
        .expect("create the fixture task")
    }

    async fn archive(&self, task_id: &str) -> rimaia_core::tasks::ArchivedTask {
        tasks::archive_task(self.ctx(), task_id)
            .await
            .expect("archive")
    }

    async fn set_run_state(&self, task_id: &str, to: RunState) {
        tasks::set_run_state(self.ctx(), task_id, to)
            .await
            .expect("a legal run-state transition");
    }

    async fn set_on_archive(&self, mode: OnArchive, script: Option<String>) {
        rimaia_core::archive::set_repository_on_archive(
            self.ctx(),
            &self.repository.id,
            mode,
            script,
        )
        .await
        .expect("set the cleanup slot");
    }

    /// Writes an executable fixture script and returns its path.
    #[cfg(unix)]
    async fn script(&self, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;

        let path = self.scripts.path().join(name);
        tokio::fs::write(&path, body).await.expect("write");
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .await
            .expect("chmod");
        path.to_string_lossy().into_owned()
    }

    async fn reload(&self, task_id: &str) -> Task {
        tasks::get_task(self.ctx(), task_id)
            .await
            .expect("read the task back")
            .task
    }

    async fn reload_repository(&self) -> Repository {
        repo::get(self.ctx(), &self.repository.id)
            .await
            .expect("read the repository back")
    }

    async fn board(&self) -> Vec<rimaia_core::tasks::TaskSummary> {
        tasks::list_tasks(self.ctx(), TaskFilter::default())
            .await
            .expect("board read")
    }

    async fn board_titles(&self) -> Vec<String> {
        self.board()
            .await
            .into_iter()
            .map(|summary| summary.task.title)
            .collect()
    }

    async fn board_ids(&self) -> Vec<String> {
        self.board()
            .await
            .into_iter()
            .map(|summary| summary.task.id)
            .collect()
    }

    async fn archived_titles(&self) -> Vec<String> {
        self.titles_for(ArchiveFilter::Archived).await
    }

    async fn all_titles(&self) -> Vec<String> {
        self.titles_for(ArchiveFilter::All).await
    }

    async fn titles_for(&self, archived: ArchiveFilter) -> Vec<String> {
        tasks::list_tasks(
            self.ctx(),
            TaskFilter {
                archived,
                ..TaskFilter::default()
            },
        )
        .await
        .expect("filtered read")
        .into_iter()
        .map(|summary| summary.task.title)
        .collect()
    }

    /// One run row, so a test can assert it survived the archive.
    async fn record_a_run(&self, task_id: &str) -> String {
        tasks::set_run_state(self.ctx(), task_id, RunState::Queued)
            .await
            .expect("queue");
        tasks::set_run_state(self.ctx(), task_id, RunState::Running)
            .await
            .expect("claim");
        let run = rimaia_core::runner::outcome::start_run(
            self.ctx(),
            &self.paths,
            rimaia_core::runner::outcome::NewRun {
                task_id: task_id.to_string(),
                session_id: "session".to_string(),
                prompt: "the composed prompt".to_string(),
                base_ref: None,
            },
        )
        .await
        .expect("open a run row");
        tasks::set_run_state(self.ctx(), task_id, RunState::Idle)
            .await
            .expect("release");
        run.id
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
