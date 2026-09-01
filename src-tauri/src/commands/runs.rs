//! Starting, cancelling and tailing a run (task 008; ADR-0004, ADR-0011,
//! ADR-0012, ADR-0013; seam-contract D14).
//!
//! Every decision that matters — whether the repository has opted in,
//! whether the CLI is installed, how the process is supervised, how the
//! ending is classified, where the outcome is stored — lives in
//! `rimaia_core::runner`. What a desktop shell has to add on top of a single
//! `run_task` call is two things a library function cannot be: a button that
//! returns before the work it started is done, and something for a second
//! click on the same task to fail against instead of a second child process.
//!
//! "Read the *most recent* run's outcome and its log path" needs no command
//! of its own: that is `TaskDetail.lastRun`, which `commands::tasks::get_task`
//! already returns — the run this crate just started **is** the task's most
//! recent attempt for as long as [`crate::state::RunRegistry`] refuses a
//! second concurrent one. Task 015's [`get_run`] below is a different read:
//! *any* attempt by id, with the branch's diff and commits alongside it
//! (ADR-0013), for a history list that shows every attempt rather than only
//! the last one.

use std::path::Path;

use chrono::{DateTime, Utc};
use rimaia_core::db::{Run, RunStatus};
use rimaia_core::runner::events::RunTail;
use rimaia_core::runner::{probe_cli, run_task, RunRequest, RunTrigger};
use rimaia_core::runs::transcript::{self, SearchHit, TranscriptPage};
use rimaia_core::runs::{self, PruneCriterion, PruneResult, RunDetail, RunFilter, RunListEntry};
use rimaia_core::scheduler::{self, ClaimOutcome};
use rimaia_core::{repo, tasks, Error, Result};
use serde::Deserialize;
use tauri::State;
use tauri_plugin_opener::OpenerExt;

use crate::state::{AppState, RunRegistry};

/// Releases `task_id`'s claim in `registry` on every exit from the scope that
/// holds it — the early `?` returns below, and (moved into the spawned
/// future) whatever happens to `run_task` after, panic included.
///
/// A trailing `registry.finish(&task_id)` statement only runs when the
/// future it is in returns normally; a panic anywhere in `run_task` unwinds
/// past it and leaves `task_id` claimed forever, so every later "Run now" on
/// that card is refused with "a run is already in progress" and nothing —
/// short of restarting the app — clears it. `Drop` runs on unwind as well as
/// on a normal return, which is the property this guard exists for.
///
/// `pub(crate)` since task 020: `commands::strategy::plan_task_strategy` starts
/// a second kind of child process against the same registry entry and needs the
/// same guarantee. One guard rather than two, for the same reason there is now
/// one `RunnerConfig` — a second copy is a second place for the release to be
/// forgotten.
pub(crate) struct ReleaseOnDrop {
    registry: RunRegistry,
    task_id: String,
}

impl ReleaseOnDrop {
    /// Takes over the registry entry [`RunRegistry::start`] has already
    /// claimed for `task_id`. It claims nothing itself — a guard that could
    /// also claim would be a second door onto the refusal `start` exists to
    /// make.
    pub(crate) fn new(registry: RunRegistry, task_id: String) -> Self {
        Self { registry, task_id }
    }
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.registry.finish(&self.task_id);
    }
}

/// Starts a manual run of `task_id` and returns as soon as it is under way —
/// **not once it finishes.**
///
/// `run_task` supervises a real `claude` process for however long the agent
/// takes; a command that awaited it would hold the "Run now" button's promise
/// open for the run's entire duration. The spawned task below is what
/// actually drives the long half — worktree preparation, the claim, the
/// process itself — and the caller learns how that goes the same way every
/// other view does: `tasks:changed` / `runs:changed` (ADR-0018) once the row
/// changes, and `runs:tail` (seam-contract D14) while it is in flight.
///
/// The two refusals `run_task` can raise **before writing any state** —
/// ADR-0012's repository opt-in, and the `claude` prerequisite itself — are
/// awaited here instead, so a click that cannot possibly start a run gets an
/// error the "Run now" button can render (`TaskCard`'s `runError`) rather than
/// one that only ever reaches `tracing::error!` inside a detached task nobody
/// is still watching. Both checks are read-only and `run_task` performs them
/// again as part of its own contract; that repeated `--version` and repeated
/// row read cost nothing a user notices and keep this command a thin caller
/// of the same rule rather than a second copy of it (ADR-0006).
///
/// `RunTrigger::Manual` is ADR-0012's conservative `acceptEdits` posture: this
/// command is the foreground "Run now" button. The unattended,
/// `bypassPermissions` path is task 009's queue, not this one.
///
/// # Why this also takes `scheduler::claim` before spawning anything
///
/// `state.runs.start`'s `queue_owns` check is an in-memory read of the
/// queue's own `in_flight_task_id` — set only once the queue's own claim has
/// already committed — so a click that lands in the gap between that commit
/// and the in-memory flag being set sails straight through it. Four awaits
/// separate that check from the spawn below (this function's own body,
/// unchanged since task 008), which is window enough: by the time
/// `run_task` re-reads the task, the queue may already have moved it to
/// `running`, and `run_task`'s own internal claim treats an already-`running`
/// task as "already mine" and spawns anyway (the arm task 008 wrote for
/// exactly one starter, before there was a second). The transactional claim
/// here closes that race for real, at the row: whichever caller's
/// `set_run_state` commits first owns the task, and `run_task`'s own claim
/// then no-ops on top of a state this call already produced. Refusing on
/// `Lost` rather than propagating a raw transition error also gives the
/// button the same sentence `queue_owns` already shows for the case it does
/// catch.
#[tauri::command]
pub async fn start_task_run(state: State<'_, AppState>, task_id: String) -> Result<()> {
    let cancel = state.runs.start(&task_id)?;
    // Releases the claim above on every exit from here on — the early `?`
    // returns immediately below, or, once moved into the spawned future,
    // however `run_task` ends there.
    let release = ReleaseOnDrop::new(state.runs.clone(), task_id.clone());

    let context = state.context.clone();
    // The process-wide config, not a fresh default: the queue spawns from this
    // same value, and a button that configured its child differently from the
    // queue's would be a difference nothing on screen could explain.
    let config = state.runner.clone();

    let detail = tasks::get_task(&context, &task_id).await?;
    let repository = repo::get(&context, &detail.task.repository_id).await?;
    repo::ensure_unattended_runs_allowed(&repository)?;
    probe_cli(&config.program).await?;

    if scheduler::claim(&context, &task_id).await? == ClaimOutcome::Lost {
        return Err(Error::invalid(
            "the run queue is already working on this task; pause or stop the queue, \
             or wait for it to finish, before starting it by hand",
        ));
    }

    let paths = state.paths.clone();
    let request = RunRequest {
        task_id: task_id.clone(),
        trigger: RunTrigger::Manual,
        cancel,
    };

    tauri::async_runtime::spawn(async move {
        let _release = release;
        if let Err(error) = run_task(&context, &paths, &config, request).await {
            tracing::error!(
                %task_id, %error,
                "a run could not be started or supervised",
            );
        }
    });

    Ok(())
}

/// Asks `task_id`'s in-flight run to stop.
///
/// A no-op, not an error, when nothing is running for it — matching
/// `CancelSignal::cancel`'s own idempotence. The run itself terminates on
/// ADR-0004's schedule (SIGTERM, a grace period, then SIGKILL) inside
/// `runner::process::execute`; this only delivers the request.
#[tauri::command]
pub fn cancel_task_run(state: State<'_, AppState>, task_id: String) -> Result<()> {
    state.runs.cancel(&task_id);
    Ok(())
}

/// The most recent live-tail snapshot the shell has seen for `run_id`, or
/// `None` when it has not seen one yet.
///
/// This is the read half of seam-contract D14: a client that opens the Runs
/// view after a run has already started has missed every `runs:tail` event
/// published so far, and this is what it reads once to catch up before
/// subscribing for the rest. See [`crate::state::RunRegistry`]'s own doc for
/// why this is the latest snapshot rather than a scrollback of every one.
#[tauri::command]
pub fn get_run_tail(state: State<'_, AppState>, run_id: String) -> Result<Option<RunTail>> {
    Ok(state.runs.tail(&run_id))
}

// ---------------------------------------------------------------------------
// Run history, run detail and the transcript viewer (task 015, ADR-0013)
// ---------------------------------------------------------------------------

/// Every run of `task_id`, newest attempt first — the task detail panel's
/// history list.
#[tauri::command]
pub async fn list_runs_for_task(state: State<'_, AppState>, task_id: String) -> Result<Vec<Run>> {
    runs::list_runs_for_task(&state.context, &task_id).await
}

/// What the frontend sends [`list_runs`]. Mirrors
/// [`rimaia_core::runs::RunFilter`] — a field left out matches everything,
/// the same contract [`super::tasks::TaskFilterInput`] states for the board.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunFilterInput {
    #[serde(default)]
    pub repository_id: Option<String>,
    #[serde(default)]
    pub status: Option<RunStatus>,
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    #[serde(default)]
    pub until: Option<DateTime<Utc>>,
}

/// The global Runs view's history: every run matching `filter`, newest
/// first, with its task's title and repository name for a list that spans
/// every repository.
#[tauri::command]
pub async fn list_runs(
    state: State<'_, AppState>,
    filter: RunFilterInput,
) -> Result<Vec<RunListEntry>> {
    runs::list_runs(
        &state.context,
        RunFilter {
            repository_id: filter.repository_id,
            status: filter.status,
            since: filter.since,
            until: filter.until,
        },
    )
    .await
}

/// One run's full detail: its own outcome, the branch's diff and commits
/// (ADR-0013's ordering), and whether its transcript file still resolves.
#[tauri::command]
pub async fn get_run(state: State<'_, AppState>, run_id: String) -> Result<RunDetail> {
    runs::get_run(&state.context, &run_id).await
}

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
    let run = runs::get_run_row(&state.context, &run_id).await?;
    transcript::read_page(
        Path::new(&run.log_path),
        offset,
        limit.unwrap_or(transcript::DEFAULT_PAGE_SIZE),
    )
    .await
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
    let run = runs::get_run_row(&state.context, &run_id).await?;
    transcript::search(Path::new(&run.log_path), &query).await
}

/// Reveals `run_id`'s raw JSONL transcript in the OS file manager — task
/// 015's "reveals the JSONL file". "Copy log path" needs no command: every
/// caller already has `Run.logPath` from [`get_run`] or
/// [`list_runs_for_task`], and the system clipboard is a browser API away.
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
    let log_path = runs::log_path_to_reveal(&state.context, &run_id).await?;
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
    runs::prune_logs(&state.context, criterion).await
}
