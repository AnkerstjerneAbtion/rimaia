//! What a crash left behind, repaired one runner at a time (ADR-0031 point 5,
//! ADR-0010, ADR-0011, seam-contract D9).
//!
//! # Each runner reconciles the leases it held
//!
//! Startup used to sweep every task in `running` or `queued`, whoever started
//! it. Shared, that would let one laptop's launch close every other machine's
//! runs, so from task 043 the sweep is three steps, in this order, and each
//! acts through the same services every other caller uses:
//!
//! 1. [`reconcile_held`] walks this runner's own `held_leases` and nothing
//!    else, through the board port. A lease with an open run is finished with
//!    the interrupted outcome, which the board lands by ADR-0011's table and
//!    pins to this runner; a lease with no run is released. A lease the board
//!    already closed answers `Conflict` or `NotFound`, and the record is
//!    dropped without touching the board.
//! 2. [`reconcile_unrecorded`] is solo's alone: what the board holds that the
//!    runner's record cannot know about. See its own doc for the two sets and
//!    why team mode has neither.
//! 3. `worktree::reconcile`, after both lease steps and never before: its
//!    `correct_run_state` moves a `running` task to `failed` through
//!    `set_run_state`, and run first it would leave the task's lease row
//!    behind, so every later claim of the task would be lost.
//!
//! # What "interrupted" is (seam-contract D9, and its 2026-09-03 amendment)
//!
//! `run_state` keeps exactly ADR-0007's seven values and `interrupted` is not
//! one of them — SQLite cannot widen a `CHECK`, so that is permanent rather
//! than provisional. A run that died with the app is recorded on its `runs` row
//! as `status = 'interrupted'` and `exit_class = 'interrupted'`, and the card
//! reads the word off its last run.
//!
//! # Offered, not performed
//!
//! The board decides whether a crashed run is worth resuming: ADR-0011's
//! budget gives it a `resume_after` when it allows one, so the task lands
//! `waiting_retry` with a due deadline, and `failed` exactly when it does not.
//! Nothing starts: seam-contract D15 has the exit path write `paused`,
//! `QueueState`'s default *is* `Paused` and `from_stored` falls back to it, so
//! a task sitting due at 03:00 waits for a human to press Start.

use chrono::{DateTime, Utc};

use crate::board::lease::{self as board_lease, Lease};
use crate::board::{service, BoardPort, FinishRun, LeaseRef, TranscriptEnd};
use crate::context::ServiceContext;
use crate::db::{ExitClass, RunState, RunStatus};
use crate::error::{ErrorCode, Result};
use crate::events::TeamId;
use crate::machine::{leases, MachineContext};
use crate::runner::events::TokenUsage;
use crate::runner::outcome::{finish_run, RunOutcome, SpawnedAs};
use crate::runs::bundle::RunCapture;
use crate::scheduler::attempts::{self, Ending};
use crate::scheduler::retry;
use crate::tasks::set_run_state;

/// What the run row of a process that died with the app says happened to it.
///
/// No metrics: an interrupted run never reached its `result` event, and
/// inventing a turn count for it would put a number on the row that nothing
/// measured.
///
/// `resume_after` is left `None` here and filled by [`interrupted_after`],
/// which is the only thing that has read the budget.
fn interrupted() -> RunOutcome {
    RunOutcome {
        exit_class: ExitClass::Interrupted,
        status: RunStatus::Interrupted,
        error_message: Some(
            "Rimaia stopped while this run was in flight; the run did not survive it".to_string(),
        ),
        num_turns: None,
        cost_usd: None,
        duration_ms: None,
        pr_url: None,
        usage_limit_resets_at: None,
        resume_after: None,
        // The same argument as the metrics above, and seam-contract D18 states
        // it: this reconciler never saw the process, so what it was spawned as
        // and what it spent are *not recorded* rather than zero. The run's own
        // `execute` would have filled these had it lived to return.
        spawned_as: SpawnedAs::default(),
        usage: TokenUsage::default(),
    }
}

/// The same, with ADR-0011's decision about whether this crash is worth
/// resuming from.
///
/// The budget is the ordinary one: `interrupted` resumes once immediately, and
/// every interruption after that spends the transient allowance — so an app
/// that crashes in the same place five nights running eventually stops offering
/// to try again, which is the runaway ADR-0011's cap exists to stop.
///
/// A failure to read the history is logged and read as "no resume", which is
/// the same conservative direction `runner::process::apply_retry_policy` takes
/// and for the same reason: a launch must open its window, and a card a human
/// has to press Start on is a better outcome than a repair that refused to
/// finish.
async fn interrupted_after(ctx: &ServiceContext, task_id: &str, run_id: &str) -> RunOutcome {
    let mut outcome = interrupted();

    let ending = Ending {
        exit_class: outcome.exit_class,
        usage_limit_resets_at: None,
    };
    match attempts::history(ctx, task_id, ending).await {
        Ok(Some(history)) => {
            // No window, unconditionally, and that is not a shortcut. This runs
            // at *launch*, where seam-contract D15's amendment guarantees there
            // is none: quitting closes the window, and a launch starts paused.
            // Passing the window that will be open at 22:00 tonight would cap a
            // decision about last night against a night that has not happened.
            outcome.resume_after =
                retry::decide(&history, ctx.clock.now(), run_id, None).resume_after();
        }
        Ok(None) => {}
        Err(error) => tracing::error!(
            %task_id, %run_id, %error,
            "could not read the attempt history of a run a crash caught; it will not be offered for resume",
        ),
    }

    outcome
}

/// The facts a runner reports for a run that died with the previous launch.
/// The board decides `resume_after` (D31 point 4), so the outcome carries none.
fn interrupted_finish() -> FinishRun {
    FinishRun {
        outcome: interrupted(),
        head_sha: None,
        bundle: None,
        window_closes_at: None,
        transcript: TranscriptEnd::KeptOnRunner,
    }
}

/// Reconciles every lease this runner recorded holding, and nothing else
/// (ADR-0031 point 5), and answers the tasks it settled.
///
/// - **With an open run**: `finish_run` with the interrupted outcome. The board
///   decides `resume_after` (D31 point 4) and lands the task by ADR-0011's
///   table, and the close pins it to this runner. With the budget spent the
///   board's `resume_after` is `None` and the task lands `failed`, so no second
///   hop is needed for a held lease.
/// - **With no run**: `release`. A `strategy` lease from `Plan` leaves
///   `run_state` alone, and an inline planner's lands `failed`. A run the board
///   already closed, which a crash between a `Continue` and its note can leave
///   recorded, is not open: its finish is refused, and the lease is released.
/// - **`Conflict` or `NotFound`**: the board already closed that lease. The
///   record is forgotten and nothing on the board is touched.
///
/// One failure is logged and does not stop the rest, and its record is kept
/// for the next launch: a launch that cannot settle one lease still has to
/// settle the others and open its window.
pub async fn reconcile_held(
    board: &dyn BoardPort,
    machine: &MachineContext,
) -> Result<Vec<String>> {
    let mut reconciled = Vec::new();

    for record in leases::held(machine).await? {
        let lease = record.lease();
        let task_id = record.task_id.as_str();
        let settled = match &record.run_id {
            Some(run_id) => match board.finish_run(&lease, run_id, interrupted_finish()).await {
                Err(error) if error.code() == ErrorCode::Invalid => board.release(&lease).await,
                finished => finished.map(drop),
            },
            None => board.release(&lease).await,
        };

        match settled {
            Ok(()) => {
                leases::forget(machine, task_id).await;
                reconciled.push(record.task_id.clone());
            }
            Err(error) if matches!(error.code(), ErrorCode::Conflict | ErrorCode::NotFound) => {
                tracing::debug!(
                    %task_id, %error,
                    "the board already closed a lease this runner recorded; dropping the record",
                );
                leases::forget(machine, task_id).await;
            }
            Err(error) => tracing::error!(
                %task_id, %error,
                "could not reconcile a lease this runner held",
            ),
        }
    }

    if !reconciled.is_empty() {
        tracing::warn!(
            tasks = reconciled.len(),
            "marked runs this runner left in flight as interrupted",
        );
    }
    Ok(reconciled)
}

/// What only a solo board can hold for its runner, reconciled through the
/// same services: run after [`reconcile_held`], with the task ids this runner
/// still records (`held`).
///
/// 1. **Leases the board records for `runner_id` that `held` does not.** The
///    process died between the claim's commit and the runner's record of it:
///    two stores cannot share a transaction, and a solo lease never expires,
///    so without this the task would stay `running` forever. Each is settled
///    as [`reconcile_held`] settles a record, under the lease itself: its open
///    run finished as interrupted (and pinned), or the lease released.
/// 2. **Tasks in `running` or `queued`, or with a run still open, and no
///    lease row at all.** A build older than task 043 left them. They are
///    reconciled exactly as before leases existed: each open run closed as
///    interrupted, then the task walked off whatever the crash caught it in,
///    `queued -> cancelled` included. An open run counts on its own because
///    another repair may already have moved the task on, and the row still
///    has to end.
///
/// **In team mode neither set can exist.** The first expires on the server
/// (task 053). The second cannot be written once the claim writes the edges
/// and the lease together, so its query is the one task 065 can delete.
pub async fn reconcile_unrecorded(
    ctx: &ServiceContext,
    runner_id: &str,
    held: &[String],
) -> Result<Vec<String>> {
    let mut reconciled = Vec::new();

    for (lease, team_id) in board_lease::held_by(ctx, runner_id).await? {
        if held.contains(&lease.task_id) {
            continue;
        }
        let task_id = lease.task_id.clone();
        match settle_unrecorded(ctx, runner_id, &lease, team_id).await {
            Ok(()) => reconciled.push(task_id),
            Err(error) => tracing::error!(
                %task_id, %error,
                "could not reconcile a lease this runner never recorded",
            ),
        }
    }

    for task_id in leaseless_in_flight(ctx).await? {
        match reconcile_one(ctx, &task_id).await {
            Ok(()) => reconciled.push(task_id),
            Err(error) => tracing::error!(
                %task_id, %error,
                "could not reconcile a task a previous build left running",
            ),
        }
    }

    if !reconciled.is_empty() {
        tracing::warn!(
            tasks = reconciled.len(),
            "marked runs no lease record named as interrupted",
        );
    }
    Ok(reconciled)
}

/// One lease the board holds for this runner that the runner never recorded.
async fn settle_unrecorded(
    ctx: &ServiceContext,
    runner_id: &str,
    lease: &Lease,
    team_id: TeamId,
) -> Result<()> {
    let reference = LeaseRef::new(lease.task_id.clone(), lease.generation, team_id);
    let open = open_runs(ctx, &lease.task_id).await?;
    match lease.run_id.as_ref().filter(|run_id| open.contains(run_id)) {
        Some(run_id) => {
            service::finish_run(ctx, runner_id, &reference, run_id, interrupted_finish()).await?;
        }
        None => service::release(ctx, runner_id, &reference).await?,
    }
    Ok(())
}

/// Tasks in `running` or `queued`, or with a run still open, that no lease row
/// names: what a build older than task 043 left, and nothing a 043 claim can
/// produce.
async fn leaseless_in_flight(ctx: &ServiceContext) -> Result<Vec<String>> {
    let scope = ctx.scope.json();
    let ids = sqlx::query_scalar!(
        "SELECT t.id FROM tasks t
          WHERE (t.run_state = ?1 OR t.run_state = ?2
                 OR EXISTS (SELECT 1 FROM runs r WHERE r.task_id = t.id AND r.ended_at IS NULL))
            AND t.team_id IN (SELECT value FROM json_each(?3))
            AND NOT EXISTS (SELECT 1 FROM runner_leases l WHERE l.task_id = t.id)
          ORDER BY t.id",
        RunState::Running,
        RunState::Queued,
        scope,
    )
    .fetch_all(&ctx.pool)
    .await?;
    Ok(ids)
}

async fn reconcile_one(ctx: &ServiceContext, task_id: &str) -> Result<()> {
    for run_id in open_runs(ctx, task_id).await? {
        // The row first, the task second, so a crash between them leaves the
        // outcome recorded and only the task stale — `finish_run`'s own
        // ordering argument, and the recoverable direction.
        let outcome = interrupted_after(ctx, task_id, &run_id).await;
        // Records nothing about the worktree: an interrupted row stays
        // `NotRecorded`, and the attempt that resumes it records its own.
        // Capturing here would put git on the board side of the port task 036
        // draws, for a row whose resumed attempt records a bundle anyway.
        if let Err(error) = finish_run(ctx, &run_id, &outcome, &RunCapture::default()).await {
            // The `runs` row is already written when this can fail; what failed
            // is the task-side transition, which `settle` takes from wherever
            // the task actually is.
            tracing::warn!(
                %task_id, %run_id, %error,
                "recording an interrupted run did not complete cleanly",
            );
        }
    }

    settle(ctx, task_id).await
}

/// Every attempt of `task_id` that was still in flight, oldest first.
///
/// `ended_at IS NULL` rather than `status = 'running'`: the column that says
/// "this row was never closed out" is the one whose absence a crash guarantees,
/// and `finish_run` refuses a row that already has it — so this is exactly the
/// set it will accept.
///
/// Realistically at most one: `start_run` is only ever reached by a caller
/// holding the claim. The loop is here because "realistically" is not an
/// invariant, and a second orphaned row would otherwise stay open forever.
async fn open_runs(ctx: &ServiceContext, task_id: &str) -> Result<Vec<String>> {
    let scope = ctx.scope.json();
    let ids = sqlx::query_scalar!(
        "SELECT r.id FROM runs r JOIN tasks t ON t.id = r.task_id
          WHERE r.task_id = ?1 AND r.ended_at IS NULL
            AND t.team_id IN (SELECT value FROM json_each(?2))
          ORDER BY r.attempt ASC",
        task_id,
        scope,
    )
    .fetch_all(&ctx.pool)
    .await?;
    Ok(ids)
}

/// Walks the task off whatever a crash caught it in, from wherever closing
/// its run left it.
///
/// `Running` for a task claimed before its `runs` row was ever opened, which
/// lands on `Failed`: there is no attempt to resume, because none was ever
/// opened.
///
/// `WaitingRetry` is where the row this module just closed put the task, and
/// the hop off it is now **conditional** — see the header. A task with a
/// deadline is one ADR-0011 wants offered for resume and is left alone; a task
/// without one has spent its budget and takes `WaitingRetry -> Failed`, which
/// the transition table describes as "retries exhausted... *when* the scheduler
/// decides to take it is task 009/014's policy, not this table's". This is task
/// 014 taking it, on the one condition task 009 could not evaluate.
///
/// `Queued` is the narrower crash: caught between the old two-edge claim's
/// separately committed transitions, before the second — `queued -> running`
/// — ever ran, so there is no open run for [`open_runs`] to have found above.
/// Only a build older than task 043 can leave it.
/// ADR-0007's machine has no `Queued -> Failed` edge, and adding one is a
/// bigger change than this repair needs; `Queued -> Cancelled` already exists
/// for exactly this shape of task, "waiting for its turn with no live process
/// to kill" (the same edge cancel-one takes on it). Anything else is a task
/// something already settled while this was running, and is left alone.
async fn settle(ctx: &ServiceContext, task_id: &str) -> Result<()> {
    let scope = ctx.scope.json();
    let run_state = sqlx::query_scalar!(
        r#"SELECT run_state AS "run_state: RunState" FROM tasks
            WHERE id = ?1 AND team_id IN (SELECT value FROM json_each(?2))"#,
        task_id,
        scope,
    )
    .fetch_optional(&ctx.pool)
    .await?;

    match run_state {
        Some(RunState::Running) => {
            set_run_state(ctx, task_id, RunState::Failed).await?;
        }
        Some(RunState::WaitingRetry) if !has_scheduled_resume(ctx, task_id).await? => {
            set_run_state(ctx, task_id, RunState::Failed).await?;
        }
        Some(RunState::Queued) => {
            set_run_state(ctx, task_id, RunState::Cancelled).await?;
        }
        _ => {}
    }

    Ok(())
}

/// Whether the newest attempt of `task_id` carries a deadline.
///
/// The newest, not "any": an older attempt's deadline was superseded by the one
/// that followed it, and a task whose latest attempt gave up is not rescued by
/// something two walls ago having been retryable.
async fn has_scheduled_resume(ctx: &ServiceContext, task_id: &str) -> Result<bool> {
    let scope = ctx.scope.json();
    let resume_after: Option<Option<DateTime<Utc>>> = sqlx::query_scalar!(
        r#"SELECT r.resume_after AS "resume_after: DateTime<Utc>"
             FROM runs r JOIN tasks t ON t.id = r.task_id
            WHERE r.task_id = ?1 AND t.team_id IN (SELECT value FROM json_each(?2))
            ORDER BY r.attempt DESC LIMIT 1"#,
        task_id,
        scope,
    )
    .fetch_optional(&ctx.pool)
    .await?;

    Ok(resume_after.flatten().is_some())
}
