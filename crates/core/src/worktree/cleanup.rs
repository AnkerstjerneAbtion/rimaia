//! What is on disk, and the guarded ways to take it off again (task 016,
//! ADR-0005, seam-contract D19).
//!
//! [`super::remove`] has existed since task 007 and does the mechanics: the
//! directory, git's administrative record, and optionally the branch. This
//! module is the layer above it — the one that decides *whether* a removal
//! should happen at all, and refuses in words when it should not.
//!
//! # The design rule
//!
//! Task 016's Notes: **deletion is the one irreversible thing this app does.
//! Every guard here earns its place — if in doubt, refuse and explain.** That
//! is why every refusal below names what blocked it and what would unblock it,
//! why the two overridable guards take an explicit confirmation rather than a
//! `bool` that defaults to the dangerous value, and why the one guard that
//! protects a live process has no override at all.
//!
//! # Three kinds of authority, deliberately not one
//!
//! A caller answers three separate questions, because they protect three
//! different things and a single "force" flag would collapse them:
//!
//! - **Uncommitted changes** — work that exists in no commit anywhere.
//! - **Unpushed commits** — work that exists in exactly one place, this disk.
//! - **The branch** — the thing that survives a worktree removal and holds
//!   everything the run committed. ADR-0005: "the branch is left alone unless
//!   the user asks for it to go", so [`BranchDisposition::Keep`] is the
//!   default and deleting an *unmerged* branch needs its own answer, distinct
//!   from the one that authorised removing the worktree.
//!
//! And one question nobody may answer: a task in `running` or `waiting_retry`
//! keeps its worktree, forced or not. A live process is writing in there, and
//! there is no confirmation dialog that makes pulling the directory out from
//! under it a good idea.
//!
//! # Nothing here deletes a `runs` row
//!
//! ADR-0022 part 2, verbatim: pruning "does not delete `runs` rows, and neither
//! does task 016's worktree cleanup." Reclaiming disk must not cost the record
//! of what was spent. The one exception is not this module's — deleting a
//! *task* cascades to its runs, because that is a person saying "this never
//! happened".

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{git, safety, ForceRemoval};
use crate::context::ServiceContext;
use crate::db::{settings, BoardColumn, RunState};
use crate::error::{Error, Result};

/// The `settings` key holding task 016's auto-removal policy.
///
/// Owned here rather than in [`crate::db::settings`], in the shape D3 fixed and
/// D16.2 repeated: storage goes through `settings::get`/`set`, but what the key
/// means and what an absent one means live with the module that acts on it.
pub const AUTO_CLEANUP: &str = "worktree_auto_cleanup";

/// Whether a task reaching `done` takes its worktree with it.
///
/// **Off by default, and there is no seeded row** — an absent key *is*
/// [`Off`](AutoCleanup::Off), which is what makes "off by default" true of a
/// database nobody has configured rather than of a migration (D4 forbids one
/// anyway).
///
/// The `on` spelling carries the acknowledgement in its own name. Task 016
/// requires that enabling this means acknowledging what it deletes; a stored
/// `"true"` would let a future writer flip the policy without ever having had
/// the sentence in front of it, and the enum is how the acknowledgement
/// survives into the row instead of evaporating in the dialog that collected
/// it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AutoCleanup {
    #[default]
    Off,
    /// The user was shown what automatic removal deletes and turned it on.
    OnDoneAcknowledged,
}

impl AutoCleanup {
    /// The stored spelling, which is also the wire spelling — one string, so a
    /// settings row stays legible in the sqlite3 CLI (ADR-0003).
    pub const fn as_str(self) -> &'static str {
        match self {
            AutoCleanup::Off => "off",
            AutoCleanup::OnDoneAcknowledged => "on_done_acknowledged",
        }
    }

    /// Tolerant, like `RunEnvironment::from_stored` and `mcp::configured_port`
    /// before it: `settings` has no `CHECK` on `value` and the user is a
    /// supported writer of that file. A typo hand-edited into this row costs
    /// the *safer* value and a log line — which here means "do not delete
    /// anything", the only direction an unreadable policy may fail in.
    fn from_stored(value: &str) -> Self {
        match value {
            "off" => AutoCleanup::Off,
            "on_done_acknowledged" => AutoCleanup::OnDoneAcknowledged,
            other => {
                tracing::warn!(
                    value = other,
                    "unrecognised worktree_auto_cleanup; falling back to off",
                );
                AutoCleanup::Off
            }
        }
    }
}

/// What happens to the branch when its worktree goes.
///
/// Three variants rather than a `delete_branch: bool`, because task 016 asks
/// for "keeping or deleting its branch — separate, explicit choices" and
/// because the middle one is the honest default for a bulk action: delete it
/// *if git agrees it is merged*, and say so otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchDisposition {
    /// ADR-0005's rule and this module's default: the branch outlives the
    /// worktree, holding everything the run committed.
    #[default]
    Keep,
    /// Delete it, but only when [`git::is_merged`] says the default branch
    /// already has every commit. An unmerged branch is refused, not silently
    /// kept — a caller that asked for deletion is owed the reason it did not
    /// happen.
    DeleteIfMerged,
    /// **The separate confirmation.** Task 016: "never delete a branch that is
    /// not merged, without a separate confirmation". This variant *is* that
    /// confirmation, and it is distinct from the two force flags below so that
    /// authorising the loss of a worktree can never be mistaken for
    /// authorising the loss of its history.
    DeleteEvenIfUnmerged,
}

/// The three answers a removal needs.
///
/// [`Default`] is the safe posture in all three axes — refuse dirty, refuse
/// unpushed, keep the branch — which is what lets automatic removal and the
/// bulk actions be spelled `RemovalAuthorization::default()` rather than by
/// naming three values and hoping the next reader checks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default, deny_unknown_fields)]
pub struct RemovalAuthorization {
    /// Discards work committed nowhere at all.
    pub uncommitted_changes: ForceRemoval,
    /// Discards commits that exist on no remote.
    pub unpushed_commits: ForceRemoval,
    pub branch: BranchDisposition,
}

/// One worktree as Settings → Storage lists it.
///
/// Every field is computed fresh from git and the filesystem on each read, for
/// the reason [`super::WorktreeStatus::exists`] gives at its own: a fact that
/// changes between two reads has no business being cached in a row nobody
/// re-validates. That makes this read expensive — a directory walk and five
/// git invocations per worktree — and it is affordable because there are
/// dozens of these, not thousands, and because the alternative is an inventory
/// that confidently offers to delete something that is no longer there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeInventoryEntry {
    pub task_id: String,
    pub task_title: String,
    pub repository_id: String,
    pub repository_name: String,
    pub column: BoardColumn,
    pub run_state: RunState,
    /// What the row records, whether or not it still resolves — shown so the
    /// user can go and look, including when it is gone.
    pub path: String,
    /// The directory is on disk **and** git still lists it as a worktree on
    /// this branch. A directory git has forgotten is a directory, not a
    /// worktree.
    pub exists: bool,
    pub branch: Option<String>,
    pub base_ref: String,
    pub size_bytes: u64,
    /// The newest mtime under the worktree, or `None` when there is nothing to
    /// read one off. Mtime rather than `tasks.updated_at`: the question the
    /// user is answering is "have I finished with this checkout", and a card
    /// edited this morning says nothing about a directory last written to in
    /// July.
    pub last_activity: Option<DateTime<Utc>>,
    /// Whether the default branch already contains every commit on this one.
    pub merged: bool,
    pub uncommitted_changes: i64,
    pub unpushed_commits: i64,
    /// A run is working in this directory right now, so **no** removal will
    /// touch it — surfaced rather than left for the UI to re-derive from
    /// `run_state`, because the rule is this module's and a second copy of it
    /// in TypeScript is a second copy free to disagree.
    pub live: bool,
}

/// The inventory, plus the total task 016 asks to be shown alongside task 015's
/// run-log usage.
///
/// `total_bytes` is summed here rather than by the caller so that "what
/// Settings displays" and "what the entries add up to" cannot drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeInventory {
    pub entries: Vec<WorktreeInventoryEntry>,
    pub total_bytes: u64,
}

/// One worktree that went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovedWorktree {
    pub task_id: String,
    pub path: String,
    pub bytes_freed: u64,
    /// The branch that went with it, when one was asked for and was allowed.
    pub branch_deleted: Option<String>,
}

/// One worktree a bulk action declined to touch, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefusedWorktree {
    pub task_id: String,
    pub task_title: String,
    /// The refusal, as the single-worktree call would have raised it — the
    /// same sentence, so a user who meets a guard in bulk and then again
    /// individually is told the same thing twice rather than two things once.
    pub reason: String,
}

/// What a bulk cleanup did and what it would not do.
///
/// A report rather than a `Result`, because a bulk action that aborted on its
/// first guard would leave the user unable to reclaim nine safe worktrees
/// because the tenth is dirty — and would not even tell them which one. Every
/// refusal is carried back with its reason; the individual action is where a
/// refusal is an error, because there the user asked about exactly one thing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReport {
    pub removed: Vec<RemovedWorktree>,
    pub refused: Vec<RefusedWorktree>,
    pub bytes_freed: u64,
}

// ---------------------------------------------------------------------------
// The policy setting
// ---------------------------------------------------------------------------

/// Whether a task reaching `done` takes its worktree with it. Absent means
/// [`AutoCleanup::Off`].
pub async fn auto_cleanup(pool: &sqlx::SqlitePool) -> Result<AutoCleanup> {
    Ok(settings::get(pool, AUTO_CLEANUP)
        .await?
        .as_deref()
        .map(AutoCleanup::from_stored)
        .unwrap_or_default())
}

pub async fn set_auto_cleanup(ctx: &ServiceContext, value: AutoCleanup) -> Result<()> {
    settings::set(ctx, AUTO_CLEANUP, value.as_str()).await
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Every worktree Rimaia believes it created, with what it costs and whether it
/// is finished with.
///
/// Driven from `tasks.worktree_path` rather than from `git worktree list`: the
/// listing would also surface worktrees the *user* made in their own
/// repositories, and offering to delete those is exactly the kind of
/// helpfulness this module exists not to have. A path the row records and the
/// disk does not is still listed, with `exists: false` — it is the thing
/// reconciliation is for, and hiding it would hide the problem.
pub async fn inventory(ctx: &ServiceContext) -> Result<WorktreeInventory> {
    let rows = sqlx::query!(
        r#"SELECT t.id AS task_id, t.title AS task_title, t.repository_id,
                  t.branch, t.worktree_path,
                  t.board_column AS "column: BoardColumn",
                  t.run_state AS "run_state: RunState",
                  r.name AS repository_name, r.path AS repository_path,
                  r.default_branch
             FROM tasks t
             JOIN repositories r ON r.id = t.repository_id
            WHERE t.worktree_path IS NOT NULL
            ORDER BY r.name, t.title"#
    )
    .fetch_all(&ctx.pool)
    .await?;

    let mut entries = Vec::with_capacity(rows.len());
    let mut total_bytes = 0;

    for row in rows {
        let Some(path) = row.worktree_path else {
            // Unreachable through the `WHERE`, but the column is nullable and
            // sqlx types it as such; skipping is cheaper than an `expect` that
            // would turn a schema change into a panic in a read.
            continue;
        };

        // A repository moved off the disk must not fail the *whole* inventory —
        // the user opened Settings precisely to clean up, and one unreachable
        // repository would otherwise leave them with an error page instead of
        // the other nine worktrees. Reported as an entry with nothing measured.
        let measured = measure_entry(
            &row.repository_path,
            &row.default_branch,
            &path,
            &row.branch,
        )
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(
                task_id = %row.task_id,
                %error,
                "could not measure a worktree; listing it as unreadable",
            );
            Measured::default()
        });

        total_bytes += measured.size_bytes;
        entries.push(WorktreeInventoryEntry {
            task_id: row.task_id,
            task_title: row.task_title,
            repository_id: row.repository_id,
            repository_name: row.repository_name,
            column: row.column,
            live: is_live(row.run_state),
            run_state: row.run_state,
            path,
            exists: measured.exists,
            branch: row.branch,
            base_ref: row.default_branch,
            size_bytes: measured.size_bytes,
            last_activity: measured.last_activity,
            merged: measured.merged,
            uncommitted_changes: measured.uncommitted_changes,
            unpushed_commits: measured.unpushed_commits,
        });
    }

    Ok(WorktreeInventory {
        entries,
        total_bytes,
    })
}

/// Everything about one worktree that comes from git or the filesystem rather
/// than from a row.
#[derive(Debug, Default)]
struct Measured {
    exists: bool,
    size_bytes: u64,
    last_activity: Option<DateTime<Utc>>,
    merged: bool,
    uncommitted_changes: i64,
    unpushed_commits: i64,
}

async fn measure_entry(
    repository_path: &str,
    base_ref: &str,
    worktree_path: &str,
    branch: &Option<String>,
) -> Result<Measured> {
    let mut measured = Measured::default();

    let path = safety::resolve(Path::new(worktree_path)).await?;
    let (size_bytes, last_activity) = measure_tree(&path).await;
    measured.size_bytes = size_bytes;
    measured.last_activity = last_activity;
    measured.exists = matches!(tokio::fs::try_exists(&path).await, Ok(true));

    let repository = safety::resolve(Path::new(repository_path)).await?;
    if !matches!(tokio::fs::try_exists(&repository).await, Ok(true)) {
        return Ok(measured);
    }

    let Some(branch) = branch.as_deref() else {
        return Ok(measured);
    };
    // A branch the user deleted from the shell is a status, not an error —
    // `super::status` draws the same distinction at the same place.
    if !git::branch_exists(&repository, branch).await?
        || !git::commit_exists(&repository, base_ref).await?
    {
        return Ok(measured);
    }

    measured.merged = git::is_merged(&repository, base_ref, branch).await?;
    measured.unpushed_commits = git::unpushed_commits(&repository, base_ref, branch).await?;
    if measured.exists {
        // Only the working tree can answer this, so it is asked only once the
        // directory has been confirmed to be there.
        measured.uncommitted_changes = git::dirty_file_count(&path).await?;
    }

    Ok(measured)
}

/// Bytes under `path`, and the newest mtime seen — **one** walk, because a full
/// checkout is expensive to traverse and the inventory needs both numbers.
///
/// Iterative rather than recursive: an `async fn` that calls itself needs
/// boxing at every level, and a worklist costs nothing and cannot blow the
/// stack on a deep `node_modules`.
///
/// Symlinks are counted as neither size nor a place to descend. A worktree's
/// links may point anywhere — including back inside it — so following them
/// risks both double-counting a directory the app does not own and looping
/// forever. A link is a few bytes of the number and none of the risk.
///
/// A worktree's `.git` is a *file* pointing into the repository's own
/// administrative directory, not a copy of the object store (that is the whole
/// economy of `git worktree`), so nothing here has to exclude it: the bytes it
/// reports are genuinely the bytes a removal reclaims.
async fn measure_tree(path: &Path) -> (u64, Option<DateTime<Utc>>) {
    let mut total = 0u64;
    let mut newest: Option<DateTime<Utc>> = None;
    let mut pending: Vec<PathBuf> = vec![path.to_path_buf()];

    while let Some(directory) = pending.pop() {
        let Ok(mut children) = tokio::fs::read_dir(&directory).await else {
            // A directory that cannot be read contributes nothing rather than
            // failing the report: this is a size, and a partial one is more
            // useful than none.
            continue;
        };

        while let Ok(Some(child)) = children.next_entry().await {
            let Ok(file_type) = child.file_type().await else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                pending.push(child.path());
                continue;
            }
            let Ok(metadata) = child.metadata().await else {
                continue;
            };
            total += metadata.len();
            if let Ok(modified) = metadata.modified() {
                let modified = DateTime::<Utc>::from(modified);
                if newest.is_none_or(|current| modified > current) {
                    newest = Some(modified);
                }
            }
        }
    }

    (total, newest)
}

// ---------------------------------------------------------------------------
// Removing
// ---------------------------------------------------------------------------

/// Removes one task's worktree, subject to every guard, and its branch when
/// `authorization` says so.
///
/// Returns what was freed, so Settings can report a number that agrees with
/// what just happened rather than one recomputed from a second read.
pub async fn remove_worktree(
    ctx: &ServiceContext,
    task_id: &str,
    authorization: RemovalAuthorization,
) -> Result<RemovedWorktree> {
    let task = crate::tasks::get_task(ctx, task_id).await?.task;
    let repository = crate::repo::get(ctx, &task.repository_id).await?;
    let base_ref = repository.default_branch.clone();

    let Some(recorded) = task.worktree_path.clone() else {
        // Idempotent, the way `super::remove` is: the state the caller wanted
        // is the state it is in.
        return Ok(RemovedWorktree {
            task_id: task.id,
            path: String::new(),
            bytes_freed: 0,
            branch_deleted: None,
        });
    };

    let repository_path = safety::resolve(Path::new(&repository.path)).await?;
    let path = safety::resolve(Path::new(&recorded)).await?;

    ensure_not_live(&task.id, &task.title, task.run_state)?;
    ensure_committed(&path, &task.title, authorization).await?;
    ensure_pushed(
        &repository_path,
        &base_ref,
        task.branch.as_deref(),
        &task.title,
        authorization,
    )
    .await?;
    let delete_branch = resolve_branch_deletion(
        &repository_path,
        &base_ref,
        task.branch.as_deref(),
        authorization,
    )
    .await?;

    // Measured before the removal, for the obvious reason that afterwards
    // there is nothing to measure.
    let (bytes_freed, _) = measure_tree(&path).await;

    let force = authorization.uncommitted_changes;
    super::remove(ctx, &task.id, delete_branch, force).await?;

    Ok(RemovedWorktree {
        task_id: task.id,
        path: recorded,
        bytes_freed,
        branch_deleted: delete_branch.then(|| task.branch.clone()).flatten(),
    })
}

/// Removes the worktree of every task sitting in `done`.
///
/// With [`RemovalAuthorization::default()`] and nothing else: a bulk action is
/// a single click standing in for N individual decisions, and it may not carry
/// more authority than the user would have granted one at a time. Anything a
/// guard stops is reported, not skipped silently.
pub async fn remove_done_worktrees(ctx: &ServiceContext) -> Result<CleanupReport> {
    let inventory = inventory(ctx).await?;
    let candidates = inventory
        .entries
        .iter()
        .filter(|entry| entry.column == BoardColumn::Done);
    sweep(ctx, candidates).await
}

/// Removes the worktree of every task whose branch the default branch already
/// contains.
///
/// "Merged" is [`git::is_merged`]'s conservative answer — see its doc on why a
/// squash-merged branch reads as unmerged, and why that is the error to make.
pub async fn remove_merged_worktrees(ctx: &ServiceContext) -> Result<CleanupReport> {
    let inventory = inventory(ctx).await?;
    let candidates = inventory.entries.iter().filter(|entry| entry.merged);
    sweep(ctx, candidates).await
}

async fn sweep<'a>(
    ctx: &ServiceContext,
    candidates: impl Iterator<Item = &'a WorktreeInventoryEntry>,
) -> Result<CleanupReport> {
    let mut report = CleanupReport::default();

    for entry in candidates {
        match remove_worktree(ctx, &entry.task_id, RemovalAuthorization::default()).await {
            Ok(removed) => {
                report.bytes_freed += removed.bytes_freed;
                report.removed.push(removed);
            }
            Err(error) => report.refused.push(RefusedWorktree {
                task_id: entry.task_id.clone(),
                task_title: entry.task_title.clone(),
                reason: error.to_string(),
            }),
        }
    }

    Ok(report)
}

/// Task 016's optional policy, firing from [`crate::tasks::move_task`].
///
/// **Best effort, and deliberately silent about failure.** The user moved a
/// card; that move succeeded and is committed. A cleanup that a guard refused —
/// or that failed because the repository has been moved — is not a reason to
/// report the *move* as having failed, and returning an error here would do
/// exactly that. It is logged instead.
///
/// It runs with [`RemovalAuthorization::default()`]: every force off, the
/// branch always kept. An automatic action gets strictly less authority than a
/// human clicking a button, because there is nobody to read the refusal it
/// would otherwise be overriding.
pub(crate) async fn auto_remove_on_done(ctx: &ServiceContext, task_id: &str) {
    match auto_cleanup(&ctx.pool).await {
        Ok(AutoCleanup::Off) => return,
        Ok(AutoCleanup::OnDoneAcknowledged) => {}
        Err(error) => {
            tracing::warn!(%error, "could not read the worktree auto-cleanup policy; leaving the worktree alone");
            return;
        }
    }

    match remove_worktree(ctx, task_id, RemovalAuthorization::default()).await {
        Ok(removed) => tracing::info!(
            %task_id,
            bytes_freed = removed.bytes_freed,
            "removed a done task's worktree automatically",
        ),
        Err(error) => tracing::info!(
            %task_id,
            %error,
            "left a done task's worktree in place; automatic cleanup never forces",
        ),
    }
}

// ---------------------------------------------------------------------------
// The guards
// ---------------------------------------------------------------------------

/// The two run states that mean a process is working in that directory, or one
/// is about to be. Shared with the inventory so the UI disables the button the
/// service would refuse.
fn is_live(run_state: RunState) -> bool {
    matches!(run_state, RunState::Running | RunState::WaitingRetry)
}

/// **The guard with no override.**
///
/// Every other refusal here takes a confirmation, because every other refusal
/// is about work the user might reasonably be willing to lose. This one is not
/// about the user's judgement at all: a Claude Code process is writing files in
/// that directory, and removing it mid-run produces a half-deleted checkout, a
/// run that fails on an error nobody can read, and a `git worktree` record
/// pointing at rubble. There is no answer to "are you sure?" that makes that
/// outcome better, so the question is not asked.
///
/// `waiting_retry` is included for the reason `super::correct_run_state`
/// includes it: it means "one is about to", and the gap before the next attempt
/// is not a window in which the directory is spare.
fn ensure_not_live(task_id: &str, title: &str, run_state: RunState) -> Result<()> {
    if !is_live(run_state) {
        return Ok(());
    }
    Err(Error::invalid(format!(
        "\"{title}\" is {state} — its worktree stays until the run finishes. Cancel the run \
         first if you need the directory back; there is no way to force this one, because a \
         process is writing in there right now. (task {task_id})",
        state = match run_state {
            RunState::WaitingRetry => "waiting to retry",
            _ => "running",
        },
    )))
}

/// Refuses a worktree with uncommitted work, **naming how much of it there is**
/// — task 016's acceptance criterion says the count is part of the refusal, and
/// it is the part that lets a user decide: "1 uncommitted change" after a run
/// that wrote a stray log file is a different decision from "47".
async fn ensure_committed(
    path: &Path,
    title: &str,
    authorization: RemovalAuthorization,
) -> Result<()> {
    if authorization.uncommitted_changes.is_forced() {
        return Ok(());
    }
    // Nothing on disk is nothing to lose; git is not asked, and would fail if
    // it were.
    if !matches!(tokio::fs::try_exists(path).await, Ok(true)) {
        return Ok(());
    }

    let changes = git::dirty_file_count(path).await?;
    if changes == 0 {
        return Ok(());
    }
    Err(Error::invalid(format!(
        "\"{title}\" has {changes} uncommitted change{plural} in its worktree, committed nowhere \
         else. Removing it would discard {them} for good — confirm that you want to, or commit \
         the work first.",
        plural = if changes == 1 { "" } else { "s" },
        them = if changes == 1 { "it" } else { "them" },
    )))
}

/// Refuses a worktree whose branch holds commits no remote has.
///
/// The worktree removal itself does not delete those commits — the branch
/// survives it (ADR-0005) — so this guard looks over-cautious until you notice
/// what it is really protecting against: a user who removes the worktree today
/// and deletes the "leftover" branch next week has lost work that never left
/// this machine, and the moment they had the information to decide was this
/// one.
async fn ensure_pushed(
    repository_path: &Path,
    base_ref: &str,
    branch: Option<&str>,
    title: &str,
    authorization: RemovalAuthorization,
) -> Result<()> {
    if authorization.unpushed_commits.is_forced() {
        return Ok(());
    }
    let Some(branch) = branch else {
        return Ok(());
    };
    if !git::branch_exists(repository_path, branch).await?
        || !git::commit_exists(repository_path, base_ref).await?
    {
        return Ok(());
    }

    let unpushed = git::unpushed_commits(repository_path, base_ref, branch).await?;
    if unpushed == 0 {
        return Ok(());
    }
    Err(Error::invalid(format!(
        "\"{title}\" has {unpushed} commit{plural} on {branch} that no remote has. Push the \
         branch, or confirm that you want to remove the worktree anyway — the branch itself is \
         kept either way.",
        plural = if unpushed == 1 { "" } else { "s" },
    )))
}

/// Turns a [`BranchDisposition`] into the `delete_branch` flag
/// [`super::remove`] takes, refusing the one combination that would destroy
/// history nobody confirmed losing.
async fn resolve_branch_deletion(
    repository_path: &Path,
    base_ref: &str,
    branch: Option<&str>,
    authorization: RemovalAuthorization,
) -> Result<bool> {
    let requested = match authorization.branch {
        BranchDisposition::Keep => return Ok(false),
        other => other,
    };
    let Some(branch) = branch else {
        // Nothing to delete, which is not a refusal — `super::remove` treats a
        // missing branch the same way.
        return Ok(false);
    };
    if !git::branch_exists(repository_path, branch).await? {
        return Ok(false);
    }
    if requested == BranchDisposition::DeleteEvenIfUnmerged {
        return Ok(true);
    }

    if !git::commit_exists(repository_path, base_ref).await?
        || !git::is_merged(repository_path, base_ref, branch).await?
    {
        return Err(Error::invalid(format!(
            "{branch} is not merged into {base_ref}, so it was left alone. Removing the worktree \
             is a separate decision from deleting the only copy of its commits — confirm that \
             too if you meant it.",
        )));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn auto_cleanup_is_off_by_default() {
        // Task 016's acceptance criterion, at the type level: the *derived*
        // default is the one that deletes nothing. The database half — an
        // absent key reading as `Off` — is asserted in the integration tests,
        // which have a database.
        assert_eq!(AutoCleanup::default(), AutoCleanup::Off);
    }

    #[test]
    fn an_unreadable_auto_cleanup_policy_falls_back_to_deleting_nothing() {
        assert_eq!(AutoCleanup::from_stored("on"), AutoCleanup::Off);
        assert_eq!(AutoCleanup::from_stored(""), AutoCleanup::Off);
        assert_eq!(
            AutoCleanup::from_stored("on_done_acknowledged"),
            AutoCleanup::OnDoneAcknowledged
        );
    }

    #[test]
    fn the_default_authorization_forces_nothing_and_keeps_the_branch() {
        // What automatic removal and both bulk actions run as. Pinned here so
        // that a future field added to this struct has to be given a safe
        // default deliberately rather than by whatever `Default` derives.
        assert_eq!(
            RemovalAuthorization::default(),
            RemovalAuthorization {
                uncommitted_changes: ForceRemoval::No,
                unpushed_commits: ForceRemoval::No,
                branch: BranchDisposition::Keep,
            }
        );
    }

    #[test]
    fn an_authorization_spells_itself_the_way_the_wire_sends_it() {
        assert_eq!(
            serde_json::to_value(RemovalAuthorization {
                uncommitted_changes: ForceRemoval::ConfirmedByUser,
                unpushed_commits: ForceRemoval::No,
                branch: BranchDisposition::DeleteEvenIfUnmerged,
            })
            .expect("serialize"),
            json!({
                "uncommitted_changes": "confirmed_by_user",
                "unpushed_commits": "no",
                "branch": "delete_even_if_unmerged",
            })
        );
    }

    #[test]
    fn an_omitted_field_authorizes_nothing() {
        // `#[serde(default)]` on the struct, so a caller that sends only the
        // answer it cares about gets the *safe* value for the other two rather
        // than a deserialization error it would be tempted to fix by sending
        // every field.
        assert_eq!(
            serde_json::from_value::<RemovalAuthorization>(json!({})).expect("deserialize"),
            RemovalAuthorization::default()
        );
        assert_eq!(
            serde_json::from_value::<RemovalAuthorization>(json!({ "branch": "delete_if_merged" }))
                .expect("deserialize"),
            RemovalAuthorization {
                branch: BranchDisposition::DeleteIfMerged,
                ..RemovalAuthorization::default()
            }
        );
    }

    #[test]
    fn a_running_task_is_refused_with_no_mention_of_forcing_it() {
        let error = ensure_not_live("t-1", "Add the parser", RunState::Running)
            .expect_err("a running task keeps its worktree");
        let message = error.to_string();

        assert!(message.contains("no way to force this one"), "{message}");
        assert!(message.contains("Add the parser"), "{message}");
    }

    #[test]
    fn a_waiting_retry_task_is_refused_by_the_same_guard() {
        let error = ensure_not_live("t-1", "Add the parser", RunState::WaitingRetry)
            .expect_err("a task waiting to retry keeps its worktree");

        assert!(error.to_string().contains("waiting to retry"));
    }

    #[test]
    fn every_other_run_state_passes_the_guard_with_no_override() {
        for run_state in [
            RunState::Idle,
            RunState::Queued,
            RunState::Blocked,
            RunState::Failed,
            RunState::Cancelled,
        ] {
            assert!(
                ensure_not_live("t-1", "Add the parser", run_state).is_ok(),
                "{run_state:?} is not a state that owns a live process",
            );
        }
    }

    #[test]
    fn an_inventory_entry_serializes_with_camel_case_keys() {
        // Mirrored by hand in `src/types.ts`, where a key spelled `size_bytes`
        // would typecheck on both sides and render `undefined` — the same
        // reason `WorktreeStatus` pins its own.
        let entry = WorktreeInventoryEntry {
            task_id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
            task_title: "Add the parser".to_string(),
            repository_id: "9a1e0000-0000-4000-8000-000000000002".to_string(),
            repository_name: "rimaia".to_string(),
            column: BoardColumn::Done,
            run_state: RunState::Idle,
            path: "/data/worktrees/repo/3f2b1c00".to_string(),
            exists: true,
            branch: Some("rimaia/3f2b1c00-add-the-parser".to_string()),
            base_ref: "main".to_string(),
            size_bytes: 4096,
            last_activity: Some("2026-08-20T12:00:00Z".parse().expect("a literal timestamp")),
            merged: false,
            uncommitted_changes: 2,
            unpushed_commits: 3,
            live: false,
        };

        let wire = serde_json::to_value(&entry).expect("a DTO must always serialize");

        assert_eq!(
            wire["taskId"],
            json!("3f2b1c00-0000-4000-8000-000000000001")
        );
        assert_eq!(wire["taskTitle"], json!("Add the parser"));
        assert_eq!(wire["repositoryName"], json!("rimaia"));
        assert_eq!(wire["column"], json!("done"));
        assert_eq!(wire["runState"], json!("idle"));
        assert_eq!(wire["sizeBytes"], json!(4096));
        assert_eq!(wire["lastActivity"], json!("2026-08-20T12:00:00Z"));
        assert_eq!(wire["uncommittedChanges"], json!(2));
        assert_eq!(wire["unpushedCommits"], json!(3));
        assert_eq!(wire["live"], json!(false));
    }

    #[test]
    fn a_worktree_with_no_activity_reports_a_null_rather_than_an_epoch() {
        // `None` is "nothing to read an mtime off", which is a different fact
        // from "last touched in 1970" — the same distinction `FileDiffStat`
        // draws between a binary file's `None` and a zero.
        let entry = WorktreeInventoryEntry {
            task_id: "t-1".to_string(),
            task_title: "Add the parser".to_string(),
            repository_id: "r-1".to_string(),
            repository_name: "rimaia".to_string(),
            column: BoardColumn::Done,
            run_state: RunState::Idle,
            path: "/gone".to_string(),
            exists: false,
            branch: None,
            base_ref: "main".to_string(),
            size_bytes: 0,
            last_activity: None,
            merged: false,
            uncommitted_changes: 0,
            unpushed_commits: 0,
            live: false,
        };

        assert_eq!(
            serde_json::to_value(&entry).expect("serialize")["lastActivity"],
            json!(null)
        );
    }
}
