//! Tauri commands for the run queue (task 009; ADR-0010, ADR-0007).
//!
//! Thin over `rimaia_core::scheduler::QueueHandle`, exactly like every other
//! command module (ADR-0006): every rule — selection, the claim, what a
//! `ready` task is skipped for — lives in `rimaia-core`, and task 010's MCP
//! server drives the same queue through the same handle, not through this
//! file. `lib.rs` builds the one `QueueHandle` for the process's lifetime and
//! hands a clone to `AppState`; this module never constructs one.

use rimaia_core::scheduler::QueueStatus;
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
/// task it holds a process for right now, and every `ready` task in board
/// order with the reason the queue will pass over each one it cannot start.
#[tauri::command]
pub async fn get_queue_status(state: State<'_, AppState>) -> Result<QueueStatus> {
    state.queue.status().await
}
