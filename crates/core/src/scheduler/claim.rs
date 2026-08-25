//! Who owns a task, decided by a conditional write (ADR-0010).
//!
//! # Why this is not one `UPDATE ... WHERE run_state = 'idle'`
//!
//! ADR-0010 requires selection and the transition to `running` to happen in one
//! transaction "so the UI, the MCP server, and the scheduler cannot
//! double-claim a task", and the mechanism for that is already built:
//! [`set_run_state`] reads the current state and writes the new one **inside
//! one transaction**, refusing anything the ADR-0007 machine does not allow. It
//! is a conditional write — the row moves only if it is still in the state the
//! caller believed it was in — and the loser gets an `Err` rather than a silent
//! no-op.
//!
//! `crates/core/src/tasks/run_state.rs` says so in the transition table itself,
//! on the edge that matters: "`Queued -> Running`: the scheduler claims the
//! task and starts a process, **in the one transaction ADR-0010 requires**...
//! `Idle -> Running` directly is task 004's own illegal example, precisely
//! because it would skip that transaction and the selection it protects." So
//! the claim is two edges, and the *decisive* one is the second: `Queued` has
//! exactly one door out into `Running`, so whoever walks through it owns the
//! process. A caller that did not walk through it must not spawn anything, and
//! [`claim`] is what tells it which it was.
//!
//! [`selection::plan`](super::selection::plan) is a *ranking*, not the
//! selection ADR-0010 is talking about; it is deliberately outside the
//! transaction because "between tasks, re-read the board" requires it to be a
//! fresh query every pass. The selection the transaction protects — "is this
//! task still claimable?" — is the state check inside `set_run_state`.
//!
//! # The scheduler claims all the way to `running`, on purpose
//!
//! `runner::process::claim` already has the arm for it: "`running` already is
//! not an error. That is task 009's arm... when the scheduler exists it claims
//! the task itself and hands this a task already claimed." Stopping at `queued`
//! instead would leave a window where a manual "Run now" could take
//! `Queued -> Running` out from under a queue that had already committed to the
//! task, and a task stranded at `queued` if the run failed to start — a state
//! the queue's own selection then skips forever.

use crate::context::ServiceContext;
use crate::db::RunState;
use crate::error::{Error, ErrorCode, Result};
use crate::tasks::set_run_state;

/// What trying to claim a task came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClaimOutcome {
    /// This caller owns the task: its `run_state` is `running` and nothing else
    /// can reach that state until this run ends. Spawn a process.
    Claimed,
    /// Somebody else got there first, or the task is gone. Nothing was written.
    /// **Do not spawn a process.**
    Lost,
}

/// Takes a task from `idle` to `running`, or reports that somebody else did.
///
/// Two `set_run_state` calls, because the machine has no `Idle -> Running`
/// edge and inventing one would skip the very transaction it exists to force —
/// see this module's header. Each is its own conditional write, so every
/// interleaving of two concurrent claimers ends with exactly one of them
/// holding a `running` task:
///
/// | Interleaving | Winner | Loser sees |
/// | --- | --- | --- |
/// | A claims both edges, then B starts | A | `Running -> Queued` refused |
/// | A takes `Idle -> Queued`, then B starts | A | `Queued -> Queued` refused |
/// | Both read `queued`, both try `Queued -> Running` | whoever commits first | `Running -> Running` refused |
///
/// A refusal is not an error the caller has to handle: a queue that lost a race
/// simply looks at the board again. A *database* failure still propagates,
/// because that is a condition nothing here can carry on through — and one on
/// the *second* edge leaves the task in `queued`, which is the one state
/// nothing here can move it out of (ADR-0007 has no `Queued -> Failed` edge,
/// and `Queued -> Cancelled` would claim the user called it off). Named rather
/// than papered over: it needs the store to fail between two statements on one
/// connection, and the honest fix is the same `set_run_state` change the
/// window below names.
///
/// # What it will and will not take
///
/// The route is fixed rather than derived from the row, which makes the set of
/// startable states exactly the set with a legal edge into `queued`: `idle`,
/// and — because ADR-0007's own note on those edges says trying again
/// "re-enters at Queued like every other start" — `failed` and `cancelled`.
/// `queued`, `running` and `waiting_retry` are all [`Lost`](ClaimOutcome::Lost),
/// which is the property the table above depends on.
///
/// So this is a claim, not a policy: it will happily take a task that failed
/// last night, and deciding *not* to is [`selection`](super::selection)'s job,
/// which the queue does before it gets here. That leaves one window worth
/// naming — a task the queue selected as `idle` that reaches `failed` in the
/// moment between the board read and this call would be claimed anyway. It
/// costs one extra attempt on a card that was going to need attention, it
/// cannot repeat (the second ending leaves it `failed` again, which selection
/// then skips), and closing it properly needs a `set_run_state` that takes an
/// expected current state — which is a change to task 004's module, not
/// something to work around here.
pub async fn claim(ctx: &ServiceContext, task_id: &str) -> Result<ClaimOutcome> {
    for state in [RunState::Queued, RunState::Running] {
        match set_run_state(ctx, task_id, state).await {
            Ok(_) => {}
            Err(error) if lost_the_race(&error) => {
                tracing::debug!(
                    %task_id, %error,
                    "the claim was lost; another starter reached this task first",
                );
                return Ok(ClaimOutcome::Lost);
            }
            Err(error) => return Err(error),
        }
    }

    Ok(ClaimOutcome::Claimed)
}

/// Lands a claimed task that never became a finished run.
///
/// The backstop for the one gap `runner::process::run_task` leaves a *caller*
/// that pre-claimed: its own `release` only covers a failure after the `runs`
/// row exists, so a refusal before that — a vanished worktree, a `claude` that
/// disappeared between the probe and the spawn — would otherwise leave a task
/// reading "running" with no process, which is a badge that lies and a card the
/// queue then skips forever.
///
/// Only ever moves a task that is **still** `running`, so a run that already
/// classified itself keeps its own verdict: ADR-0011 puts a `transient` ending
/// in `waiting_retry`, and overwriting that with `failed` here would throw away
/// the retry task 014 exists to schedule.
///
/// Best effort and infallible by design, like the release it backs up: the
/// caller is already reporting the failure that got here, and "and also the
/// release failed" would bury it. Startup reconciliation is the next backstop.
pub async fn release(ctx: &ServiceContext, task_id: &str) {
    match current_run_state(ctx, task_id).await {
        Ok(Some(RunState::Running)) => {
            if let Err(error) = set_run_state(ctx, task_id, RunState::Failed).await {
                tracing::error!(
                    %task_id, %error,
                    "could not release a task whose queued run never finished",
                );
            }
        }
        Ok(_) => {}
        Err(error) => tracing::error!(
            %task_id, %error,
            "could not read back a task whose queued run never finished",
        ),
    }
}

/// A read, not a second writer: the scheduler never issues an `UPDATE tasks`
/// (see this module's parent). `None` for a task that no longer exists.
async fn current_run_state(ctx: &ServiceContext, task_id: &str) -> Result<Option<RunState>> {
    let run_state = sqlx::query_scalar!(
        r#"SELECT run_state AS "run_state: RunState" FROM tasks WHERE id = ?1"#,
        task_id,
    )
    .fetch_optional(&ctx.pool)
    .await?;
    Ok(run_state)
}

/// Whether an error from [`set_run_state`] means "somebody else has this task"
/// rather than "the store is broken".
///
/// [`ErrorCode::Invalid`] is the only thing that function raises for an illegal
/// transition, and [`ErrorCode::NotFound`] means the row went away — a task
/// deleted while the queue was looking at it, which is equally not this
/// caller's to run. Everything else propagates, so a full disk never reads as a
/// lost race (seam-contract D8: the code stays coarse, and this is exactly the
/// coarse distinction it is for).
fn lost_the_race(error: &Error) -> bool {
    matches!(error.code(), ErrorCode::Invalid | ErrorCode::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_transition_and_a_deleted_task_are_both_lost_races() {
        assert!(lost_the_race(&Error::invalid("not a legal transition")));
        assert!(lost_the_race(&Error::not_found("no task with id x")));
    }

    #[test]
    fn a_store_failure_is_never_read_as_a_lost_race() {
        // The distinction that keeps a full disk from looking like contention:
        // a queue that treated this as "somebody else has it" would move on and
        // never report the thing that is actually wrong.
        assert!(!lost_the_race(&Error::internal("the disk is full")));
        assert!(!lost_the_race(&Error::from(sqlx::Error::RowNotFound)));
    }
}
