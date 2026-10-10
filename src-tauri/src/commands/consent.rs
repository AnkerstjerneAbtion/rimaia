//! Acceptance and the consent read (task 045, ADR-0032).
//!
//! Thin adapters over `rimaia_core::consent`, which the MCP tools of the same
//! names call too (ADR-0021 parity). Assignment lives with the other task
//! writes in `tasks.rs`, and the team ceiling with the repository commands.

use rimaia_core::consent::{self, pieces::ContentKind, TaskConsent};
use rimaia_core::Result;
use tauri::State;

use crate::state::AppState;

/// Records that the user read `revision` of one piece of content and accepts
/// running it. `task_id` is absent exactly for the team's base and review
/// instructions; only the current revision is accepted.
#[tauri::command]
pub async fn accept_content(
    state: State<'_, AppState>,
    team_id: String,
    task_id: Option<String>,
    kind: ContentKind,
    revision: String,
) -> Result<()> {
    consent::accept(
        &state.context,
        &team_id,
        task_id.as_deref(),
        kind,
        &revision,
    )
    .await
}

/// Why `runner_id`, one of the user's own runners, would or would not take
/// `task_id`: eligibility, the pin, the team ceiling and every missing piece.
#[tauri::command]
pub async fn get_task_consent(
    state: State<'_, AppState>,
    task_id: String,
    runner_id: String,
) -> Result<TaskConsent> {
    consent::status(&state.context, &task_id, &runner_id).await
}
