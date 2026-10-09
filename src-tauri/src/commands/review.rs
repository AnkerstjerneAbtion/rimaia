//! The review verdicts, the two reads that inform them, the digest marker
//! (task 034, ADR-0006), the read of what review runs found (task 035), and
//! the review loop's configuration (task 021).
//!
//! One line each over `rimaia_core::review`, `review_loop::config` and
//! `tasks::dependents_of`. Every refusal, the note's format and the digest's
//! order are core's, so the MCP tools of the same names cannot disagree with
//! these.

use chrono::{DateTime, Utc};
use rimaia_core::db::Task;
use rimaia_core::review::{self, Dependent, Digest, FindingStatus, ReviewFinding, ReviewOutcome};
use rimaia_core::review_loop::config::{
    self as review_config, ReviewLevel, ReviewLevelName, ReviewSettings, TaskReview,
};
use rimaia_core::review_loop::{self, ReviewConfig, ReviewHistory};
use rimaia_core::Result;
use serde_json::Value;
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

/// Every loop the task has had, with each review's findings, what the fix after
/// it resolved, and the loop's verdict and open counts. Grouping, what blocks
/// and the ping-pong lists are core's: the view renders this as it is returned
/// (task 037).
#[tauri::command]
pub async fn get_review_history(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<ReviewHistory> {
    review_loop::history(&state.context, &task_id).await
}

/// One level of the loop's configuration next to what it inherits and what it
/// resolves to, so the interface can say `Inherit (<value>)` without working
/// the precedence out itself (task 037). `id` is the repository's or the
/// task's, and absent for `global`.
#[tauri::command]
pub async fn get_review_level(
    state: State<'_, AppState>,
    level: ReviewLevelName,
    id: Option<String>,
) -> Result<ReviewLevel> {
    review_config::get_review_level(&state.context.pool, level, id.as_deref()).await
}

/// The global review instructions and loop configuration.
#[tauri::command]
pub async fn get_review_settings(state: State<'_, AppState>) -> Result<ReviewSettings> {
    review_config::get_review_settings(&state.context.pool).await
}

/// Replaces the global review instructions and configuration.
///
/// `config` arrives as raw JSON and goes to the service as it is, so
/// `"enabled": true` is refused in core's sentence rather than in Tauri's
/// deserializer, the same refusal the MCP tool gives.
#[tauri::command]
pub async fn set_review_settings(
    state: State<'_, AppState>,
    instructions: String,
    config: Value,
) -> Result<ReviewSettings> {
    review_config::set_review_settings(
        &state.context,
        state.runner.provider.as_ref(),
        &instructions,
        config,
    )
    .await
}

/// Replaces one repository's loop configuration.
#[tauri::command]
pub async fn set_repository_review_config(
    state: State<'_, AppState>,
    repository_id: String,
    config: Value,
) -> Result<ReviewConfig> {
    review_config::set_repository_review_config(
        &state.context,
        state.runner.provider.as_ref(),
        &repository_id,
        config,
    )
    .await
}

/// Replaces one task's override of the review instructions and its loop
/// configuration.
#[tauri::command]
pub async fn set_task_review(
    state: State<'_, AppState>,
    task_id: String,
    review_instructions: Option<String>,
    config: Value,
) -> Result<TaskReview> {
    review_config::set_task_review(
        &state.context,
        state.runner.provider.as_ref(),
        &task_id,
        review_instructions,
        config,
    )
    .await
}

#[cfg(test)]
mod tests {
    use rimaia_core::review_loop::ReviewConfig;
    use rimaia_core::strategy::Catalogue;
    use rimaia_core::ErrorCode;

    /// The four setters take `config` as raw JSON, so Tauri's deserializer
    /// accepts `"enabled": true` and core refuses it, in the sentence the MCP
    /// tool gives. A typed `ReviewConfig` parameter would have Tauri refuse it
    /// first, in a string the frontend cannot read as `{ code, message }`.
    #[test]
    fn a_command_hands_true_to_the_service_which_refuses_it() {
        let payload: serde_json::Value = serde_json::json!({ "enabled": true });

        let refused = ReviewConfig::from_door(payload, &Catalogue::default())
            .expect_err("only the acknowledgement turns the loop on");

        assert_eq!(refused.code(), ErrorCode::Invalid);
        assert!(refused.to_string().contains("on_cost_acknowledged"));
    }
}
