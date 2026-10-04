//! The review verdicts, the two reads that inform them, the digest marker
//! (task 034, ADR-0006), and the read of what review runs found (task 035).
//!
//! One line each over `rimaia_core::review` and `tasks::dependents_of`. Every
//! refusal, the note's format and the digest's order are core's, so the MCP
//! tools of the same names cannot disagree with these.

use chrono::{DateTime, Utc};
use rimaia_core::db::Task;
use rimaia_core::review::{self, Dependent, Digest, FindingStatus, ReviewFinding, ReviewOutcome};
use rimaia_core::Result;
use tauri::State;

use crate::state::AppState;

/// `in_review` to the bottom of `done`.
#[tauri::command]
pub async fn approve_task(state: State<'_, AppState>, task_id: String) -> Result<Task> {
    review::approve(&state.context, &task_id).await
}

/// Back to `ready` for a fresh start: the worktree is removed and the branch is
/// set aside in git, never deleted. The note is required.
#[tauri::command]
pub async fn reject_task(
    state: State<'_, AppState>,
    task_id: String,
    note: String,
) -> Result<ReviewOutcome> {
    review::reject(&state.context, &task_id, &note).await
}

/// Back to `ready` to continue on the reviewed commits. The note is required.
#[tauri::command]
pub async fn request_task_changes(
    state: State<'_, AppState>,
    task_id: String,
    note: String,
) -> Result<ReviewOutcome> {
    review::request_changes(&state.context, &task_id, &note).await
}

/// The tasks that depend directly on this one, and which already built on it.
#[tauri::command]
pub async fn get_task_dependents(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<Dependent>> {
    review::dependents(&state.context, &task_id).await
}

/// What the queue did since the last finished review.
#[tauri::command]
pub async fn get_review_digest(state: State<'_, AppState>) -> Result<Digest> {
    review::digest(&state.context).await
}

/// Marks the digest seen through `through`, normally the `until` it was read at.
#[tauri::command]
pub async fn mark_review_digest_seen(
    state: State<'_, AppState>,
    through: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    review::mark_seen(&state.context, through).await
}

/// What review runs found on `task_id`, in review order and then in the order
/// each reviewer gave them, optionally of one status. The read only: no UI
/// writes a finding (seam-contract D30 point 5), so the two write tools have
/// no command.
#[tauri::command]
pub async fn list_review_findings(
    state: State<'_, AppState>,
    task_id: String,
    status: Option<FindingStatus>,
) -> Result<Vec<ReviewFinding>> {
    review::findings::list(&state.context, &task_id, status).await
}
