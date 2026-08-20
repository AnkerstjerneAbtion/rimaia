//! `run_state`: the machine's half of ADR-0007's two dimensions, and the one
//! path that writes it.
//!
//! [`set_run_state`] is that path. Nothing else in this crate issues an
//! `UPDATE tasks SET run_state = ...` — `startup::survey`'s own module docs
//! say so explicitly, naming this function as the one allowed to make a
//! transition, because a second writer of `run_state` is the exact bug
//! ADR-0006 names: the same invariant enforced in two places eventually
//! enforces two different invariants.
//!
//! # The transition table
//!
//! Every legal edge is listed once in [`is_legal_run_state_transition`],
//! each with the ADR text that grounds it. A pair not listed is illegal,
//! including a state naming itself: nothing in the product ever *decides*
//! to stay put, so there is no event that should map onto a self-transition,
//! and allowing one silently would hide a caller bug (setting a task to the
//! state it is already in) behind what looks like success.
//!
//! Two edges are this crate's own judgment call rather than a direct
//! reading of an ADR, because dependency semantics (task 011) and the
//! scheduler (task 009/010) do not exist yet to have exercised them:
//!
//! - `Idle -> Blocked` is deliberately **not** legal. A task only becomes
//!   blocked by way of the scheduler evaluating it as a queue candidate
//!   (`Queued -> Blocked`), never by skipping the queue.
//! - `WaitingRetry -> Failed` (retries exhausted) has no ADR-given attempt
//!   count for `usage_limit` specifically — ADR-0011 caps `transient` at
//!   five attempts and says a usage-limit wait is "capped only by the run
//!   window" without naming what happens at that boundary. The edge is
//!   legal either way; *when* the scheduler decides to take it is task
//!   009/014's policy, not this table's.
//!
//! If task 009, 010 or 011 finds either judgment wrong, this table is the
//! place to change it — not a second switch statement somewhere that
//! disagrees with it.

use crate::context::ServiceContext;
use crate::db::{RunState, Task};
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::tasks::service::fetch_task_row;

/// Whether ADR-0007's run-state machine allows moving from `from` to `to`.
pub fn is_legal_run_state_transition(from: RunState, to: RunState) -> bool {
    use RunState::*;

    matches!(
        (from, to),
        // Idle -> Queued: a `ready` task enters the run queue. ADR-0007:
        // "Only `ready` feeds the run queue." This is the *only* door into
        // Queued — there is no `X -> Queued` edge that skips Idle, which is
        // what keeps every run traceable back to a task that was actually
        // idle beforehand.
        (Idle, Queued)
        // Queued -> Running: the scheduler claims the task and starts a
        // process, in the one transaction ADR-0010 requires ("Selection and
        // the transition to running happen in a single database
        // transaction"). This is the ONLY door into Running other than
        // resuming a wait below — `Idle -> Running` directly is task 004's
        // own illegal example, precisely because it would skip that
        // transaction and the selection it protects.
        | (Queued, Running)
        // Queued -> Blocked: the scheduler re-evaluates the same candidate
        // and finds an unsatisfied dependency (ADR-0010's selection filter
        // — "not blocked by an unsatisfied dependency"; ADR-0008's
        // blocking).
        | (Queued, Blocked)
        // Blocked -> Queued: the blocking dependency's own run succeeded.
        // ADR-0008: "a dependency is satisfied when the dependency's run
        // completes successfully" — monotonic, so the only way out of
        // Blocked going forward is back into contention for selection.
        | (Blocked, Queued)
        // Running -> Idle: the run's `result` classified `success`.
        // ADR-0011's table: "Task -> in_review". That is a `column` move,
        // not a `run_state`; nothing is left for `run_state` to track once a
        // run finished cleanly, so it returns to the value a task that has
        // never run also holds.
        | (Running, Idle)
        // Running -> WaitingRetry: the run classified `usage_limit` or
        // `transient` (ADR-0011: wait for the reset, or back off, then
        // resume — "every retry is `claude -p --resume <session-id>`").
        | (Running, WaitingRetry)
        // Running -> Failed: the run classified `fatal` (ADR-0011: "no
        // retry... run_state = failed"), OR the user cancelled an in-flight
        // run. ADR-0010's Control section is explicit that cancel-one on a
        // running task "goes to `failed` with `cancelled` reason" — the
        // `Cancelled` *run_state* is reserved for a task that has no live
        // process to kill (see the Queued/Blocked/WaitingRetry edges below).
        | (Running, Failed)
        // WaitingRetry -> Running: the wait elapsed — the usage-limit reset
        // plus jitter, or the next backoff step — and the attempt resumes
        // (ADR-0011).
        | (WaitingRetry, Running)
        // WaitingRetry -> Failed: retries exhausted. ADR-0011 caps
        // `transient` backoff at five attempts; a `usage_limit` wait is
        // capped only by the run window, but a task still stuck past that
        // window still has to land somewhere terminal, and this is the
        // table's answer for "somewhere" — see this module's doc for why the
        // exact trigger is left to task 009/014's policy.
        | (WaitingRetry, Failed)
        // WaitingRetry -> Cancelled, Queued -> Cancelled, Blocked ->
        // Cancelled: cancel-one reaching a task that has not started running
        // yet — waiting between attempts, waiting for its turn, or waiting
        // on a dependency. No process is alive to SIGTERM, so unlike the
        // `Running -> Failed` edge there is no run to mark `failed` on the
        // task's behalf; the task itself is the thing being called off
        // (ADR-0010 Control: cancel-one, cancel-all).
        | (WaitingRetry, Cancelled)
        | (Queued, Cancelled)
        | (Blocked, Cancelled)
        // Failed -> Queued, Cancelled -> Queued: the user requeues a task
        // that stopped short of success. Task 004's own rule for `done` —
        // "the user is in charge of their own board" — applies the same way
        // here: nothing forbids trying again, and trying again re-enters at
        // Queued like every other start, never around it.
        | (Failed, Queued)
        | (Cancelled, Queued)
    )
}

/// Writes `to` into a task's `run_state`, after checking
/// [`is_legal_run_state_transition`] against its current value. Illegal
/// transitions — including a state naming itself — are refused with a
/// message naming both states, and change nothing.
///
/// Runs inside one transaction so the read of the current state and the
/// write of the new one cannot interleave with a concurrent caller: two
/// writers racing to transition the same task must not both succeed from a
/// state that was only true for one of them.
pub async fn set_run_state(ctx: &ServiceContext, id: &str, to: RunState) -> Result<Task> {
    let mut tx = ctx.pool.begin().await?;
    let current = fetch_task_row(&mut *tx, id).await?;

    if !is_legal_run_state_transition(current.run_state, to) {
        return Err(Error::invalid(format!(
            "cannot move task {id} from run state \"{from}\" to \"{to}\": not a legal transition",
            from = wire_spelling(current.run_state),
            to = wire_spelling(to),
        )));
    }

    let now = ctx.clock.now();
    sqlx::query!(
        "UPDATE tasks SET run_state = ?1, updated_at = ?2 WHERE id = ?3",
        to,
        now,
        id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // Publish before the read-back: the row is already committed, so a
    // failure in `fetch_task_row` below must not cost the notification for a
    // mutation that already happened (ADR-0018).
    ctx.publish(ChangeEvent::tasks([id.to_string()]));
    let updated = fetch_task_row(&ctx.pool, id).await?;
    Ok(updated)
}

/// The schema's own spelling for one `run_state` value — for an error
/// message a user reads, which should say what the board says
/// (`waiting_retry`), not what Rust's `Debug` says (`WaitingRetry`).
fn wire_spelling(state: RunState) -> &'static str {
    match state {
        RunState::Idle => "idle",
        RunState::Queued => "queued",
        RunState::Running => "running",
        RunState::Blocked => "blocked",
        RunState::WaitingRetry => "waiting_retry",
        RunState::Failed => "failed",
        RunState::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// ADR-0015 names `set_run_state` — every legal and illegal transition —
    /// on its must-have-tests list. Rather than 49 individually named tests,
    /// one exhaustive comparison against the documented set: every pair not
    /// in `LEGAL` must be illegal, and every pair in it must be legal. A
    /// change to [`is_legal_run_state_transition`] that adds or removes an
    /// edge without updating this list fails here, which is what makes the
    /// table's own doc comments — not this array — the place a reviewer
    /// checks for the *reason* an edge exists.
    const LEGAL: &[(RunState, RunState)] = &[
        (RunState::Idle, RunState::Queued),
        (RunState::Queued, RunState::Running),
        (RunState::Queued, RunState::Blocked),
        (RunState::Blocked, RunState::Queued),
        (RunState::Running, RunState::Idle),
        (RunState::Running, RunState::WaitingRetry),
        (RunState::Running, RunState::Failed),
        (RunState::WaitingRetry, RunState::Running),
        (RunState::WaitingRetry, RunState::Failed),
        (RunState::WaitingRetry, RunState::Cancelled),
        (RunState::Queued, RunState::Cancelled),
        (RunState::Blocked, RunState::Cancelled),
        (RunState::Failed, RunState::Queued),
        (RunState::Cancelled, RunState::Queued),
    ];

    const ALL_STATES: [RunState; 7] = [
        RunState::Idle,
        RunState::Queued,
        RunState::Running,
        RunState::Blocked,
        RunState::WaitingRetry,
        RunState::Failed,
        RunState::Cancelled,
    ];

    #[test]
    fn every_pair_agrees_with_the_documented_legal_set() {
        for from in ALL_STATES {
            for to in ALL_STATES {
                let expected = LEGAL.contains(&(from, to));
                assert_eq!(
                    is_legal_run_state_transition(from, to),
                    expected,
                    "{from:?} -> {to:?} should be {}",
                    if expected { "legal" } else { "illegal" }
                );
            }
        }
    }

    #[test]
    fn idle_to_running_skips_queued_and_is_illegal() {
        // Task 004's own named example.
        assert!(!is_legal_run_state_transition(
            RunState::Idle,
            RunState::Running
        ));
    }

    #[test]
    fn every_state_naming_itself_is_illegal() {
        for state in ALL_STATES {
            assert!(
                !is_legal_run_state_transition(state, state),
                "{state:?} -> {state:?} must not be a transition a caller can no-op through"
            );
        }
    }

    #[test]
    fn cancelling_a_running_task_lands_on_failed_not_cancelled() {
        // ADR-0010's literal words: cancel-one on a running task "goes to
        // `failed` with `cancelled` reason" (that reason lives on the run
        // row's `exit_class`, not on `run_state`).
        assert!(is_legal_run_state_transition(
            RunState::Running,
            RunState::Failed
        ));
        assert!(!is_legal_run_state_transition(
            RunState::Running,
            RunState::Cancelled
        ));
    }
}
