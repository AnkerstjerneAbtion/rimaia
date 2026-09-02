//! Git worktree operations: create, idempotent re-create, remove, reconcile
//! (ADR-0005, task 007).
//!
//! One worktree and branch per task, living under the app data directory and
//! never inside the repository. Git is invoked as a subprocess with an argument
//! vector — never `sh -c`, because repository paths contain spaces.
//!
//! # What this module owns
//!
//! `tasks.branch` and `tasks.worktree_path`. ADR-0005 puts them there — "the
//! database records the worktree path and branch" — and nothing in task 004
//! writes them, so [`write_worktree_columns`] is their single writer, the way
//! `tasks::set_run_state` is `run_state`'s. What this module deliberately does
//! **not** own is `run_state` itself: reconciliation corrects it by calling
//! that function, never by issuing its own `UPDATE`, because a second writer of
//! an invariant is the bug ADR-0006 names. `startup::survey`'s module doc
//! states the same split from the other side, and names task 007 as the thing
//! that acts on its `missing_worktrees` findings.
//!
//! # Repository state on disk is authoritative
//!
//! ADR-0005's last bullet, and the reason for [`reconcile`]: the row records
//! what Rimaia was told, and every operation here re-derives the truth from
//! git rather than trusting the row. A path that no longer resolves is cleared;
//! a branch that survived it is kept, because it still holds whatever the run
//! committed.
//!
//! # The base ref
//!
//! [`base_ref`] owns it — ADR-0008's branch chaining, which task 011 grew out
//! of the one-line seam task 007 left here. Two rules of this module follow
//! from it and are stated once, at [`recorded_base_ref`]: [`prepare`] resolves
//! the base fresh and records it on the run it is about to start, while
//! [`status`] and [`diff_summary`] prefer the *recorded* value.

mod base_ref;
mod git;
mod naming;
mod safety;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::context::ServiceContext;
use crate::db::{Repository, RunState, Task};
use crate::error::{Error, Result};
use crate::events::ChangeEvent;

/// A task's worktree: where it is, what branch it is on, and what that branch
/// was created from.
///
/// `base_ref` is carried rather than stored **on the task** — there is no
/// column for it there and seam-contract D4 forbids a migration to add one — so
/// it is re-derived on every call by [`base_ref::resolve`]. The caller that
/// starts a run puts it on the `runs` row instead, which is a column that
/// already exists: see [`recorded_base_ref`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    pub task_id: String,
    pub repository_id: String,
    pub path: String,
    pub branch: String,
    pub base_ref: String,
    /// ADR-0008's explicit multi-dependency warning, or `None`. Carried on the
    /// worktree as well as on [`WorktreeStatus`] because [`prepare`] is what
    /// the runner calls, and a warning produced during an unattended run has
    /// nowhere else to be logged.
    pub dependency_warning: Option<String>,
}

/// Files changed, insertions and deletions — ADR-0013's "git diff summary".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStat {
    pub files_changed: i64,
    pub insertions: i64,
    pub deletions: i64,
}

/// What the task detail panel shows about a task's worktree.
///
/// Every numeric field is zero, and `dirty` false, whenever the branch or the
/// base ref is missing — a task that has never run, or one whose branch was
/// deleted. That is a real answer rather than an absence: the panel renders
/// "no worktree yet" from [`exists`](WorktreeStatus::exists), and has nothing
/// to do with a `None` it would have to unwrap five times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeStatus {
    pub task_id: String,
    /// The directory is on disk **and** git still lists it as a worktree of
    /// this repository on this branch. A directory git has forgotten is a
    /// directory, not a worktree, and running an agent in it would produce
    /// commits on nothing.
    pub exists: bool,
    /// What the row records, whether or not it still resolves — the panel
    /// shows the path so the user can go and look, including when it is gone.
    pub path: Option<String>,
    pub branch: Option<String>,
    /// What the last attempt was actually built on when there was one, and a
    /// fresh resolution otherwise — see [`recorded_base_ref`].
    pub base_ref: String,
    /// ADR-0008's multi-dependency warning, always computed against the
    /// **current** dependency set rather than the recorded attempt's.
    ///
    /// The split is deliberate and is the reason both fields are here.
    /// `base_ref` answers "what was this branch built on", which is a fact
    /// about an attempt that already happened and must not change when the
    /// board does. This answers "what should you do about it", which is advice
    /// about the graph as it stands — the ADR's own remedy is "merge them or
    /// serialize the work", and neither is an instruction about the past.
    pub dependency_warning: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    /// Uncommitted work in the worktree: modified, staged or untracked alike,
    /// since all three are what a removal would destroy.
    pub dirty: bool,
    /// Commits on the branch that are not on the base — the same number as
    /// [`ahead`](WorktreeStatus::ahead), because that is what "ahead of base"
    /// counts. Both are here because the panel says "3 commits" where the
    /// branch header says "3 ahead, 1 behind", and a caller should not have to
    /// know they are the same arithmetic.
    pub commit_count: i64,
    pub diff: DiffStat,
}

/// One file's insertions and deletions out of a [`DiffSummary`] — task 015's
/// per-file breakdown, alongside the aggregate [`DiffStat`] rather than
/// replacing it: a review opens on the totals and drills into this list only
/// when it wants to.
///
/// `insertions`/`deletions` are `None` for a binary file, which `git diff
/// --numstat` reports as `-` in both columns — a fact distinct from "zero
/// lines changed", which is why this is not simply `0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiffStat {
    pub path: String,
    pub insertions: Option<i64>,
    pub deletions: Option<i64>,
}

/// One commit on a task's branch, as the review view lists it (ADR-0013).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitSummary {
    pub sha: String,
    pub short_sha: String,
    pub subject: String,
    pub author: String,
    pub committed_at: DateTime<Utc>,
}

/// What ADR-0013 puts at the top of a review: the diff and the commits,
/// "because that is what review is actually about".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    pub task_id: String,
    pub branch: Option<String>,
    pub base_ref: String,
    pub diff: DiffStat,
    /// The same diff, broken out per file (task 015) — `diff`'s totals are a
    /// sum over this list, kept alongside it rather than instead of it because
    /// a review opens on the totals.
    pub files: Vec<FileDiffStat>,
    /// Newest first.
    pub commits: Vec<CommitSummary>,
}

/// Whether the user has confirmed discarding uncommitted work.
///
/// An enum and a required parameter rather than something [`remove`] decides:
/// `git worktree remove --force` deletes work that was never committed
/// anywhere, and task 007's Scope allows it "only on explicit user
/// confirmation". A `bool` would make the dangerous value the one that is
/// easier to type; this way the call site has to name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForceRemoval {
    /// `git worktree remove` refuses a dirty worktree, and that refusal
    /// reaches the user as the error message.
    No,
    /// The user was shown what would be discarded and said yes.
    ConfirmedByUser,
}

impl ForceRemoval {
    fn is_forced(self) -> bool {
        matches!(self, Self::ConfirmedByUser)
    }
}

/// What [`reconcile`] repaired on one task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciledWorktree {
    pub task_id: String,
    /// The `worktree_path` that was cleared, as it was recorded — so a log
    /// line or a startup notice can name the directory that went missing.
    pub cleared_path: String,
    /// The branch left on the row because it still exists in the repository,
    /// holding whatever the run committed. `None` when the branch was gone
    /// too and the row was cleared of it as well.
    pub retained_branch: Option<String>,
    /// What [`crate::tasks::set_run_state`] moved the task to, when it was in
    /// a state that assumed the worktree was still there.
    pub corrected_run_state: Option<RunState>,
}

/// The two canonical directories every operation works from.
struct Location {
    /// The repository's own work tree.
    repository: PathBuf,
    /// `repositories.worktree_root` — `<app-data>/worktrees/<repo-slug>` by
    /// default (ADR-0005), overridable per repository by task 003.
    root: PathBuf,
}

/// Creates a task's worktree and branch, or returns the one it already has.
///
/// The order is deliberate and each step earns its place:
///
/// 1. **Idempotence first.** A worktree that is still there and still valid is
///    returned unchanged, before anything fetches or writes. This is what makes
///    a retry resume in place rather than start over (ADR-0005's "Reuse",
///    ADR-0011's "Work already committed is not lost"), and it is why a retry
///    does not need the network.
/// 2. `git fetch --prune`, **best effort** — being offline is a logged warning
///    and never a failure, because an overnight queue on a train still has to
///    run.
/// 3. `git worktree prune`, because git's administrative record outlives a
///    directory somebody deleted by hand.
/// 4. `git worktree add <path> -b <branch> <base-ref>`, with the path
///    `<worktree_root>/<task-id>` and the branch `rimaia/<task-id>-<slug>`.
///
/// Refuses a worktree root inside the repository working tree, and any path
/// outside the configured root — see [`safety`] for why neither check is a
/// string comparison.
pub async fn prepare(ctx: &ServiceContext, task_id: &str) -> Result<Worktree> {
    let task = fetch_task(ctx, task_id).await?;
    let repository = crate::repo::get(ctx, &task.repository_id).await?;
    let location = locate(&repository).await?;
    // Fresh, never the recorded value: this call is what produces the next
    // attempt, so the answer has to describe the dependency graph as it is now.
    let resolved = base_ref::resolve(ctx, &task, &repository).await?;
    let base_ref = resolved.base_ref;

    if let Some(warning) = &resolved.warning {
        // Logged as well as returned, because `prepare`'s caller on the
        // unattended path is the runner, and nobody is reading a return value
        // at 03:00.
        tracing::warn!(task_id = %task.id, %base_ref, "{warning}");
    }

    if let Some(existing) =
        existing_worktree(&location, &task, &base_ref, resolved.warning.clone()).await?
    {
        return Ok(existing);
    }

    let path = location.root.join(&task.id);
    safety::ensure_within_root(&path, &location.root).await?;

    // `existing_worktree` above already ruled out the happy path — a live
    // worktree on this task's own branch. A directory at the target path that
    // is *not* that (a detached checkout left by an interrupted rebase, one
    // git no longer lists at all) makes `git worktree add` fail with a raw
    // "already exists" that names no cause and offers no next step. Refusing
    // it here, before any fetch or prune, turns that into a sentence the user
    // can act on. Deleting the directory ourselves is not this function's
    // call — ADR-0005 keeps cleanup an explicit, separate act.
    if matches!(tokio::fs::try_exists(&path).await, Ok(true)) {
        return Err(Error::invalid(format!(
            "{} already exists but git no longer recognizes it as this task's \
             worktree — move it aside or delete it, then try again",
            path.display(),
        )));
    }

    git::fetch_prune(&location.repository).await;
    git::worktree_prune(&location.repository).await?;

    ensure_base_ref_exists(&location, &repository, &base_ref).await?;
    let (branch, create_branch) = resolve_branch(&location.repository, &task).await?;

    // The components of `root` that did not exist when it was resolved are
    // created here as plain directories, so the resolved form stays canonical
    // — nothing in the tail can have become a symlink behind it.
    tokio::fs::create_dir_all(&location.root).await?;
    git::worktree_add(
        &location.repository,
        &path,
        &branch,
        &base_ref,
        create_branch,
    )
    .await?;

    let path = path_to_string(&path)?;
    write_worktree_columns(ctx, &task.id, Some(&branch), Some(&path)).await?;

    Ok(Worktree {
        task_id: task.id,
        repository_id: repository.id,
        path,
        branch,
        base_ref,
        dependency_warning: resolved.warning,
    })
}

/// Everything the task detail panel needs about a worktree, computed fresh
/// from git.
pub async fn status(ctx: &ServiceContext, task_id: &str) -> Result<WorktreeStatus> {
    let task = fetch_task(ctx, task_id).await?;
    let repository = crate::repo::get(ctx, &task.repository_id).await?;
    let repository_path = locate_repository(&repository).await?;
    let resolved = base_ref::resolve(ctx, &task, &repository).await?;
    let base_ref = recorded_base_ref(ctx, &task.id)
        .await?
        .unwrap_or(resolved.base_ref);

    let mut status = WorktreeStatus {
        task_id: task.id.clone(),
        exists: false,
        path: task.worktree_path.clone(),
        branch: task.branch.clone(),
        base_ref: base_ref.clone(),
        dependency_warning: resolved.warning,
        ahead: 0,
        behind: 0,
        dirty: false,
        commit_count: 0,
        diff: DiffStat::default(),
    };

    let Some(branch) = task.branch.as_deref() else {
        return Ok(status);
    };
    // A branch or a base ref the user deleted from the shell is not an error
    // to report — it is a status, and this is what it looks like.
    if !git::branch_exists(&repository_path, branch).await?
        || !git::commit_exists(&repository_path, &base_ref).await?
    {
        return Ok(status);
    }

    let (ahead, behind) = git::ahead_behind(&repository_path, &base_ref, branch).await?;
    status.ahead = ahead;
    status.behind = behind;
    status.commit_count = ahead;
    status.diff = git::diff_stat(&repository_path, &base_ref, branch).await?;

    // Dirtiness is the one thing that needs the working tree itself, so it is
    // asked only once git has confirmed there is one.
    if let Some((path, _)) = live_worktree(&repository_path, &task).await? {
        status.exists = true;
        status.dirty = git::is_dirty(&path).await?;
    }

    Ok(status)
}

/// The diff and the commits a review opens with (ADR-0013).
///
/// Both are measured against the merge base of the base ref and the branch,
/// not against the base ref's current tip: a base branch that moved on after
/// the worktree was created must not show up as work the agent undid.
pub async fn diff_summary(ctx: &ServiceContext, task_id: &str) -> Result<DiffSummary> {
    let task = fetch_task(ctx, task_id).await?;
    let repository = crate::repo::get(ctx, &task.repository_id).await?;
    let repository_path = locate_repository(&repository).await?;
    let base_ref = match recorded_base_ref(ctx, &task.id).await? {
        Some(recorded) => recorded,
        None => base_ref::resolve(ctx, &task, &repository).await?.base_ref,
    };

    let mut summary = DiffSummary {
        task_id: task.id.clone(),
        branch: task.branch.clone(),
        base_ref: base_ref.clone(),
        diff: DiffStat::default(),
        files: Vec::new(),
        commits: Vec::new(),
    };

    let Some(branch) = task.branch.as_deref() else {
        return Ok(summary);
    };
    if !git::branch_exists(&repository_path, branch).await?
        || !git::commit_exists(&repository_path, &base_ref).await?
    {
        return Ok(summary);
    }

    (summary.diff, summary.files) = git::diff(&repository_path, &base_ref, branch).await?;
    summary.commits = git::commits(&repository_path, &base_ref, branch).await?;
    Ok(summary)
}

/// Removes a task's worktree — the directory **and** git's administrative
/// record of it — and, when asked, its branch.
///
/// Idempotent, the way [`prepare`] is: a task with nothing to remove is not an
/// error, because the state the caller wanted is the state it is in.
///
/// Cleanup is never automatic on failure (ADR-0005); this is the explicit act,
/// and `force` is the user's explicit answer to the one question it cannot
/// decide for them.
pub async fn remove(
    ctx: &ServiceContext,
    task_id: &str,
    delete_branch: bool,
    force: ForceRemoval,
) -> Result<()> {
    let task = fetch_task(ctx, task_id).await?;
    let repository = crate::repo::get(ctx, &task.repository_id).await?;
    let location = locate(&repository).await?;

    if let Some(recorded) = task.worktree_path.as_deref() {
        let path = safety::resolve(Path::new(recorded)).await?;
        safety::ensure_within_root(&path, &location.root).await?;

        if matches!(tokio::fs::try_exists(&path).await, Ok(true)) {
            git::worktree_remove(&location.repository, &path, force.is_forced()).await?;
        }
        // Unconditionally, and not only after a successful `remove`: the
        // administrative directory under `.git/worktrees/` is precisely what a
        // vanished directory leaves behind, and task 007's acceptance
        // criterion is that *both* halves go.
        git::worktree_prune(&location.repository).await?;
    }

    let branch = match (delete_branch, task.branch.as_deref()) {
        // After the worktree is gone, never before: git refuses to delete a
        // branch that is checked out in a worktree, and this is that branch.
        (true, Some(branch)) => {
            if git::branch_exists(&location.repository, branch).await? {
                git::delete_branch(&location.repository, branch).await?;
            }
            None
        }
        (_, current) => current.map(str::to_string),
    };

    write_worktree_columns(ctx, &task.id, branch.as_deref(), None).await
}

/// Repairs the tasks `startup::survey` reported as having a `worktree_path`
/// that no longer resolves.
///
/// Takes the ids rather than re-running the scan, because that is the split
/// `startup::survey`'s own module doc describes: it "hands each of those tasks
/// a list of ids to act on and takes no position on what the right action is".
/// Every id is re-checked against the filesystem before anything is cleared —
/// the list is a snapshot, and a worktree that is still there must not be
/// cleared off its task because it was missing a moment ago.
///
/// No `Result`. This runs before the window opens, and one task whose
/// repository has itself been moved off the disk is not a reason for the app
/// not to start; a failure is logged and the next id is tried. What it returns
/// is what it actually repaired.
pub async fn reconcile(ctx: &ServiceContext, task_ids: &[String]) -> Vec<ReconciledWorktree> {
    let mut reconciled = Vec::new();

    for task_id in task_ids {
        match reconcile_one(ctx, task_id).await {
            Ok(Some(record)) => reconciled.push(record),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                %task_id,
                %error,
                "could not reconcile a missing worktree; leaving the row as it is",
            ),
        }
    }

    if !reconciled.is_empty() {
        tracing::info!(
            count = reconciled.len(),
            "cleared worktree paths that no longer exist on disk",
        );
    }
    reconciled
}

async fn reconcile_one(ctx: &ServiceContext, task_id: &str) -> Result<Option<ReconciledWorktree>> {
    let task = fetch_task(ctx, task_id).await?;
    let Some(recorded) = task.worktree_path.clone() else {
        return Ok(None);
    };

    let path = safety::resolve(Path::new(&recorded)).await?;
    // Only a clean "not found" counts as missing, for the reason
    // `startup::survey` gives at its own `try_exists`: a stat that failed
    // because a network volume has not mounted yet is not a vanished worktree.
    if !matches!(tokio::fs::try_exists(&path).await, Ok(false)) {
        return Ok(None);
    }

    let repository = crate::repo::get(ctx, &task.repository_id).await?;
    let repository_path = safety::resolve(Path::new(&repository.path)).await?;

    // Best effort: the repository may have moved too, and that is not a reason
    // to leave the row claiming a worktree that is not there.
    if let Err(error) = git::worktree_prune(&repository_path).await {
        tracing::warn!(%task_id, %error, "could not prune git's worktree metadata");
    }

    // ADR-0005 leaves branches alone, and a branch that outlived its directory
    // still holds everything the run committed — clearing it off the row would
    // orphan that work from every view that reads the task. It goes only when
    // it is gone from the repository too.
    let retained_branch = match task.branch.as_deref() {
        Some(branch) if git::branch_exists(&repository_path, branch).await? => {
            Some(branch.to_string())
        }
        _ => None,
    };

    write_worktree_columns(ctx, &task.id, retained_branch.as_deref(), None).await?;
    let corrected_run_state = correct_run_state(ctx, &task).await?;

    Ok(Some(ReconciledWorktree {
        task_id: task.id,
        cleared_path: recorded,
        retained_branch,
        corrected_run_state,
    }))
}

/// Moves a task whose worktree vanished out of a state that assumed it was
/// still there — **through [`crate::tasks::set_run_state`]**, never through an
/// `UPDATE` of its own. That function is the only writer of `run_state` in this
/// crate, and `startup::survey`'s module doc names it as such while naming this
/// task as the thing that acts on the findings.
///
/// `running` and `waiting_retry` are the two states that mean "a process is
/// working in that directory, or one is about to". Both land on `failed`,
/// which is where seam-contract D9 already puts a task whose run died with the
/// app, and where ADR-0007's failure rule leaves it: in `ready`, with the
/// failure shown on the card. Every other state is already consistent with
/// having no worktree, and moving it would be inventing a transition nothing
/// asked for.
async fn correct_run_state(ctx: &ServiceContext, task: &Task) -> Result<Option<RunState>> {
    let corrected = match task.run_state {
        RunState::Running | RunState::WaitingRetry => RunState::Failed,
        _ => return Ok(None),
    };

    crate::tasks::set_run_state(ctx, &task.id, corrected).await?;
    Ok(Some(corrected))
}

/// What the task's most recent attempt was actually built on, when there is
/// one — `runs.base_ref`, written by `runner::outcome::start_run`.
///
/// **[`status`] and [`diff_summary`] prefer this over a fresh resolution, and
/// [`prepare`] never reads it.** ADR-0008's amendment of 2026-09-02 takes that
/// decision; the argument is that a task's dependencies can change between
/// attempts, so a re-derivation answers a different question from the one being
/// asked. The morning review is reading a branch that already exists and wants
/// to know what it was measured against; re-resolving would silently re-measure
/// yesterday's diff against a base chosen from today's graph, and the diff would
/// change without anybody committing anything. `prepare` is the opposite case —
/// it is producing the next attempt, so the current graph is exactly the right
/// input.
///
/// `None` for a task that has never run, where there is nothing recorded and the
/// fresh resolution is also the only answer available.
///
/// Highest `attempt`, never latest `ended_at`, for the reason `list_tasks` gives
/// at its own last-run join: `ended_at` is NULL while a run is in flight, and
/// the run in flight is precisely the one whose base a status call is about.
async fn recorded_base_ref(ctx: &ServiceContext, task_id: &str) -> Result<Option<String>> {
    let recorded = sqlx::query_scalar!(
        "SELECT base_ref FROM runs WHERE task_id = ?1 ORDER BY attempt DESC LIMIT 1",
        task_id,
    )
    .fetch_optional(&ctx.pool)
    .await?
    // Two layers of `Option`: no run at all, and a run from before this column
    // was written. Both mean "nothing recorded", and so does a blank string —
    // `base_ref::has_branch` refuses the same value for the same reason.
    .flatten()
    .filter(|base_ref| !base_ref.trim().is_empty());

    Ok(recorded)
}

/// Resolves and safety-checks the two directories every operation works from.
///
/// The order matters. The repository has to exist — registration validated it,
/// but a directory can be moved afterwards — the root is resolved as far as it
/// exists, and only then is containment decided, so a root configured inside
/// the repository is refused *before* anything creates it there.
async fn locate(repository: &Repository) -> Result<Location> {
    let repository_path = locate_repository(repository).await?;

    let root = safety::resolve(Path::new(&repository.worktree_root)).await?;
    safety::ensure_outside_repository(&root, &repository_path).await?;

    Ok(Location {
        repository: repository_path,
        root,
    })
}

/// The repository's own work tree, resolved and confirmed still on disk.
///
/// [`status`] and [`diff_summary`] need only this half of [`locate`] — they
/// have no worktree root to check containment against — but skipping the
/// existence check entirely would let the first git call spawn into a missing
/// `cwd` and surface as an `Error::internal` carrying a raw OS error, where a
/// repository moved or deleted out from under the app is exactly the
/// user-fixable condition [`Error::invalid`] exists for.
async fn locate_repository(repository: &Repository) -> Result<PathBuf> {
    let repository_path = safety::resolve(Path::new(&repository.path)).await?;
    if !matches!(tokio::fs::try_exists(&repository_path).await, Ok(true)) {
        return Err(Error::invalid(format!(
            "{} no longer exists — \"{}\" has been moved or deleted",
            repository.path, repository.name,
        )));
    }
    Ok(repository_path)
}

/// The worktree [`prepare`] returns unchanged, when there is a valid one.
async fn existing_worktree(
    location: &Location,
    task: &Task,
    base_ref: &str,
    dependency_warning: Option<String>,
) -> Result<Option<Worktree>> {
    let Some((path, branch)) = live_worktree(&location.repository, task).await? else {
        return Ok(None);
    };
    // Checked even on the idempotent path: a `worktree_root` edited in
    // Settings after the worktree was created leaves the row pointing outside
    // it, and "return it unchanged" must not become the door that skips the
    // guard every other operation passes through.
    safety::ensure_within_root(&path, &location.root).await?;

    Ok(Some(Worktree {
        task_id: task.id.clone(),
        repository_id: task.repository_id.clone(),
        path: path_to_string(&path)?,
        branch,
        base_ref: base_ref.to_string(),
        dependency_warning,
    }))
}

/// The task's worktree path and branch, when the row records both, the
/// directory is on disk, **and** git still lists it as a worktree on that
/// branch.
///
/// The third condition is the one a `try_exists` would miss. A directory whose
/// administrative data under `.git/worktrees/` has been pruned is an ordinary
/// directory: git commands run inside it fail, and an agent started there
/// would produce nothing recoverable.
async fn live_worktree(repository_path: &Path, task: &Task) -> Result<Option<(PathBuf, String)>> {
    let (Some(recorded), Some(branch)) = (task.worktree_path.as_deref(), task.branch.as_deref())
    else {
        return Ok(None);
    };

    let path = safety::resolve(Path::new(recorded)).await?;
    if !matches!(tokio::fs::try_exists(&path).await, Ok(true)) {
        return Ok(None);
    }

    for entry in git::worktree_list(repository_path).await? {
        if entry.branch.as_deref() == Some(branch)
            && safety::same_directory(&entry.path, &path).await
        {
            return Ok(Some((path, branch.to_string())));
        }
    }
    Ok(None)
}

/// The branch to put the new worktree on, and whether `git worktree add` has
/// to create it.
///
/// Two cases that look alike and are not. A branch **recorded on this task's
/// own row** is reused if it still exists: it was created for this task, it
/// holds this task's commits, and reusing it is what ADR-0005's "Reuse" and
/// ADR-0011's resume semantics require. A freshly computed name that is
/// already taken is a *collision* with something this task did not create, and
/// gets a numeric suffix — task 007's Scope: "never by reuse", because
/// checking out somebody else's branch would silently continue their work.
async fn resolve_branch(repository_path: &Path, task: &Task) -> Result<(String, bool)> {
    if let Some(branch) = task.branch.as_deref() {
        if git::branch_exists(repository_path, branch).await? {
            return Ok((branch.to_string(), false));
        }
    }

    let candidate = naming::branch_name(&task.id, &task.title);
    if !git::branch_exists(repository_path, &candidate).await? {
        return Ok((candidate, true));
    }
    for attempt in 2..=naming::MAX_COLLISION_ATTEMPTS {
        let suffixed = naming::with_collision_suffix(&candidate, attempt);
        if !git::branch_exists(repository_path, &suffixed).await? {
            return Ok((suffixed, true));
        }
    }

    Err(Error::internal(format!(
        "every branch name from {candidate} to {} is already taken",
        naming::with_collision_suffix(&candidate, naming::MAX_COLLISION_ATTEMPTS),
    )))
}

/// Refuses a base ref that is not in the repository, with a sentence naming
/// what to fix — a `default_branch` typed by hand into task 003's edit form is
/// not re-validated there, so this is where a typo surfaces.
async fn ensure_base_ref_exists(
    location: &Location,
    repository: &Repository,
    base_ref: &str,
) -> Result<()> {
    if git::commit_exists(&location.repository, base_ref).await? {
        return Ok(());
    }
    Err(Error::invalid(format!(
        "\"{}\" has no branch named {base_ref} to create a worktree from. \
         Set its default branch in Settings → Repositories.",
        repository.name,
    )))
}

/// The one place `tasks.branch` and `tasks.worktree_path` are written.
///
/// One function rather than four `UPDATE`s, so "a task's branch and its
/// worktree path are set and cleared together" is a property of the signature
/// instead of a convention. See this module's own doc for why these two
/// columns are worktree state rather than task 004's.
async fn write_worktree_columns(
    ctx: &ServiceContext,
    task_id: &str,
    branch: Option<&str>,
    worktree_path: Option<&str>,
) -> Result<()> {
    let now = ctx.clock.now();
    sqlx::query!(
        "UPDATE tasks SET branch = ?1, worktree_path = ?2, updated_at = ?3 WHERE id = ?4",
        branch,
        worktree_path,
        now,
        task_id,
    )
    .execute(&ctx.pool)
    .await?;

    // After the write is committed — this runs in autocommit — never before
    // (ADR-0018).
    ctx.publish(ChangeEvent::tasks([task_id.to_string()]));
    Ok(())
}

/// The task row, read through task 004's own detail service rather than a
/// second `SELECT` of the same seventeen columns. `get_task` already owns the
/// "no task with id X" message, and every operation here runs once per task
/// rather than once per row of a board read, so the extra queries it does are
/// not worth a duplicated one.
async fn fetch_task(ctx: &ServiceContext, id: &str) -> Result<Task> {
    Ok(crate::tasks::get_task(ctx, id).await?.task)
}

fn path_to_string(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| Error::invalid(format!("{} is not valid UTF-8", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    /// The DTOs are mirrored by hand in `src/types.ts`, and a key spelled
    /// `files_changed` there instead of `filesChanged` would typecheck on both
    /// sides and render `undefined` in the panel. `db::models` pins the row
    /// types' keys the same way and for the same reason.
    #[test]
    fn a_worktree_status_serializes_with_camel_case_keys() {
        let status = WorktreeStatus {
            task_id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
            exists: true,
            path: Some("/data/worktrees/repo/3f2b1c00".to_string()),
            branch: Some("rimaia/3f2b1c00-wire-the-board".to_string()),
            base_ref: "rimaia/3f2b1a00-add-the-api-endpoint".to_string(),
            dependency_warning: Some(
                "This task branches from \"Add the API endpoint\" \
                 (rimaia/3f2b1a00-add-the-api-endpoint). \"Seed the fixtures\" is also a \
                 dependency and is not in that base — merge into it what you need, or run \
                 this task again once the rest have landed."
                    .to_string(),
            ),
            ahead: 3,
            behind: 1,
            dirty: true,
            commit_count: 3,
            diff: DiffStat {
                files_changed: 4,
                insertions: 120,
                deletions: 17,
            },
        };

        assert_eq!(
            serde_json::to_value(&status).expect("a DTO must always serialize"),
            json!({
                "taskId": "3f2b1c00-0000-4000-8000-000000000001",
                "exists": true,
                "path": "/data/worktrees/repo/3f2b1c00",
                "branch": "rimaia/3f2b1c00-wire-the-board",
                // ADR-0008's chaining: a base that is another task's branch, not
                // the repository default, and the warning naming what is not in it.
                "baseRef": "rimaia/3f2b1a00-add-the-api-endpoint",
                "dependencyWarning":
                    "This task branches from \"Add the API endpoint\" \
                     (rimaia/3f2b1a00-add-the-api-endpoint). \"Seed the fixtures\" is also a \
                     dependency and is not in that base — merge into it what you need, or run \
                     this task again once the rest have landed.",
                "ahead": 3,
                "behind": 1,
                "dirty": true,
                "commitCount": 3,
                "diff": { "filesChanged": 4, "insertions": 120, "deletions": 17 },
            })
        );
    }

    #[test]
    fn a_diff_summary_serializes_its_commits_as_a_list_of_objects() {
        let summary = DiffSummary {
            task_id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
            branch: Some("rimaia/3f2b1c00-wire-the-board".to_string()),
            base_ref: "main".to_string(),
            diff: DiffStat::default(),
            files: vec![FileDiffStat {
                path: "src/lib.rs".to_string(),
                insertions: Some(12),
                deletions: Some(3),
            }],
            commits: vec![CommitSummary {
                sha: "1111111111111111111111111111111111111111".to_string(),
                short_sha: "1111111".to_string(),
                subject: "Wire the board to the store".to_string(),
                author: "Rimaia Test".to_string(),
                committed_at: "2026-08-20T12:00:00Z".parse().expect("a literal timestamp"),
            }],
        };

        let wire = serde_json::to_value(&summary).expect("a DTO must always serialize");

        assert_eq!(wire["baseRef"], json!("main"));
        assert_eq!(
            wire["files"][0],
            json!({ "path": "src/lib.rs", "insertions": 12, "deletions": 3 })
        );
        assert_eq!(
            wire["commits"][0],
            json!({
                "sha": "1111111111111111111111111111111111111111",
                "shortSha": "1111111",
                "subject": "Wire the board to the store",
                "author": "Rimaia Test",
                "committedAt": "2026-08-20T12:00:00Z",
            })
        );
    }

    #[test]
    fn force_removal_spells_itself_the_way_the_frontend_sends_it() {
        assert_eq!(
            serde_json::to_value(ForceRemoval::ConfirmedByUser).expect("serialize"),
            json!("confirmed_by_user")
        );
        assert_eq!(
            serde_json::from_value::<ForceRemoval>(json!("no")).expect("deserialize"),
            ForceRemoval::No
        );
    }

    #[test]
    fn only_confirmed_by_user_passes_force_to_git() {
        // The whole point of the enum: the dangerous value is the one that has
        // to be named.
        assert!(!ForceRemoval::No.is_forced());
        assert!(ForceRemoval::ConfirmedByUser.is_forced());
    }
}
