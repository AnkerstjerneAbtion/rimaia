mod commands;
mod logging;
// Public because `AppState`'s context is the shell's whole contract with the
// tasks that follow — 002 onward read it from commands in this crate.
pub mod state;

use std::fmt::Display;
use std::path::Path;
use std::sync::Arc;

use rimaia_core::{db, startup, AppPaths, ChangeEvent, Error, ServiceContext, SystemClock};
use tauri::{Emitter, Manager};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use crate::state::AppState;

/// The label of the one window `tauri.conf.json` declares. Spelled out because
/// that file leaves it implicit and Tauri fills it in — an unlabelled window
/// config defaults to `main`.
const MAIN_WINDOW_LABEL: &str = "main";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
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
            // `logging::init` already opened above, not a modal — D11 settles
            // that independently of whether `tauri-plugin-dialog` happens to be
            // a dependency (it now is, for task 003's folder picker below): a
            // path that has already failed is not the place to first reach for
            // it. The double-clicked-`.app`-with-nobody-watching-stderr case is
            // task 018's preflight doctor, not this hook.
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

            // One `ServiceContext` for the whole process (ADR-0018): the pool
            // above, a system clock, and a fresh change-event sender. Every
            // `rimaia-core` service task 003 onward calls goes through this,
            // never a bare pool — that is what makes the MCP server (task 010)
            // a second caller of the same rules instead of a second
            // implementation of them (ADR-0006).
            let context = ServiceContext::new(pool, Arc::new(SystemClock));

            // Subscribed once, here, for the life of the app (ADR-0018): the
            // shell is the only thing that turns a `ChangeEvent` into a Tauri
            // event, and it does so from one forwarding task rather than a
            // listener per window or per command. Subscribing before
            // `app.manage` hands `context` to any command matters less here
            // than it does in a test — nothing has published yet — but keeping
            // the order matches the rule anyway: subscribe first, then let
            // writers start.
            let change_events = context.subscribe();
            tauri::async_runtime::spawn(forward_change_events(app.handle().clone(), change_events));

            app.manage(AppState { context, paths });

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
        commands::repositories::list_repositories,
        commands::repositories::register_repository,
        commands::repositories::update_repository,
        commands::repositories::set_repository_unattended_runs,
        commands::repositories::remove_repository,
        commands::repositories::get_repository_remote_info,
        commands::tasks::create_task,
        commands::tasks::get_task,
        commands::tasks::list_tasks,
        commands::tasks::update_task,
        commands::tasks::delete_task,
        commands::tasks::move_task,
        commands::tasks::set_task_run_state,
        commands::tasks::add_task_link,
        commands::tasks::update_task_link,
        commands::tasks::remove_task_link,
        commands::tasks::reorder_task_link,
    ]);
    #[cfg(not(debug_assertions))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::app::get_app_info,
        commands::app::reveal_app_data_dir,
        commands::repositories::list_repositories,
        commands::repositories::register_repository,
        commands::repositories::update_repository,
        commands::repositories::set_repository_unattended_runs,
        commands::repositories::remove_repository,
        commands::repositories::get_repository_remote_info,
        commands::tasks::create_task,
        commands::tasks::get_task,
        commands::tasks::list_tasks,
        commands::tasks::update_task,
        commands::tasks::delete_task,
        commands::tasks::move_task,
        commands::tasks::set_task_run_state,
        commands::tasks::add_task_link,
        commands::tasks::update_task_link,
        commands::tasks::remove_task_link,
        commands::tasks::reorder_task_link,
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

/// The ADR-0018 forwarder: the one place a `rimaia-core` `ChangeEvent` becomes
/// a Tauri event. Runs for the life of the app on one subscription taken once
/// in `setup()` — not per window, not per command.
///
/// `RecvError::Lagged` does not end this loop. It means a subscriber fell
/// behind the broadcast channel's buffer and missed some publications, not
/// that anything is wrong with the app; the recovery is to tell every view to
/// re-read wholesale (an empty id array, ADR-0018's wire signal for that) and
/// carry on. `RecvError::Closed` is the only exit — every `ServiceContext`
/// clone holding the sender has been dropped, i.e. the app is shutting down.
async fn forward_change_events(
    app: tauri::AppHandle,
    mut events: broadcast::Receiver<ChangeEvent>,
) {
    loop {
        match events.recv().await {
            Ok(event) => emit_change_event(&app, event),
            Err(RecvError::Lagged(dropped)) => {
                tracing::warn!(
                    dropped,
                    "change-event receiver fell behind; telling every view to re-read"
                );
                emit_change_event(&app, ChangeEvent::tasks(Vec::<String>::new()));
                emit_change_event(&app, ChangeEvent::repositories(Vec::<String>::new()));
                emit_change_event(&app, ChangeEvent::runs(Vec::<String>::new()));
                emit_change_event(&app, ChangeEvent::Settings);
            }
            Err(RecvError::Closed) => break,
        }
    }
}

/// The variant-to-event-name mapping ADR-0018's table fixes. Kept as the only
/// place those strings appear in the shell, so a renamed event is a one-line
/// change here rather than a search across every window and command.
fn emit_change_event(app: &tauri::AppHandle, event: ChangeEvent) {
    let result = match &event {
        ChangeEvent::Tasks(ids) => app.emit("tasks:changed", ids.as_ref()),
        ChangeEvent::Repositories(ids) => app.emit("repositories:changed", ids.as_ref()),
        ChangeEvent::Runs(ids) => app.emit("runs:changed", ids.as_ref()),
        ChangeEvent::Settings => app.emit("settings:changed", ()),
    };
    if let Err(error) = result {
        tracing::error!(%error, "failed to forward a change event to the frontend");
    }
}
