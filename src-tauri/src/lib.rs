mod commands;
mod logging;
// Public because `AppState`'s pool and clock are the shell's whole contract with
// the tasks that follow — 002 onward read them from commands in this crate.
pub mod state;

use std::sync::Arc;

use rimaia_core::{db, startup, AppPaths, SystemClock};
use tauri::Manager;

use crate::state::AppState;

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

            // Migrations run before the window opens, and a failure here aborts
            // startup outright (seam-contract D11): there is no useful UI to draw
            // over a half-migrated database. "Fails loudly" is deliberately just
            // process exit + stderr + the rolling log file `logging::init` already
            // opened above, not a modal — that needs `tauri-plugin-dialog`, which
            // is not a dependency, and adding it here for a path that has already
            // failed would be the wrong place to first reach for it. The
            // double-clicked-`.app`-with-nobody-watching-stderr case is task 018's
            // preflight doctor, not this hook.
            if let Err(err) = tauri::async_runtime::block_on(db::migrate(&pool)) {
                tracing::error!(
                    db_file = %paths.db_file().display(),
                    error = %err,
                    "migration failed; the window will not open"
                );
                return Err(err.into());
            }

            // Nothing is running yet — the process that set any of this is the one
            // that just died — so whatever `survey` finds is history, not a live
            // condition. It logs its own warning when the report isn't empty and
            // otherwise only reads (see its module docs); repairing a stuck
            // `running` task, a vanished worktree or a missing run log belongs to
            // tasks 004, 007 and 008 respectively, not to startup.
            tauri::async_runtime::block_on(startup::survey(&pool))?;

            app.manage(AppState {
                pool,
                paths,
                clock: Arc::new(SystemClock),
            });

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
