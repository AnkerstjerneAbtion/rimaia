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

use rimaia_core::worktree::{self, WorktreeStatus};
use rimaia_core::{tasks, Error, Result};
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
