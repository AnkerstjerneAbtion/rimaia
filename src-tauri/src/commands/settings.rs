//! The run-environment, strategy-ceiling and prompt-preview commands (task
//! 006, ADR-0009, ADR-0012, seam-contract D3; task 072, ADR-0032). The team's
//! base instructions and the run-cost summary are board commands in
//! `rimaia_core::api::board::settings`.
//!
//! Every rule — what an absent key means, how `run_environment` parses, how
//! a prompt is actually composed from them — lives in
//! `rimaia_core::db::settings` and `rimaia_core::runner::prompt`. This file
//! only reshapes wire args and calls them, which is what makes
//! [`preview_composed_prompt`] trustworthy: it calls the exact function a
//! real run calls, so task 006's acceptance criterion — the Settings preview
//! matches what a run would receive, byte for byte — cannot drift into a
//! frontend-side approximation.

use rimaia_core::consent::ceiling::{self, StrategyCeiling};
use rimaia_core::db::{settings, RunEnvironment};
use rimaia_core::runner::prompt;
use rimaia_core::{repo, tasks, Result};
use tauri::State;

use crate::state::AppState;

#[tauri::command]
pub async fn get_run_environment(state: State<'_, AppState>) -> Result<RunEnvironment> {
    settings::run_environment(&state.machine).await
}

#[tauri::command]
pub async fn set_run_environment(state: State<'_, AppState>, value: RunEnvironment) -> Result<()> {
    settings::set_run_environment(&state.machine, value).await
}

/// This runner's strategy ceiling (ADR-0032 point 3, task 072): the models
/// and the highest effort a run on this machine may spend its owner's
/// subscription on. A runner setting, so local (seam-contract D32); no ceiling
/// is every existing install.
#[tauri::command]
pub async fn get_strategy_ceiling(state: State<'_, AppState>) -> Result<StrategyCeiling> {
    ceiling::strategy_ceiling(&state.machine).await
}

/// Replaces this runner's strategy ceiling. The next claim carries it and the
/// next spawn is judged against it; a run already spawned keeps what it was
/// spawned with.
#[tauri::command]
pub async fn set_strategy_ceiling(
    state: State<'_, AppState>,
    ceiling: StrategyCeiling,
) -> Result<()> {
    ceiling::set_strategy_ceiling(&state.machine, &ceiling).await
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
    let base = settings::base_instructions_for_task(&state.context, &task_id).await?;
    let detail = tasks::get_task(&state.context, &task_id).await?;
    let repository = repo::get(&state.context, &detail.task.repository_id).await?;
    let guidance = prompt::StrategyGuidance::for_task(&detail);
    let team_kind = tasks::team_kind_of_task(&state.context, &task_id).await?;

    Ok(prompt::compose_prompt(
        &base,
        &detail,
        &repository,
        // ADR-0032 point 7's facts name the runner a lease was granted to,
        // and a preview holds no lease. A personal team has none, and it is
        // the only one the desktop composes for before 059, so the preview
        // and a run still agree there byte for byte.
        prompt::PromptTeam::from_kind(team_kind, None),
        guidance.as_ref(),
        state.runner.provider.fanout_noun(),
    ))
}
