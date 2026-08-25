//! What a crash left behind, repaired (ADR-0010, ADR-0011, seam-contract D9).
//!
//! [`startup::survey`](crate::startup::survey) finds it and deliberately
//! repairs nothing — its own module doc explains why, and the reason is
//! ADR-0006: a second writer of `run_state`, even a well-meaning one in a
//! startup hook, is how one invariant becomes two. So the survey stays
//! read-only and hands this function a list of ids, and this function acts
//! through the same services every other caller uses:
//! [`finish_run`](crate::runner::outcome::finish_run) for the `runs` row,
//! [`set_run_state`] for the task.
//!
//! # What "interrupted" is (seam-contract D9)
//!
//! `run_state` keeps exactly ADR-0007's seven values and `interrupted` is not
//! one of them — SQLite cannot widen a `CHECK`, so that is permanent rather
//! than provisional. A run that died with the app is recorded on its `runs` row
//! as `status = 'interrupted'` and `exit_class = 'interrupted'`; the task lands
//! in `run_state = 'failed'` and stays in `ready`; the card reads the word off
//! its last run. Task 009's acceptance criterion — "reopening shows accurate
//! state: one `interrupted` task" — is a statement about what the user sees,
//! and this is what makes it true.
//!
//! # Why the task takes two transitions to get there
//!
//! `finish_run` applies ADR-0011's action column, and ADR-0011 gives an
//! `interrupted` run "resume once immediately, then treat as transient" — so it
//! lands the task in `waiting_retry`. That is right for a run interrupted while
//! the app kept going, and wrong for one interrupted *by the app going away*:
//! nothing in the MVP resumes a `waiting_retry` task (that is task 014), so it
//! would sit invisible rather than interrupting the morning review the way
//! ADR-0007 wants a failure to. [`settle`] therefore takes the second edge,
//! `WaitingRetry -> Failed` — which the transition table describes as "retries
//! exhausted... *when* the scheduler decides to take it is task 009/014's
//! policy, not this table's". This is task 009 taking it.
//!
//! # A task a crash caught still `queued`
//!
//! `startup::survey`'s `tasks_left_running` also reports a task at `queued` —
//! `scheduler::claim` walks `idle -> queued -> running` as two separately
//! committed transitions, and a crash between them leaves a task there with
//! no open run for [`open_runs`] to find and no legal edge back to `idle`.
//! Left alone it is a trap worse than `running`: `selection::skip_reason`
//! only ever claims from `idle`, so the queue passes over it forever, and
//! nothing else in the product writes `run_state` to clear it. [`settle`]
//! takes it to `Cancelled` rather than `Failed` — ADR-0007's machine has no
//! `Queued -> Failed` edge, and `Queued -> Cancelled` is already the edge a
//! task with no live process to kill takes, which is exactly this shape.
//!
//! # It shares a startup with `worktree::reconcile`, in either order
//!
//! Task 007's repair walks `survey`'s `missing_worktrees` and already lands a
//! `running` task on `failed` when its directory vanished, so a crash that took
//! both leaves two repairs looking at one task. They converge whichever runs
//! first, because each only acts on a state the other has not already produced:
//! run this one second and [`settle`] finds `failed` and leaves it; run it
//! first and task 007's `correct_run_state` does. The `runs` row is closed
//! either way — `finish_run` writes and commits it *before* it applies anything
//! to the task, so even the ordering where its task-side transition is refused
//! still leaves a reviewable attempt behind.

use crate::context::ServiceContext;
use crate::db::{ExitClass, RunState, RunStatus};
use crate::error::Result;
use crate::runner::outcome::{finish_run, RunOutcome};
use crate::startup::ReconciliationReport;
use crate::tasks::set_run_state;

/// What the run row of a process that died with the app says happened to it.
///
/// No metrics: an interrupted run never reached its `result` event, and
/// inventing a turn count for it would put a number on the row that nothing
/// measured.
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
    }
}

/// Closes out every task a crash left `running`, and reports which ones.
///
/// Takes the report rather than re-querying, so there is one definition of
/// "left running" and the survey stays the only thing that decides what counts
/// (see this module's header).
///
/// One bad row does not stop the rest: a launch that cannot repair one task
/// still has to repair the others and still has to open the window, so failures
/// are logged per task and the ids that did land come back.
pub async fn reconcile_interrupted(
    ctx: &ServiceContext,
    report: &ReconciliationReport,
) -> Result<Vec<String>> {
    let mut reconciled = Vec::new();

    for task_id in &report.tasks_left_running {
        match reconcile_one(ctx, task_id).await {
            Ok(()) => reconciled.push(task_id.clone()),
            Err(error) => tracing::error!(
                %task_id, %error,
                "could not reconcile a task a previous run left running",
            ),
        }
    }

    if !reconciled.is_empty() {
        tracing::warn!(
            tasks = reconciled.len(),
            "marked runs a previous launch left in flight as interrupted",
        );
    }

    Ok(reconciled)
}

async fn reconcile_one(ctx: &ServiceContext, task_id: &str) -> Result<()> {
    for run_id in open_runs(ctx, task_id).await? {
        // The row first, the task second, so a crash between them leaves the
        // outcome recorded and only the task stale — `finish_run`'s own
        // ordering argument, and the recoverable direction.
        if let Err(error) = finish_run(ctx, &run_id, &interrupted()).await {
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
    let ids = sqlx::query_scalar!(
        "SELECT id FROM runs WHERE task_id = ?1 AND ended_at IS NULL ORDER BY attempt ASC",
        task_id,
    )
    .fetch_all(&ctx.pool)
    .await?;
    Ok(ids)
}

/// Walks the task off whatever a crash caught it in, from wherever closing
/// its run left it.
///
/// `Running` for a task claimed before its `runs` row was ever opened,
/// `WaitingRetry` for one whose row this module just closed — see the header
/// for why the second hop is task 009's to take. Both land on `Failed`.
/// `Queued` is the narrower crash: caught between `scheduler::claim`'s two
/// separately committed transitions, before the second — `queued -> running`
/// — ever ran, so there is no open run for [`open_runs`] to have found above.
/// ADR-0007's machine has no `Queued -> Failed` edge, and adding one is a
/// bigger change than this repair needs; `Queued -> Cancelled` already exists
/// for exactly this shape of task, "waiting for its turn with no live process
/// to kill" (the same edge cancel-one takes on it). Anything else is a task
/// something already settled while this was running, and is left alone.
async fn settle(ctx: &ServiceContext, task_id: &str) -> Result<()> {
    let run_state = sqlx::query_scalar!(
        r#"SELECT run_state AS "run_state: RunState" FROM tasks WHERE id = ?1"#,
        task_id,
    )
    .fetch_optional(&ctx.pool)
    .await?;

    match run_state {
        Some(RunState::Running | RunState::WaitingRetry) => {
            set_run_state(ctx, task_id, RunState::Failed).await?;
        }
        Some(RunState::Queued) => {
            set_run_state(ctx, task_id, RunState::Cancelled).await?;
        }
        _ => {}
    }

    Ok(())
}
