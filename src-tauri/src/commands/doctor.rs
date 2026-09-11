//! Tauri commands for the preflight doctor (task 018; ADR-0004, ADR-0012,
//! seam-contract D11's 2026-09-02 amendment, D22).
//!
//! Thin over `rimaia_core::doctor`, like every other command module. Note what
//! is *not* here: nothing decides whether a failing report blocks the queue.
//! That refusal lives on `QueueHandle::start`, so this file and the `run_doctor`
//! MCP tool report the same findings while the queue refuses on its own terms —
//! one rule, one place, both doors (ADR-0006).

use rimaia_core::db::settings::Dismissal;
use rimaia_core::doctor::{self, DoctorReport};
use rimaia_core::{db, Result};
use tauri::State;

use crate::state::AppState;

/// Runs every preflight check and hands back the whole report — passing rows
/// included.
///
/// The passing rows are not padding: the welcome flow shows each step its own
/// rows, and "done" there means a check passing rather than a button having been
/// clicked. A command that returned only problems could not express that.
///
/// The environment is built from the *shell's* `AppPaths` and the one shared
/// `RunnerConfig`, never from `Programs::default`, so the doctor reports on the
/// same `claude` binary and the same MCP endpoint a run would actually use.
#[tauri::command]
pub async fn run_doctor(state: State<'_, AppState>) -> Result<DoctorReport> {
    let environment = doctor::Environment::for_runner(state.paths.clone(), &state.runner);
    doctor::run(&state.context, &environment).await
}

/// Records that the first-run walkthrough is done with, or deliberately skipped.
///
/// Write-only, and there is no command to un-dismiss: the welcome screen is
/// reachable from Settings any time, so a second command would exist only to put
/// a screen back that the user can already open.
#[tauri::command]
pub async fn dismiss_onboarding(state: State<'_, AppState>) -> Result<()> {
    db::settings::set_onboarding_dismissed(&state.context, true).await
}

/// Puts one warning down, and answers with the whole set afterwards (task 027).
///
/// The set rather than nothing, because the banner has to stop showing that row
/// *now* and re-running the doctor to find out would be eight subprocesses to
/// hide one line. Marking is still core's — the next real report arrives
/// already marked — so the window's own update is an echo of the write rather
/// than a second opinion about it.
#[tauri::command]
pub async fn dismiss_doctor_warning(
    state: State<'_, AppState>,
    dismissal: Dismissal,
) -> Result<Vec<Dismissal>> {
    doctor::dismiss(&state.context, dismissal).await
}

/// Brings one back, including a dismissal that no longer matches any row.
#[tauri::command]
pub async fn restore_doctor_warning(
    state: State<'_, AppState>,
    dismissal: Dismissal,
) -> Result<Vec<Dismissal>> {
    doctor::restore(&state.context, &dismissal).await
}
