//! Starting and cancelling a run (task 008; ADR-0004, ADR-0011, ADR-0012,
//! ADR-0013).
//!
//! Every decision that matters — whether the repository has opted in,
//! whether the CLI is installed, how the process is supervised, how the
//! ending is classified, where the outcome is stored — lives in
//! `rimaia_core::runner`. What a desktop shell has to add on top of a single
//! `run_task` call is two things a library function cannot be: a button that
//! returns before the work it started is done, and something for a second
//! click on the same task to fail against instead of a second child process.
//!
//! "Read the *most recent* run's outcome" needs no command of its own: that is
//! `TaskDetail.lastRun`, which the board command `get_task` already returns —
//! the run this crate just started **is** the task's most recent attempt for
//! as long as the in-flight registry refuses a second concurrent one. The run
//! reads every client sees, `get_run` and the history lists among them, are
//! board commands in `rimaia_core::api::board::runs` (seam-contract D32); what
//! stays here starts, cancels or reads on this machine. The live tail's
//! catch-up read, `get_run_tail`, is a board command answered from the host's
//! tail relay (task 048).

use rimaia_core::runs::transcript::{self, SearchHit, TranscriptPage};
use rimaia_core::runs::{self, PruneCriterion, PruneResult};
use rimaia_core::{Error, Result};
use serde::Deserialize;
use tauri::State;
use tauri_plugin_opener::OpenerExt;

use crate::state::AppState;

/// Asks `task_id`'s in-flight run to stop.
///
/// A no-op, not an error, when nothing is running for a task the caller can
/// see — matching `CancelSignal::cancel`'s own idempotence. A task outside the
/// caller's scope is answered as a missing one (`runs::cancel_in_flight`). The
/// run itself terminates on ADR-0004's schedule (SIGTERM, a grace period, then
/// SIGKILL) inside `runner::process::execute`; this only delivers the request.
#[tauri::command]
pub async fn cancel_task_run(state: State<'_, AppState>, task_id: String) -> Result<()> {
    runs::cancel_in_flight(&state.context, &state.in_flight, &task_id).await
}

// ---------------------------------------------------------------------------
// Run history, run detail and the transcript viewer (task 015, ADR-0013)
// ---------------------------------------------------------------------------

/// One page of `run_id`'s transcript, oldest-shown-line first.
///
/// `limit` defaults to [`transcript::DEFAULT_PAGE_SIZE`] rather than being
/// required, so a caller that just wants "the next page" does not have to
/// repeat the constant on every call.
#[tauri::command]
pub async fn read_run_transcript_page(
    state: State<'_, AppState>,
    run_id: String,
    offset: usize,
    limit: Option<usize>,
) -> Result<TranscriptPage> {
    let log_path = runs::transcript_of(&state.context, &state.paths, &run_id).await?;
    transcript::read_page(
        &log_path,
        offset,
        limit.unwrap_or(transcript::DEFAULT_PAGE_SIZE),
    )
    .await
}

/// How `run_id`'s transcript begins and ends: the permission mode and model
/// the CLI reported, how many tool calls were refused, and whether the stream
/// reached a `result` at all.
///
/// A separate read from [`get_run`] rather than a field on it, because it
/// costs a scan of the file: a caller listing runs pays nothing, and the run
/// detail view — which is about to page that same file anyway — pays it once.
#[tauri::command]
pub async fn summarize_run_transcript(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<transcript::TranscriptSummary> {
    let log_path = runs::transcript_of(&state.context, &state.paths, &run_id).await?;
    transcript::summarize(&log_path).await
}

/// Text search across `run_id`'s whole transcript — inside tool inputs as
/// well as assistant messages, since [`transcript::search`] matches the raw
/// JSON line rather than a rendering of it.
#[tauri::command]
pub async fn search_run_transcript(
    state: State<'_, AppState>,
    run_id: String,
    query: String,
) -> Result<Vec<SearchHit>> {
    let log_path = runs::transcript_of(&state.context, &state.paths, &run_id).await?;
    transcript::search(&log_path, &query).await
}

/// Where one run's transcript is on this machine, derived from its ids
/// (ADR-0013): what "copy log path" copies, now that `Run` carries no
/// `logPath` (task 066). A local command (D32's appendix).
#[tauri::command]
pub async fn get_run_log_path(
    state: State<'_, AppState>,
    task_id: String,
    run_id: String,
) -> Result<String> {
    let log_path = runs::log_path(&state.context, &state.paths, &task_id, &run_id).await?;
    Ok(log_path.to_string_lossy().into_owned())
}

/// Reveals `run_id`'s raw JSONL transcript in the OS file manager — task
/// 015's "reveals the JSONL file".
///
/// # `reveal_item_in_dir`, not `open_path`
///
/// `open_path` hands the file to whatever the OS has registered for
/// `.jsonl`, through a **detached** child process: on a machine with no
/// handler for that extension the launch fails after this call has already
/// returned `Ok`, so the button did nothing and had nothing to say about it —
/// which is exactly how this was found. Revealing the file has no such gap
/// (the plugin canonicalizes the path and the failure comes back here), and
/// it is the better action anyway: a run's transcript is megabytes of JSONL,
/// and "show me where it is" is what a reviewer wants from it, not "open it
/// in a text editor".
///
/// The existence check is `rimaia_core::runs::log_path_to_reveal`'s, not this
/// adapter's — a missing transcript is the same fact `get_run` reports as
/// `logAvailable`, and it is stated once, in core (ADR-0006).
#[tauri::command]
pub async fn reveal_run_log(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    run_id: String,
) -> Result<()> {
    let log_path = runs::log_path_to_reveal(&state.context, &state.paths, &run_id).await?;
    app.opener()
        .reveal_item_in_dir(log_path)
        .map_err(|e| Error::internal(format!("could not reveal the log file: {e}")))
}

// ---------------------------------------------------------------------------
// Storage housekeeping (ADR-0013's "Retention")
// ---------------------------------------------------------------------------

/// Total bytes on disk across every run's transcript, for Settings' storage
/// report alongside worktree size.
#[tauri::command]
pub async fn get_run_log_size(state: State<'_, AppState>) -> Result<u64> {
    Ok(runs::total_log_size(&state.paths).await)
}

/// What the frontend sends [`prune_run_logs`]. Mirrors
/// [`rimaia_core::runs::PruneCriterion`] — the by-age and by-task actions
/// task 015's Scope names, and nothing else.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PruneCriterionInput {
    OlderThanDays {
        days: i64,
    },
    // The enum-level `rename_all` only cases the variant tag; this field's
    // own casing needs stating separately so the wire key stays `taskId`
    // like every other id on this boundary.
    #[serde(rename_all = "camelCase")]
    Task {
        task_id: String,
    },
}

/// Deletes transcript (and stderr) files matching `criterion`, leaving every
/// `runs` row untouched — see [`rimaia_core::runs::prune_logs`]'s own doc for
/// why the row survives.
#[tauri::command]
pub async fn prune_run_logs(
    state: State<'_, AppState>,
    criterion: PruneCriterionInput,
) -> Result<PruneResult> {
    let criterion = match criterion {
        PruneCriterionInput::OlderThanDays { days } => PruneCriterion::OlderThanDays(days),
        PruneCriterionInput::Task { task_id } => PruneCriterion::Task(task_id),
    };
    // `state.paths` for the second half: task 020's `strategy-*.jsonl` files
    // have no `runs` row, so they are found by walking the filesystem rather
    // than by any query (seam-contract D17.5, D20).
    runs::prune_logs(&state.context, &state.paths, criterion).await
}
