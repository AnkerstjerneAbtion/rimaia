mod commands;
mod logging;
// Public because `AppState`'s pool and clock are the shell's whole contract with
// the tasks that follow — 002 onward read them from commands in this crate.
pub mod state;

use std::sync::Arc;

use rimaia_core::{db, AppPaths, SystemClock};
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
