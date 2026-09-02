//! Tauri commands for the run queue (task 009; ADR-0010, ADR-0007).
//!
//! Thin over `rimaia_core::scheduler::QueueHandle`, exactly like every other
//! command module (ADR-0006): every rule — selection, the claim, what a
//! `ready` task is skipped for — lives in `rimaia-core`, and task 010's MCP
//! server drives the same queue through the same handle, not through this
//! file. `lib.rs` builds the one `QueueHandle` for the process's lifetime and
//! hands a clone to `AppState`; this module never constructs one.

use rimaia_core::db::ScheduleMode;
use rimaia_core::scheduler::{capacity, QueueStatus, RunCapacity};
use rimaia_core::Result;
use tauri::State;

use crate::state::AppState;

/// Starts working the `ready` column top-down, one task at a time. Idempotent
/// — starting an already-running queue is not an error.
#[tauri::command]
pub async fn start_queue(state: State<'_, AppState>) -> Result<()> {
    state.queue.start().await
}

/// The same action as [`start_queue`], under the name the user presses after
/// a pause. `QueueHandle` makes no distinction between the two — see its own
/// doc for why there is nothing for one to make.
#[tauri::command]
pub async fn resume_queue(state: State<'_, AppState>) -> Result<()> {
    state.queue.resume().await
}

/// Starts nothing new; lets the current run finish.
#[tauri::command]
pub async fn pause_queue(state: State<'_, AppState>) -> Result<()> {
    state.queue.pause().await
}

/// Pause, plus cancel whatever the queue is currently running (ADR-0010's
/// SIGTERM-then-grace-period-then-SIGKILL sequence, same as a manual cancel).
#[tauri::command]
pub async fn stop_queue(state: State<'_, AppState>) -> Result<()> {
    state.queue.stop().await
}

/// The whole picture for the Runs view: whether the queue is running, which
/// tasks it holds processes for right now, and every `ready` task in board
/// order with the reason the queue will pass over each one it cannot start.
#[tauri::command]
pub async fn get_queue_status(state: State<'_, AppState>) -> Result<QueueStatus> {
    state.queue.status().await
}

/// How many runs the queue may have in flight, as configured (ADR-0010).
///
/// One command for the mode, the limit and the ceiling, because the Settings
/// control renders all three together and three round trips to draw one panel
/// is three chances to draw it half-updated.
#[tauri::command]
pub async fn get_run_capacity(state: State<'_, AppState>) -> Result<RunCapacity> {
    capacity::configured(&state.context.pool).await
}

/// Switches the queue between one run at a time and several (ADR-0010's Modes).
///
/// Returns the whole configuration rather than nothing, on the precedent
/// `set_mcp_port` sets: the caller already asked the question this answers, and
/// a re-read is a second chance to disagree with what was just written.
#[tauri::command]
pub async fn set_schedule_mode(
    state: State<'_, AppState>,
    mode: ScheduleMode,
) -> Result<RunCapacity> {
    capacity::set_schedule_mode(&state.context, mode).await?;
    capacity::configured(&state.context.pool).await
}

/// How many runs [`ScheduleMode::Parallel`] may have in flight at once.
///
/// Refused outside `1..=ceiling`, with a sentence the panel renders — the
/// tolerance that lets a hand-edited row through does not extend to a form.
#[tauri::command]
pub async fn set_max_concurrency(state: State<'_, AppState>, value: usize) -> Result<RunCapacity> {
    capacity::set_max_concurrency(&state.context, value).await?;
    capacity::configured(&state.context.pool).await
}
