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
//! "Read a run's outcome and its log path" needs no command of its own: that
//! is `TaskDetail.lastRun`, which `commands::tasks::get_task` already
//! returns — the run this crate just started **is** the task's most recent
//! attempt for as long as [`crate::state::RunRegistry`] refuses a second
//! concurrent one, so a dedicated `get_run` would only read the same row a
//! second way.

use rimaia_core::runner::events::RunTail;
use rimaia_core::runner::{probe_cli, run_task, RunRequest, RunTrigger};
use rimaia_core::scheduler::{self, ClaimOutcome};
use rimaia_core::{repo, tasks, Error, Result};
use tauri::State;

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
