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
//! recent attempt for as long as the in-flight registry refuses a
//! second concurrent one. Task 015's [`get_run`] below is a different read:
//! *any* attempt by id, with the branch's diff and commits alongside it
//! (ADR-0013), for a history list that shows every attempt rather than only
//! the last one.

use std::path::Path;

use chrono::{DateTime, Utc};
use rimaia_core::db::{Run, RunStatus};
use rimaia_core::runner::events::RunTail;
use rimaia_core::runner::{probe_cli, run_task, ResumeSession, RunRequest, RunTrigger};
use rimaia_core::runs::transcript::{self, SearchHit, TranscriptPage};
use rimaia_core::runs::{self, PruneCriterion, PruneResult, RunDetail, RunFilter, RunListEntry};
use rimaia_core::scheduler::{self, ClaimOutcome, LeaseOwner};
use rimaia_core::{repo, tasks, Error, Result};
use serde::Deserialize;
use tauri::State;
use tauri_plugin_opener::OpenerExt;

use crate::state::AppState;

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
/// The lease is an in-memory fact about *this process*, and it is taken
/// before the queue's own claim can possibly have committed for a task the
/// queue picked in the same instant. Several awaits separate the lease from
/// the spawn below, which is window enough: by the time `run_task` re-reads
/// the task, the queue may already have moved it to `running`, and
/// `run_task`'s own internal claim treats an already-`running` task as
/// "already mine" and spawns anyway (the arm task 008 wrote for exactly one
/// starter, before there was a second). The transactional claim here closes
/// that race for real, at the row: whichever caller's `set_run_state` commits
/// first owns the task, and `run_task`'s own claim then no-ops on top of a
/// state this call already produced.
///
/// The two guards are not redundant. The lease stops *this* process starting
/// two children for one task, including the "Plan now" / "Run now" pair, which
/// no database state distinguishes. The claim stops two writers disagreeing
/// about a row, which an in-memory map cannot survive a restart to do.
#[tauri::command]
pub async fn start_task_run(state: State<'_, AppState>, task_id: String) -> Result<()> {
    let context = state.context.clone();
    // The process-wide config, not a fresh default: the queue spawns from this
    // same value, and a button that configured its child differently from the
    // queue's would be a difference nothing on screen could explain.
    let config = state.runner.clone();

    // Read before the lease rather than after, because a lease is taken per
    // repository and this is the only thing that knows which one. Both reads
    // are read-only, so the pair costs a query and changes nothing that a
    // second click could race: `acquire` is still the atomic step, and it is
    // still ahead of every spawn.
    let detail = tasks::get_task(&context, &task_id).await?;
    let repository = repo::get(&context, &detail.task.repository_id).await?;

    // `acquire_unbounded`, not `acquire`: the concurrency caps bound what the
    // *scheduler* starts, and a person clicking Run now with the app in front
    // of them is not the mis-set-configuration failure those settings exist
    // for. The per-task exclusion and the absolute ceiling still apply.
    //
    // The lease is also what replaced the hand-written `ReleaseOnDrop` guard
    // that used to live in this file: `Lease`'s own `Drop` frees the slot on
    // every path out of here, including a panic inside the spawned future, and
    // it is owned by the thing that hands out the claim rather than by each
    // caller who remembers to write the guard.
    let lease = state
        .in_flight
        .acquire_unbounded(&task_id, &repository.id, LeaseOwner::Manual)
        .map_err(|refused| Error::invalid(refused.message()))?;
    let cancel = lease.cancel_signal();

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
        // "Run now" starts, it does not continue. Resuming a session is
        // `retry_task_now`'s job, and the two are separate buttons because they
        // mean different things to a human looking at a card: one says "do this
        // work", the other says "carry on with the work you were doing".
        resume: None,
        cancel,
        // The same registry the queue hands its own runs, so a "Run now" and a
        // queued run in one repository take turns creating their worktrees
        // rather than racing `git worktree add` against one `.git` — see
        // `InFlight::preparation_lock`. This is exactly the pair the caps do
        // *not* keep apart (D19 point 5), which is what makes it the case that
        // needs the lock.
        in_flight: Some(state.in_flight.clone()),
    };

    tauri::async_runtime::spawn(async move {
        let _lease = lease;
        if let Err(error) = run_task(&context, &paths, &config, request).await {
            tracing::error!(
                %task_id, %error,
                "a run could not be started or supervised",
            );
        }
    });

    Ok(())
}

/// Resumes a task that is waiting out a retry, **now**, without waiting for its
/// deadline.
///
/// The operator's override of ADR-0011's wait: they can see that the window
/// reopened early, or simply want the attempt made while they are watching. It
/// deliberately ignores two things the queue honours — the task's own
/// `resume_after` and the global usage-limit hold — for the reason seam-contract
/// D19 point 5 gives about the concurrency caps: those bound the *scheduler*,
/// and a person clicking a button with the app in front of them is not the
/// unattended failure they exist for.
///
/// What it does **not** ignore is the session. This continues the attempt chain
/// rather than starting a fresh one, which is the whole of ADR-0011's "resume,
/// do not restart" — the worktree keeps its commits and the context is reused.
/// A task whose runs have been pruned away resumes as a fresh session, because
/// there is nothing left to continue.
///
/// The same two calls in the same order as `start_task_run` — lease, probe,
/// claim, spawn — with `claim_retry` in place of `claim` because the task is in
/// `waiting_retry` and that is the edge ADR-0007 gives it.
#[tauri::command]
pub async fn retry_task_now(state: State<'_, AppState>, task_id: String) -> Result<()> {
    let context = state.context.clone();
    let config = state.runner.clone();

    let detail = tasks::get_task(&context, &task_id).await?;
    let repository = repo::get(&context, &detail.task.repository_id).await?;

    let lease = state
        .in_flight
        .acquire_unbounded(&task_id, &repository.id, LeaseOwner::Manual)
        .map_err(|refused| Error::invalid(refused.message()))?;
    let cancel = lease.cancel_signal();

    repo::ensure_unattended_runs_allowed(&repository)?;
    probe_cli(&config.program).await?;

    if scheduler::claim_retry(&context, &task_id).await? == ClaimOutcome::Lost {
        return Err(Error::invalid(
            "this task is not waiting to be retried; the queue may have already picked it up, \
             or its retries may have run out",
        ));
    }

    let resume = scheduler::resumable_session(&context, &task_id)
        .await?
        .map(|session_id| ResumeSession { session_id });
    let paths = state.paths.clone();
    let request = RunRequest {
        task_id: task_id.clone(),
        trigger: RunTrigger::Manual,
        resume,
        cancel,
        in_flight: Some(state.in_flight.clone()),
    };

    tauri::async_runtime::spawn(async move {
        let _lease = lease;
        if let Err(error) = run_task(&context, &paths, &config, request).await {
            tracing::error!(
                %task_id, %error,
                "a resumed run could not be started or supervised",
            );
        }
    });

    Ok(())
}

/// Ends a task's retry loop: `waiting_retry -> failed`.
///
/// Thin over [`scheduler::give_up`], which is where the rule lives — the MCP
/// tool of the same name calls the identical function, which is what ADR-0006
/// asks for and ADR-0021 makes a rule.
#[tauri::command]
pub async fn give_up_on_task(state: State<'_, AppState>, task_id: String) -> Result<()> {
    scheduler::give_up(&state.context, &task_id).await
}

/// Asks `task_id`'s in-flight run to stop.
///
/// A no-op, not an error, when nothing is running for it — matching
/// `CancelSignal::cancel`'s own idempotence. The run itself terminates on
/// ADR-0004's schedule (SIGTERM, a grace period, then SIGKILL) inside
/// `runner::process::execute`; this only delivers the request.
#[tauri::command]
pub fn cancel_task_run(state: State<'_, AppState>, task_id: String) -> Result<()> {
    state.in_flight.cancel(&task_id);
    Ok(())
}

/// The most recent live-tail snapshot the shell has seen for `run_id`, or
/// `None` when it has not seen one yet.
///
/// This is the read half of seam-contract D14: a client that opens the Runs
/// view after a run has already started has missed every `runs:tail` event
/// published so far, and this is what it reads once to catch up before
/// subscribing for the rest. See [`crate::state::RunTails`]'s own doc for
/// why this is the latest snapshot rather than a scrollback of every one.
#[tauri::command]
pub fn get_run_tail(state: State<'_, AppState>, run_id: String) -> Result<Option<RunTail>> {
    Ok(state.tails.get(&run_id))
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
    let run = runs::get_run_row(&state.context, &run_id).await?;
    transcript::summarize(Path::new(&run.log_path)).await
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
