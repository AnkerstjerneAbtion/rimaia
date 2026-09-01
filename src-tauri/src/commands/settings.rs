//! Base instructions and run-environment commands (task 006, ADR-0009,
//! ADR-0012, seam-contract D3).
//!
//! Every rule — what an absent key means, how `run_environment` parses, how
//! a prompt is actually composed from them — lives in
//! `rimaia_core::db::settings` and `rimaia_core::runner::prompt`. This file
//! only reshapes wire args and calls them, which is what makes
//! [`preview_composed_prompt`] trustworthy: it calls the exact function a
//! real run calls, so task 006's acceptance criterion — the Settings preview
//! matches what a run would receive, byte for byte — cannot drift into a
//! frontend-side approximation.

use rimaia_core::db::{settings, RunEnvironment};
use rimaia_core::runner::outcome::{self, RunCostSummary};
use rimaia_core::runner::prompt;
use rimaia_core::{repo, tasks, Result};
use tauri::State;

use crate::state::AppState;

#[tauri::command]
pub async fn get_base_instructions(state: State<'_, AppState>) -> Result<String> {
    settings::base_instructions(&state.context.pool).await
}

/// Replaces `settings.base_instructions`. Never touches a run already
/// composed and stored: `runs.prompt` holds its own copy per ADR-0009, and
/// [`prompt::compose_prompt`] is pure, so there is no path from this write to
/// a prompt already on a `runs` row.
#[tauri::command]
pub async fn set_base_instructions(state: State<'_, AppState>, value: String) -> Result<()> {
    settings::set_base_instructions(&state.context, &value).await
}

#[tauri::command]
pub async fn get_run_environment(state: State<'_, AppState>) -> Result<RunEnvironment> {
    settings::run_environment(&state.context.pool).await
}

#[tauri::command]
pub async fn set_run_environment(state: State<'_, AppState>, value: RunEnvironment) -> Result<()> {
    settings::set_run_environment(&state.context, value).await
}

/// What runs on this machine have actually cost, so the environment toggle can
/// state its overhead as a share of real work rather than as the spike's ratio.
///
/// A separate command rather than a field on `get_run_environment`: the answer
/// changes every time a run finishes, where the setting changes when a human
/// edits it, and folding them together would make the cheap read as volatile
/// as the expensive one.
#[tauri::command]
pub async fn get_run_cost_summary(state: State<'_, AppState>) -> Result<RunCostSummary> {
    outcome::observed_run_cost(&state.context.pool).await
}

/// The prompt `task_id` would receive right now, composed the same way task
/// 008 composes one for a real run.
///
/// Reads the current base instructions and the task fresh on every call
/// rather than caching either, so an edit not yet saved anywhere else is
/// never shown as if it had already taken effect. The recorded proposal is read
/// the same way, through [`prompt::StrategyGuidance::for_task`] rather than
/// interpreted here: task 006's criterion is that this preview and a real run
/// agree byte for byte, and a second reading of the `strategy_plan` envelope in
/// this file is exactly how they would stop agreeing.
#[tauri::command]
pub async fn preview_composed_prompt(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<String> {
    let base = settings::base_instructions(&state.context.pool).await?;
    let detail = tasks::get_task(&state.context, &task_id).await?;
    let repository = repo::get(&state.context, &detail.task.repository_id).await?;
    let guidance = prompt::StrategyGuidance::for_task(&detail);

    Ok(prompt::compose_prompt(
        &base,
        &detail,
        &repository,
        guidance.as_ref(),
    ))
}
