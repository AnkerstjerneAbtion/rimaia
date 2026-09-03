//! Tauri commands for named schedules (task 013; ADR-0010).
//!
//! Thin over `rimaia_core::schedule`, exactly like every other command module
//! (ADR-0006): which combinations of columns are legal, what a missing timezone
//! means, and when a schedule is due all live in core, because the MCP tools
//! call the same functions without passing through here.
//!
//! Nothing in this file starts a queue. The timer is a third arm of the
//! scheduler's own `select!` — see `rimaia_core::scheduler::queue`'s header on
//! why it is not a second task, and why it is emphatically not a command.

use rimaia_core::db::Schedule;
use rimaia_core::schedule::{self, PreflightSummary, ScheduleInput, ScheduleView};
use rimaia_core::Result;
use tauri::State;

use crate::state::AppState;

/// Every schedule, with the next fire time computed for each.
///
/// The next fire time is the reason this is not just a table read: task 013's
/// whole point is that a wrong cron expression is visible **in the evening**,
/// and a list without it would be a list nobody could check.
#[tauri::command]
pub async fn list_schedules(state: State<'_, AppState>) -> Result<Vec<ScheduleView>> {
    schedule::list(&state.context).await
}

/// Creates a schedule, armed from now.
#[tauri::command]
pub async fn create_schedule(
    state: State<'_, AppState>,
    input: ScheduleInput,
) -> Result<Schedule> {
    schedule::create(&state.context, input).await
}

/// Replaces a schedule's configuration, leaving its fire history alone.
#[tauri::command]
pub async fn update_schedule(
    state: State<'_, AppState>,
    id: String,
    input: ScheduleInput,
) -> Result<Schedule> {
    schedule::update(&state.context, &id, input).await
}

/// Turns a schedule on or off without deleting its configuration (task 013's
/// fifth acceptance criterion).
///
/// Its own command rather than a field on [`update_schedule`], on the precedent
/// `set_repository_unattended_runs` sets: a toggle in a list is one click, and
/// routing it through a form that has to send every other field back unchanged
/// is how a toggle silently reverts an edit somebody else made.
#[tauri::command]
pub async fn set_schedule_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<Schedule> {
    schedule::set_enabled(&state.context, &id, enabled).await
}

#[tauri::command]
pub async fn delete_schedule(state: State<'_, AppState>, id: String) -> Result<()> {
    schedule::delete(&state.context, &id).await
}

/// What this schedule would do if it fired now: which tasks will run, in what
/// order, and which are blocked and why.
///
/// The evening button. Computed from `selection::plan` — the same function the
/// queue loop itself calls — so it cannot drift from what actually happens.
#[tauri::command]
pub async fn preview_schedule_preflight(
    state: State<'_, AppState>,
    id: String,
) -> Result<PreflightSummary> {
    schedule::preview(&state.context, &id).await
}

/// Every IANA zone name, for the picker.
///
/// A command rather than a table shipped in the frontend, which is what keeps
/// the npm dependency count where it is: the list the `<select>` offers and the
/// list the service will accept come from one `chrono-tz` table, so a pickable
/// name is a storable name by construction.
#[tauri::command]
pub fn list_timezones() -> Vec<String> {
    schedule::timezones()
}
