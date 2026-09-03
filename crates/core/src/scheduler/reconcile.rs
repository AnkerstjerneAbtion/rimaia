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
//! # What "interrupted" is (seam-contract D9, and its 2026-09-03 amendment)
//!
//! `run_state` keeps exactly ADR-0007's seven values and `interrupted` is not
//! one of them — SQLite cannot widen a `CHECK`, so that is permanent rather
//! than provisional. A run that died with the app is recorded on its `runs` row
//! as `status = 'interrupted'` and `exit_class = 'interrupted'`, and the card
//! reads the word off its last run. Task 009's acceptance criterion —
//! "reopening shows accurate state: one `interrupted` task" — is a statement
//! about what the user sees, and that is still what makes it true.
//!
//! # Where the *task* lands changed with task 014
//!
//! D9 as first written said the task "lands in `run_state = 'failed'`", and
//! this module used to take a second hop, `WaitingRetry -> Failed`, to force
//! it. Its own comment said why: "nothing in the MVP resumes a `waiting_retry`
//! task (that is task 014), so it would sit invisible rather than interrupting
//! the morning review".
//!
//! Now something does. ADR-0010:57-59 and ADR-0011's startup reconciliation
//! both ask for a crashed run to be **offered** for resume, and this is where
//! that happens: [`interrupted_after`] carries a `resume_after` when ADR-0011's
//! budget allows one, so the task lands `waiting_retry` with a due deadline,
//! and lands `failed` exactly when it does not. [`settle`] keeps the second hop
//! only for the second case.
//!
//! **Offered, not performed.** Nothing starts: seam-contract D15 has the exit
//! path write `paused`, `QueueState`'s default *is* `Paused` and `from_stored`
//! falls back to it, so a task sitting due at 03:00 waits for a human to press
//! Start. Three independent things guarantee that, which is why
//! `a_launch_offers_a_crashed_run_for_resume_and_starts_nothing_until_the_queue_is_started`
//! asserts all three rather than the outcome alone.
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

use chrono::{DateTime, Utc};

use crate::context::ServiceContext;
use crate::db::{ExitClass, RunState, RunStatus};
use crate::error::Result;
use crate::runner::events::TokenUsage;
use crate::runner::outcome::{finish_run, RunOutcome, SpawnedAs};
use crate::scheduler::attempts::{self, Ending};
use crate::scheduler::retry;
use crate::startup::ReconciliationReport;
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
        let outcome = interrupted_after(ctx, task_id, &run_id).await;
        if let Err(error) = finish_run(ctx, &run_id, &outcome).await {
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
    let resume_after: Option<Option<DateTime<Utc>>> = sqlx::query_scalar!(
        r#"SELECT resume_after AS "resume_after: DateTime<Utc>"
             FROM runs WHERE task_id = ?1 ORDER BY attempt DESC LIMIT 1"#,
        task_id,
    )
    .fetch_optional(&ctx.pool)
    .await?;

    Ok(resume_after.flatten().is_some())
}
