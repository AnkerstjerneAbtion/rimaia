mod commands;
mod logging;
// Public because `AppState`'s context is the shell's whole contract with the
// tasks that follow — 002 onward read it from commands in this crate.
pub mod state;

use std::fmt::Display;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rimaia_core::db::MutationSource;
use rimaia_core::doctor;
use rimaia_core::mcp::{self, McpState, RunHandles};
use rimaia_core::runner::events::RunTail;
use rimaia_core::runner::process::DEFAULT_GRACE_PERIOD;
use rimaia_core::runner::RunnerConfig;
use rimaia_core::scheduler::{self, InFlight};
use rimaia_core::{
    db, startup, worktree, AppPaths, ChangeEvent, Error, ServiceContext, SystemClock,
};
use tauri::{Emitter, Manager, RunEvent};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use crate::state::{AppState, RunTails};

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
            let report = match tauri::async_runtime::block_on(startup::survey(&pool)) {
                Ok(report) => report,
                Err(err) => {
                    log_startup_failure("startup survey", &paths.db_file(), &err);
                    return Err(err.into());
                }
            };

            // One `ServiceContext` for the whole process (ADR-0018): the pool
            // above, a system clock, and a fresh change-event sender. Every
            // `rimaia-core` service task 003 onward calls goes through this,
            // never a bare pool — that is what makes the MCP server (task 010)
            // a second caller of the same rules instead of a second
            // implementation of them (ADR-0006).
            // `Ui`, because this is the context the user's own commands write
            // through (ADR-0019). The scheduler and the MCP server are handed
            // this same context and each re-sources its own clone at
            // construction, so nothing below has to remember to pass a source.
            let context = ServiceContext::new(pool, Arc::new(SystemClock), MutationSource::Ui);

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

            // Task 008's D14 channel, subscribed the same way and for the same
            // reason: once, here, before anything can publish to it. `tails` is
            // the shell's own bookkeeping — the latest tail snapshot per run
            // (see `state::RunTails`'s doc) — and the forwarder is what keeps
            // it current. Which tasks have a process in flight used to be here
            // too; it is `rimaia_core::scheduler::InFlight` now.
            let tails = RunTails::new();
            let tail_events = context.subscribe_tail();
            tauri::async_runtime::spawn(forward_run_tail(
                app.handle().clone(),
                tail_events,
                tails.clone(),
            ));

            // Task 009's repair for `survey`'s `tasks_left_running` finding
            // (ADR-0010, ADR-0011, seam-contract D9): a task still `running`
            // is not a live condition (nothing was running when this process
            // started), it is what a crash left behind, and it has to be
            // settled *before* the queue below ever reads the board. A queue
            // that started selecting while a stale `running` row was still
            // sitting there could read it as legitimately in flight forever
            // (nothing in the MVP transitions a task out of `running` except
            // a run finishing) — this is what finishes it instead, through
            // the same services every other caller uses
            // (`runner::outcome::finish_run`, `tasks::set_run_state`), never a
            // raw `UPDATE`. It shares this startup with `worktree::reconcile`
            // below in either order — each only acts on a state the other has
            // not already produced — but it must land before the queue is
            // built, which is why it is sequenced here rather than after.
            let reconciled = match tauri::async_runtime::block_on(scheduler::reconcile_interrupted(
                &context, &report,
            )) {
                Ok(reconciled) => reconciled,
                Err(err) => {
                    log_startup_failure("reconcile interrupted runs", &paths.db_file(), &err);
                    return Err(err.into());
                }
            };
            if !reconciled.is_empty() {
                tracing::info!(
                    tasks = reconciled.len(),
                    "marked tasks a previous launch left running as interrupted",
                );
            }

            // Task 007's repair step for `survey`'s `missing_worktrees` finding
            // (ADR-0005: "repository state on disk is authoritative"). Placed
            // after the subscriber above, matching that step's own "subscribe
            // first, then let writers start" — `reconcile` is itself a writer,
            // just one that runs once at startup instead of once per command.
            // Unlike a failed migration this is recoverable: `reconcile` takes
            // no `Result`, logs and skips whatever it cannot clear, and so
            // cannot itself stop the window from opening.
            tauri::async_runtime::block_on(worktree::reconcile(
                &context,
                &report.missing_worktrees,
            ));

            // Task 009's one long-lived queue, built and spawned once here —
            // after both reconciliations above have committed, never before.
            // `tauri::async_runtime::spawn` rather than a bare `tokio::spawn`
            // for the reason every other background task in this hook uses
            // it: Tauri's async runtime *is* Tokio, so this rides the runtime
            // already here instead of starting a second.
            //
            // `in_flight` is built here and handed to both the queue and
            // `AppState`, which is what lets a manual "Run now", a "Plan now"
            // and the queue's own claim refuse to double-spawn a process for
            // the same task. It used to be an `attach_queue` back-reference
            // from a shell-side map onto the queue's private one; one registry
            // both doors read needs no wiring between them, and is reachable
            // from `rimaia-core` — which is what ADR-0021's known gap was
            // waiting on.
            //
            // Task 020's two shared values are built here, ahead of the queue,
            // rather than inside `scheduler::build`, because each is shared
            // with something the queue knows nothing about (see `AppState`'s
            // own docs): the `RunnerConfig` with every other starter, so a
            // manual "Run now" and the queue cannot spawn differently
            // configured processes for the same card; the `RunHandles` with the
            // MCP server below, which records the address it actually bound
            // into them on every bind, and with the runner, which mints one
            // scoped token per run against that same table.
            let run_handles = RunHandles::default();
            // The same table the MCP server below records its bound address
            // into. Cloning it here rather than leaving `RunnerConfig::default`'s
            // empty one is what lets a strategy run mint a token against an
            // endpoint that exists — with the default, `mcp_config_json` would
            // answer `None` forever and no task would ever plan.
            let runner = RunnerConfig {
                run_handles: run_handles.clone(),
                ..RunnerConfig::default()
            };
            let in_flight = InFlight::new();
            let (queue, queue_task) = scheduler::build(
                context.clone(),
                paths.clone(),
                runner.clone(),
                in_flight.clone(),
            );
            tauri::async_runtime::spawn(queue_task.run());

            // Task 010's MCP server (ADR-0006). Last of the startup steps on
            // purpose: this is the first thing in this hook that a process
            // *outside* Rimaia can write through, and nothing outside should
            // reach the board until every repair this startup was going to
            // make has been made — `reconcile_interrupted`, `worktree::reconcile`
            // and the queue's own construction all sit above it.
            let mcp_port = tauri::async_runtime::block_on(mcp::configured_port(&context.pool))
                .unwrap_or_else(|error| {
                    tracing::warn!(
                        %error,
                        port = mcp::DEFAULT_PORT,
                        "could not read mcp_port; using the default",
                    );
                    mcp::DEFAULT_PORT
                });
            let (mcp_handle, mcp_task) = tauri::async_runtime::block_on(mcp::build(
                context.clone(),
                mcp_port,
                run_handles.clone(),
                // The shell's own paths and runner, not `Programs::default` —
                // so `run_doctor` over MCP reports on the same `claude` binary
                // and the same data directory the window does. ADR-0021's
                // parity is only worth having if both surfaces answer about the
                // same installation.
                doctor::Environment::for_runner(paths.clone(), &runner),
            ));
            let mcp_status = mcp_handle.status();
            match mcp_status.state {
                McpState::Listening => {
                    tracing::info!(address = ?mcp_status.bound_address, "MCP server listening")
                }
                // Task 010's "surfaces a port-in-use error instead of failing
                // silently" — half of it. The other half is Settings → MCP,
                // reading the same status. Not fatal: seam-contract D16.
                McpState::PortInUse => tracing::warn!(
                    port = mcp_port,
                    error = ?mcp_status.message,
                    "the MCP port is taken; Rimaia is running without its MCP server — \
                     change the port in Settings → MCP",
                ),
                McpState::Stopped => tracing::warn!(
                    port = mcp_port,
                    error = ?mcp_status.message,
                    "the MCP server did not start",
                ),
            }
            tauri::async_runtime::spawn(mcp_task.run());

            app.manage(AppState {
                context,
                paths,
                in_flight,
                tails,
                queue,
                runner,
                run_handles,
                mcp: std::sync::Mutex::new(mcp_handle),
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
        commands::repositories::list_repositories,
        commands::repositories::register_repository,
        commands::repositories::update_repository,
        commands::repositories::set_repository_unattended_runs,
        commands::repositories::set_repository_max_concurrency,
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
        commands::settings::get_base_instructions,
        commands::settings::set_base_instructions,
        commands::settings::get_run_environment,
        commands::settings::get_run_cost_summary,
        commands::settings::set_run_environment,
        commands::settings::preview_composed_prompt,
        commands::strategy::get_strategy_catalogue,
        commands::strategy::set_strategy_catalogue,
        commands::strategy::get_strategy_defaults,
        commands::strategy::set_strategy_defaults,
        commands::strategy::get_strategy_approval,
        commands::strategy::set_strategy_approval,
        commands::strategy::accept_task_strategy,
        commands::strategy::clear_task_strategy,
        commands::strategy::plan_task_strategy,
        commands::worktree::get_worktree_status,
        commands::worktree::get_diff_summary,
        commands::worktree::reveal_task_worktree,
        commands::runs::start_task_run,
        commands::runs::cancel_task_run,
        commands::runs::retry_task_now,
        commands::runs::give_up_on_task,
        commands::runs::get_run_tail,
        commands::runs::list_runs_for_task,
        commands::runs::list_runs,
        commands::runs::get_run,
        commands::runs::read_run_transcript_page,
        commands::runs::search_run_transcript,
        commands::runs::summarize_run_transcript,
        commands::runs::reveal_run_log,
        commands::runs::get_run_log_size,
        commands::runs::prune_run_logs,
        commands::queue::start_queue,
        commands::queue::pause_queue,
        commands::queue::resume_queue,
        commands::queue::stop_queue,
        commands::queue::get_queue_status,
        commands::queue::get_run_capacity,
        commands::queue::set_schedule_mode,
        commands::queue::set_max_concurrency,
        commands::mcp::get_mcp_status,
        commands::mcp::set_mcp_port,
        commands::mcp::test_mcp_connection,
        commands::doctor::run_doctor,
        commands::doctor::dismiss_onboarding,
    ]);
    #[cfg(not(debug_assertions))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::app::get_app_info,
        commands::app::reveal_app_data_dir,
        commands::repositories::list_repositories,
        commands::repositories::register_repository,
        commands::repositories::update_repository,
        commands::repositories::set_repository_unattended_runs,
        commands::repositories::set_repository_max_concurrency,
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
        commands::settings::get_base_instructions,
        commands::settings::set_base_instructions,
        commands::settings::get_run_environment,
        commands::settings::get_run_cost_summary,
        commands::settings::set_run_environment,
        commands::settings::preview_composed_prompt,
        commands::strategy::get_strategy_catalogue,
        commands::strategy::set_strategy_catalogue,
        commands::strategy::get_strategy_defaults,
        commands::strategy::set_strategy_defaults,
        commands::strategy::get_strategy_approval,
        commands::strategy::set_strategy_approval,
        commands::strategy::accept_task_strategy,
        commands::strategy::clear_task_strategy,
        commands::strategy::plan_task_strategy,
        commands::worktree::get_worktree_status,
        commands::worktree::get_diff_summary,
        commands::worktree::reveal_task_worktree,
        commands::runs::start_task_run,
        commands::runs::cancel_task_run,
        commands::runs::retry_task_now,
        commands::runs::give_up_on_task,
        commands::runs::get_run_tail,
        commands::runs::list_runs_for_task,
        commands::runs::list_runs,
        commands::runs::get_run,
        commands::runs::read_run_transcript_page,
        commands::runs::search_run_transcript,
        commands::runs::summarize_run_transcript,
        commands::runs::reveal_run_log,
        commands::runs::get_run_log_size,
        commands::runs::prune_run_logs,
        commands::queue::start_queue,
        commands::queue::pause_queue,
        commands::queue::resume_queue,
        commands::queue::stop_queue,
        commands::queue::get_queue_status,
        commands::queue::get_run_capacity,
        commands::queue::set_schedule_mode,
        commands::queue::set_max_concurrency,
        commands::mcp::get_mcp_status,
        commands::mcp::set_mcp_port,
        commands::mcp::test_mcp_connection,
        commands::doctor::run_doctor,
        commands::doctor::dismiss_onboarding,
    ]);

    // `Builder::run(context)` is exactly `build(context)?.run(|_, _| {})`
    // (tauri 2.11.5 `src/app.rs:2449`) — spelling it out rather than using the
    // shorthand is what lets the closure below see `RunEvent::ExitRequested`.
    // Everything about setup and its failure mode (D11) is unchanged: the
    // setup hook still runs, and still panics on `Err`, inside `App::run`
    // itself, not during `build`.
    let app = builder
        .build(tauri::generate_context!())
        .expect("error while running tauri application");

    app.run(|app_handle, event| {
        if let RunEvent::ExitRequested { api, code, .. } = event {
            // Task 008's "all child processes reaped on app exit". Tauri
            // turns a completed exit into `std::process::exit`, which runs no
            // destructors at all — so the `ChildProcess::Drop` backstop
            // `rimaia_core::runner::process` documents for a panic or an
            // aborted future never fires on a plain quit, and cancelling has
            // to actually *finish* here rather than merely being requested.
            // `prevent_exit` delays this specific exit; `shut_down` cancels
            // every in-flight run, waits (bounded) for their process groups
            // to die, and then calls `AppHandle::exit`, which re-enters this
            // same closure with `code: Some(_)` set. Only the first request —
            // the user's own, always `code: None` — is prevented; the second
            // is let through, or this spins forever asking itself to quit.
            if code.is_none() {
                api.prevent_exit();
                let app_handle = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    shut_down(&app_handle).await;
                    app_handle.exit(0);
                });
            }
        }
    });
}

/// Cancels every run still in flight — manual, and task 009's queue — and
/// waits, bounded, for it to actually end before the app is allowed to finish
/// quitting.
///
/// A user who quits mid-run sees the same SIGTERM-then-grace-period-then-
/// SIGKILL sequence a manual Cancel would produce (ADR-0004) — quitting just
/// asks every in-flight run for it at once. The wait is capped so a child
/// that refuses to die cannot hold the app open indefinitely: past the
/// deadline, Rimaia exits anyway and `kill_on_drop` plus the process-group
/// signal already sent are left to finish the job.
///
/// `queue.shutdown()` runs **first** and, by itself, cancels nothing — it
/// only stops the queue's loop from claiming *more* tasks once the ones it is
/// supervising end (`scheduler::queue`'s own module doc explains why racing
/// that loop's next claim against this exit path, instead of ordering against
/// it, would leave a task claimed with nobody left supervising it). Cancelling
/// the runs actually in flight is `AppState::cancel_everything` right after,
/// which reaches every lease in the shared registry exactly as it reaches a
/// manual one. Net effect: quitting mid-run cancels those runs the same way
/// pressing the queue's own Stop button would — including that button's side
/// effect of leaving `queue_state = paused` for the next launch — and the wait
/// loop below is only the backstop for a child that refuses to die, not what
/// performs the cancellation itself.
///
/// **Both halves still hold with N runs (task 012), and neither needed a
/// change.** `cancel_all` signals every lease in one pass rather than the one
/// the queue happened to hold, so N children are SIGTERMed at the same instant
/// and the single grace period below covers all of them rather than N of them
/// in series. `has_in_flight_runs` is `!in_flight.is_empty()`, which is already
/// the question "are any left" and not "is the one left". And the two waiters —
/// this loop, and the queue's own `JoinSet` drain — converge on the same
/// condition from opposite sides without either being able to block the other:
/// the drain awaits supervisors that have already been asked to stop, and each
/// of them frees its lease on the way out, which is what this loop is watching.
async fn shut_down(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        // Nothing was ever `manage`d — setup failed before reaching that
        // point, and nothing here could have started a run either.
        return;
    };
    // Above `cancel_all`'s early return on purpose: quitting with nothing in
    // flight must still close the listener, or task 010's "stopping the app
    // makes the server unreachable" holds only on the nights a run happened to
    // be going.
    state
        .mcp
        .lock()
        .expect("the mcp handle mutex is poisoned")
        .shutdown();
    state.queue.shutdown();
    if !state.cancel_everything().await {
        return;
    }

    tracing::info!("cancelling in-flight runs before exiting");
    let deadline = tokio::time::Instant::now() + DEFAULT_GRACE_PERIOD + Duration::from_secs(5);
    while state.has_in_flight_runs() {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                "some runs were still terminating when Rimaia exited; \
                 their process groups were already signalled and should exit on their own"
            );
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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

/// The seam-contract D14 forwarder: the one place a `rimaia-core` `RunTail`
/// becomes a `runs:tail` Tauri event. Subscribed once in `setup()`, mirroring
/// [`forward_change_events`] in shape — but **not** in its recovery, which is
/// the one thing D14 says to do differently on purpose.
///
/// `RecvError::Lagged` here is **discarded and counted, never recovered.**
/// There is no empty-payload "re-read everything" signal on this channel,
/// because there is nothing to re-read: a dropped tail line is already on
/// disk in the run's JSONL transcript (ADR-0013), and the `runs` row is the
/// other source of truth if the two ever disagree. Building a recovery path
/// for a lossy, high-frequency view channel is exactly the mistake D14
/// separated this channel from `ChangeEvent` to avoid.
///
/// Every snapshot is also recorded into the shell's own catch-up cache
/// (`state::RunTails`) before it is forwarded, so a client that starts watching
/// mid-run and calls `get_run_tail` sees at least this one.
async fn forward_run_tail(
    app: tauri::AppHandle,
    mut events: broadcast::Receiver<RunTail>,
    tails: RunTails,
) {
    loop {
        match events.recv().await {
            Ok(tail) => {
                tails.record(tail.clone());
                if let Err(error) = app.emit("runs:tail", &tail) {
                    tracing::error!(%error, "failed to forward a run-tail snapshot to the frontend");
                }
            }
            Err(RecvError::Lagged(dropped)) => {
                tracing::debug!(
                    dropped,
                    "run-tail receiver fell behind; the dropped snapshots are already on disk \
                     in the run's transcript",
                );
            }
            Err(RecvError::Closed) => break,
        }
    }
}
