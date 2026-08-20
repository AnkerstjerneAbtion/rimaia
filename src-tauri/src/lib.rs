mod commands;
mod logging;
// Public because `AppState`'s pool and clock are the shell's whole contract with
// the tasks that follow — 002 onward read them from commands in this crate.
pub mod state;

use std::fmt::Display;
use std::path::Path;
use std::sync::Arc;

use rimaia_core::{db, startup, AppPaths, Error, SystemClock};
use tauri::Manager;

use crate::state::AppState;

/// The label of the one window `tauri.conf.json` declares. Spelled out because
/// that file leaves it implicit and Tauri fills it in — an unlabelled window
/// config defaults to `main`.
const MAIN_WINDOW_LABEL: &str = "main";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Order matters: the directories have to exist before the log
            // appender opens a file in one of them, and before SQLite is asked
            // to create a database in another.
            let data_dir = app.path().app_data_dir()?;
            let paths = AppPaths::new(data_dir);
            paths.create_all()?;

            logging::init(&paths.logs_dir());
            tracing::info!(data_dir = %paths.data_dir().display(), "rimaia starting");

            // Tauri's async runtime is Tokio, so this blocks the setup hook on
            // the runtime that is already there rather than starting a second.
            let pool = tauri::async_runtime::block_on(db::connect(&paths.db_file()))?;

            // Migrations run before the window is shown, and a failure here
            // aborts startup outright (seam-contract D11): there is no useful UI
            // to draw over a half-migrated database. "Fails loudly" is
            // deliberately just process exit + stderr + the rolling log file
            // `logging::init` already opened above, not a modal — that needs
            // `tauri-plugin-dialog`, which is not a dependency, and adding it
            // here for a path that has already failed would be the wrong place to
            // first reach for it. The
            // double-clicked-`.app`-with-nobody-watching-stderr case is task 018's
            // preflight doctor, not this hook.
            if let Err(err) = tauri::async_runtime::block_on(db::migrate(&pool)) {
                log_startup_failure("migrate", &paths.db_file(), &err);
                return Err(err.into());
            }

            // Nothing is running yet — the process that set any of this is the one
            // that just died — so whatever `survey` finds is history, not a live
            // condition. It logs its own warning when the report isn't empty and
            // otherwise only reads (see its module docs); repairing a stuck
            // `running` task, a vanished worktree or a missing run log belongs to
            // tasks 004, 007 and 008 respectively, not to startup.
            if let Err(err) = tauri::async_runtime::block_on(startup::survey(&pool)) {
                log_startup_failure("startup survey", &paths.db_file(), &err);
                return Err(err.into());
            }

            app.manage(AppState {
                pool,
                paths,
                clock: Arc::new(SystemClock),
            });

            // The window is declared `"visible": false` in `tauri.conf.json` and
            // shown here, once every fallible step above has succeeded. The two
            // halves are one mechanism and neither means anything alone, so do
            // not "simplify" either away: Tauri builds every window in the config
            // *before* it calls this hook (tauri 2.11.5 `src/app.rs:2524`), so
            // seam-contract D11's "the window never opens" is not the default and
            // has to be arranged. Drop the config flag and a failed migration
            // puts the window on screen and then takes it away again; drop this
            // call and a *successful* startup leaves an app with no window.
            app.get_webview_window(MAIN_WINDOW_LABEL)
                .ok_or_else(|| {
                    Error::internal(format!(
                        "no window labelled `{MAIN_WINDOW_LABEL}` to show — \
                         tauri.conf.json must declare one"
                    ))
                })?
                .show()?;

            Ok(())
        });

    // `generate_handler!` needs a literal list, so the debug-only command is
    // added by picking a whole list rather than by conditioning one entry.
    #[cfg(debug_assertions)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::app::get_app_info,
        commands::app::reveal_app_data_dir,
        commands::app::debug_provoke_error,
    ]);
    #[cfg(not(debug_assertions))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::app::get_app_info,
        commands::app::reveal_app_data_dir,
    ]);

    builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Records why a startup step failed, immediately before the error propagates
/// out of the setup hook and takes the process with it.
///
/// Propagating is what makes seam-contract D11's "fails loudly" true; this is
/// what makes it legible. Tauri turns an `Err` from the setup hook into a panic
/// at `RuntimeRunEvent::Ready`, and `panic!` does not go through `tracing` — so
/// every fallible step in that hook has to log for itself. Without this, a
/// double-clicked `.app`, which has nobody reading stderr, leaves
/// `<app-data>/logs/rimaia.log` holding nothing but the "rimaia starting" line.
fn log_startup_failure(step: &str, db_file: &Path, error: &impl Display) {
    tracing::error!(
        step,
        db_file = %db_file.display(),
        error = %error,
        "startup failed; the window will not open"
    );
}
