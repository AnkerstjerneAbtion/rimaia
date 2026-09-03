use rimaia_core::{Error, Result};
use tauri::State;
use tauri_plugin_opener::OpenerExt;

use crate::state::AppState;

/// Where Rimaia keeps its state. Surfaced in Settings so the user can find the
/// database and the logs without guessing at a platform convention.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub app_version: String,
    pub data_dir: String,
    pub db_file: String,
    pub logs_dir: String,
    /// Task 018's first-run flag, carried on the read the app already does at
    /// launch rather than on a command of its own.
    ///
    /// It rides along here for one specific reason: the opening view has to be
    /// decided *before the first frame*, and a second round trip to decide it is
    /// a frame of the board flashing up before the welcome screen replaces it.
    /// It is also not worth a command — and, under ADR-0021, not worth the MCP
    /// tool a command would owe.
    pub onboarding_dismissed: bool,
}

#[tauri::command]
pub async fn get_app_info(state: State<'_, AppState>) -> Result<AppInfo> {
    let paths = &state.paths;
    Ok(AppInfo {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        data_dir: paths.data_dir().display().to_string(),
        db_file: paths.db_file().display().to_string(),
        logs_dir: paths.logs_dir().display().to_string(),
        onboarding_dismissed: rimaia_core::db::settings::onboarding_dismissed(&state.context.pool)
            .await?,
    })
}

#[tauri::command]
pub fn reveal_app_data_dir(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<()> {
    // The path is passed as a value, not interpolated into a command line — the
    // macOS data directory contains a space.
    app.opener()
        .open_path(state.paths.data_dir().display().to_string(), None::<&str>)
        .map_err(|e| Error::internal(format!("could not open the app data directory: {e}")))
}

/// Exists only so the error path can be exercised end to end in development.
/// Task 001 has no command that legitimately fails, and "an error renders as a
/// readable message, not `[object Object]`" is an acceptance criterion that has
/// to be demonstrable. Compiled out of release builds.
#[cfg(debug_assertions)]
#[tauri::command]
pub fn debug_provoke_error() -> Result<()> {
    Err(Error::invalid(
        "this is what a backend error looks like in the UI",
    ))
}
