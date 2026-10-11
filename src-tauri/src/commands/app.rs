use rimaia_core::api::capabilities::ClientCapabilities;
use rimaia_core::{Error, Result};
use tauri::State;
use tauri_plugin_dialog::DialogExt;
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
        onboarding_dismissed: rimaia_core::db::settings::onboarding_dismissed(&state.machine)
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

/// What this client can do (ADR-0034 point 5, task 049): a desktop can do
/// every local action, and says whether its board is its own file or a server.
///
/// Reads the runner's identity off this machine's store and never the board's
/// context (D32 point 8). A store nothing has adopted into yet is solo, with
/// the installation's own runner.
#[tauri::command]
pub async fn get_client_capabilities(state: State<'_, AppState>) -> Result<ClientCapabilities> {
    let identity = rimaia_runner::identity::current(&state.runner_store).await?;
    Ok(match identity {
        Some(identity) => {
            ClientCapabilities::desktop(identity.runner_id, identity.server_url.as_deref())
        }
        None => ClientCapabilities::desktop(state.solo.runner_id.clone(), None),
    })
}

/// Opens the native folder picker and answers the folder chosen, or `None`
/// when the user cancelled (task 049).
///
/// Its own command so no component imports a Tauri plugin (ADR-0034 point 4).
/// It does not use `blocking_pick_folder`: that blocks the thread it runs on,
/// and `lib.rs`'s comment on the startup dialog explains how blocking the wrong
/// thread deadlocks the window. The callback hands the answer to a channel
/// this async command awaits instead.
#[tauri::command]
pub async fn choose_folder(app: tauri::AppHandle, title: String) -> Result<Option<String>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title(title)
        .pick_folder(move |chosen| {
            // The receiver is gone only if the command was dropped, and then
            // nobody is waiting for the answer.
            let _ = sender.send(chosen);
        });
    let Some(chosen) = receiver
        .await
        .map_err(|_| Error::internal("the folder picker closed without an answer"))?
    else {
        return Ok(None);
    };
    let path = chosen
        .into_path()
        .map_err(|error| Error::internal(format!("could not read the chosen folder: {error}")))?;
    match path.to_str() {
        Some(path) => Ok(Some(path.to_string())),
        None => Err(Error::invalid(format!(
            "the chosen folder's path is not valid UTF-8: {}",
            path.display()
        ))),
    }
}
