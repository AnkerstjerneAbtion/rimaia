//! Worktree status and file-manager commands (task 007, ADR-0005).
//!
//! Every git operation and every worktree invariant lives in
//! `rimaia_core::worktree` — this file only reshapes wire args and calls it.
//! [`reveal_task_worktree`] is the exception worth naming: revealing a
//! directory in the OS file manager has no MCP equivalent (task 010 talks to
//! Rimaia over a protocol, not a desktop it could show a Finder window on),
//! so unlike every other command in this crate it is not a thin wrapper
//! standing in front of a rule the MCP server also has to obey — there is no
//! such rule to share.
//!
//! # The three commands here that deliberately have no MCP tool
//!
//! ADR-0021 makes a Tauri command without a tool a defect, and names one
//! standing exception: `delete_task` "stays absent from both … it is a
//! decision about destructiveness, not about which client is privileged."
//! [`remove_task_worktree`], [`cleanup_done_worktrees`] and
//! [`cleanup_merged_worktrees`] join it, and seam-contract D20 records why.
//! The inventory and the policy setting *do* get tools, operator-only — the
//! read is how an agent finds out what is on disk, and refusing the read while
//! refusing the write would leave it unable even to explain the problem.

use rimaia_core::worktree::{
    self, AutoCleanup, CleanupReport, DiffSummary, RemovalAuthorization, RemovedWorktree,
    WorktreeInventory, WorktreeStatus,
};
use rimaia_core::{tasks, Error, Result};
use serde::Deserialize;
use tauri::State;
use tauri_plugin_opener::OpenerExt;

use crate::state::AppState;

/// Branch, base ref, ahead/behind, dirtiness and the diff stat — everything
/// the task detail panel's worktree section shows, recomputed fresh from git
/// on every call.
#[tauri::command]
pub async fn get_worktree_status(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<WorktreeStatus> {
    worktree::status(&state.context, &task_id).await
}

/// The diff and the commits a run detail view opens with (task 015,
/// ADR-0013): files changed, insertions, deletions, the per-file breakdown,
/// and the commit list — the branch's current state, not a snapshot of any
/// one attempt, since every attempt of a task shares one branch (ADR-0005).
#[tauri::command]
pub async fn get_diff_summary(state: State<'_, AppState>, task_id: String) -> Result<DiffSummary> {
    worktree::diff_summary(&state.context, &task_id).await
}

/// Opens the task's worktree directory in the OS file manager.
///
/// "Copy path" needs no command of its own: [`get_worktree_status`]'s `path`
/// is already on the frontend, and the system clipboard is a browser API
/// away — reaching for a plugin just to copy a string that is already there
/// would be an unlisted dependency (seam-contract D6).
#[tauri::command]
pub async fn reveal_task_worktree(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<()> {
    let detail = tasks::get_task(&state.context, &task_id).await?;
    let path = detail.task.worktree_path.ok_or_else(|| {
        Error::invalid("this task has no worktree yet — start a run to create one")
    })?;

    // The path is passed as a value, not interpolated into a command line —
    // worktree paths are built from a repository path and a task id, and
    // `TempRepo`'s own fixtures put a space in the repository directory on
    // purpose (see CLAUDE.md's house style).
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| Error::internal(format!("could not open the worktree directory: {e}")))
}

// ---------------------------------------------------------------------------
// Cleanup (task 016) — see `rimaia_core::worktree::cleanup`.
// ---------------------------------------------------------------------------

/// What the frontend sends [`remove_task_worktree`].
///
/// Mirrors [`RemovalAuthorization`] and exists only to case its fields the way
/// this boundary does — the core type is `snake_case`, because MCP is
/// (seam-contract D16.1), and every other key crossing the Tauri boundary is
/// `camelCase`. The same split [`crate::commands::runs::PruneCriterionInput`]
/// makes, for the same reason.
///
/// **Every field defaults to the refusing value**, so a caller that omits one
/// has authorised nothing. The dangerous values are the ones that have to be
/// typed, which is [`rimaia_core::worktree::ForceRemoval`]'s whole argument
/// carried across the wire.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct RemovalAuthorizationInput {
    pub uncommitted_changes: rimaia_core::worktree::ForceRemoval,
    pub unpushed_commits: rimaia_core::worktree::ForceRemoval,
    pub branch: rimaia_core::worktree::BranchDisposition,
}

impl From<RemovalAuthorizationInput> for RemovalAuthorization {
    fn from(input: RemovalAuthorizationInput) -> Self {
        RemovalAuthorization {
            uncommitted_changes: input.uncommitted_changes,
            unpushed_commits: input.unpushed_commits,
            branch: input.branch,
        }
    }
}

/// Every worktree with its task, branch, size, last activity and merged state,
/// plus the total task 016 shows alongside task 015's run-log usage.
#[tauri::command]
pub async fn get_worktree_inventory(state: State<'_, AppState>) -> Result<WorktreeInventory> {
    worktree::inventory(&state.context).await
}

/// Removes one task's worktree, subject to every guard in
/// `rimaia_core::worktree::cleanup`.
#[tauri::command]
pub async fn remove_task_worktree(
    state: State<'_, AppState>,
    task_id: String,
    authorization: RemovalAuthorizationInput,
) -> Result<RemovedWorktree> {
    worktree::remove_worktree(&state.context, &task_id, authorization.into()).await
}

/// Removes the worktree of every task in `done`, with every force off and every
/// branch kept — see [`rimaia_core::worktree::remove_done_worktrees`] on why a
/// bulk action may not carry more authority than the individual one.
#[tauri::command]
pub async fn cleanup_done_worktrees(state: State<'_, AppState>) -> Result<CleanupReport> {
    worktree::remove_done_worktrees(&state.context).await
}

/// The same, for every worktree whose branch the default branch already
/// contains.
#[tauri::command]
pub async fn cleanup_merged_worktrees(state: State<'_, AppState>) -> Result<CleanupReport> {
    worktree::remove_merged_worktrees(&state.context).await
}

/// Whether a task reaching `done` takes its worktree with it. Off unless
/// somebody turned it on.
#[tauri::command]
pub async fn get_worktree_auto_cleanup(state: State<'_, AppState>) -> Result<AutoCleanup> {
    worktree::auto_cleanup(&state.context.pool).await
}

/// Sets that policy. The `on` value is spelled `on_done_acknowledged` on the
/// wire as well as in the row: task 016 requires that enabling it means
/// acknowledging what it deletes, and the spelling is how the acknowledgement
/// survives past the dialog that collected it.
#[tauri::command]
pub async fn set_worktree_auto_cleanup(
    state: State<'_, AppState>,
    setting: AutoCleanup,
) -> Result<()> {
    worktree::set_auto_cleanup(&state.context, setting).await
}
