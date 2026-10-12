//! `run_state`: the machine's half of ADR-0007's two dimensions, and the one
//! path that writes it.
//!
//! [`set_run_state`], and [`transition`], the conditional write beneath it
//! that a lease transaction takes its edges through, are that path. Nothing
//! else in this crate issues an `UPDATE tasks SET run_state = ...` — `startup::survey`'s own module docs
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

use sqlx::SqliteConnection;

use crate::clock::Clock;
use crate::context::{ScopedTx, ServiceContext};
use crate::db::{RunState, Task};
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::tasks::service::{fetch_task_row, task_row, team_of_task};

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
/// state that was only true for one of them. The write itself is
/// [`transition`] from the value it read, so this and a lease transaction
/// share one conditional `UPDATE`.
///
/// `BEGIN IMMEDIATE` rather than a plain (deferred) `BEGIN`: on the
/// production pool — more than one connection, unlike the single-connection
/// test pool, where this never showed up — two callers racing this function
/// would otherwise both open a deferred read here, unlocked, and the loser's
/// later `UPDATE` would fail to upgrade with `SQLITE_BUSY_SNAPSHOT`, a
/// conflict SQLite's busy handler does not retry. That left `busy_timeout`
/// never applying and the loser reading a raw "database is locked" instead of
/// the transition refusal a claimer exists to recognise — measured at 598 of
/// 600 losers on a ten-connection pool. Taking the write lock up front makes
/// a second caller wait for it, which *is* covered by `busy_timeout`, so it
/// always reaches its own read seeing whatever the first writer already
/// committed rather than racing it.
pub async fn set_run_state(ctx: &ServiceContext, id: &str, to: RunState) -> Result<Task> {
    let mut tx = ctx.begin_immediate().await?;
    set_within(&mut tx, ctx.clock.as_ref(), id, to).await?;
    let team_id = team_of_task(&mut tx, id).await?;
    tx.commit().await?;

    // Publish before the read-back: the row is already committed, so a
    // failure in `fetch_task_row` below must not cost the notification for a
    // mutation that already happened (ADR-0018).
    ctx.publish(ChangeEvent::tasks(team_id, [id.to_string()]));
    let updated = task_row(ctx, id).await?;
    Ok(updated)
}

/// [`set_run_state`]'s read and write, inside a transaction the caller holds,
/// commits and publishes for.
///
/// For a write that has to land with others or not at all: a finished run
/// lands its task, deletes its lease and pins it in one transaction
/// (`board::lease`). The refusal is `set_run_state`'s, word for word.
pub(crate) async fn set_within(
    tx: &mut ScopedTx,
    clock: &dyn Clock,
    id: &str,
    to: RunState,
) -> Result<()> {
    let current = fetch_task_row(tx, id).await?;

    if !is_legal_run_state_transition(current.run_state, to) {
        return Err(Error::invalid(format!(
            "cannot move task {id} from run state \"{from}\" to \"{to}\": not a legal transition",
            from = run_state_spelling(current.run_state),
            to = run_state_spelling(to),
        )));
    }

    transition(tx, clock, id, current.run_state, to).await?;
    Ok(())
}

/// Moves a task from `from` to `to`, **only if it is still in `from`**, and
/// answers whether a row moved.
///
/// The conditional write `scheduler/claim.rs` asked for twice: the expected
/// state is in the `WHERE`, so a caller that read the row earlier in its own
/// transaction, or chose the edge from a board read, cannot move a task that
/// has since left that state. `false` is that race lost, or a task that does
/// not exist; nothing was written either way.
///
/// It runs inside the caller's transaction and never commits or publishes:
/// the caller does both, after its own commit, with whatever else the
/// transaction wrote. An edge ADR-0007's machine does not have is an error and
/// writes nothing, so a caller cannot use this to skip the table.
///
/// This file stays the only one that writes `run_state` (ADR-0006).
pub async fn transition(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    id: &str,
    from: RunState,
    to: RunState,
) -> Result<bool> {
    if !is_legal_run_state_transition(from, to) {
        return Err(Error::invalid(format!(
            "cannot move task {id} from run state \"{from}\" to \"{to}\": not a legal transition",
            from = run_state_spelling(from),
            to = run_state_spelling(to),
        )));
    }

    let now = clock.now();
    let moved = sqlx::query!(
        "UPDATE tasks SET run_state = ?1, updated_at = ?2 WHERE id = ?3 AND run_state = ?4",
        to,
        now,
        id,
        from,
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok(moved == 1)
}

/// The schema's own spelling for one `run_state` value — for an error
/// message a user reads, which should say what the board says
/// (`waiting_retry`), not what Rust's `Debug` says (`WaitingRetry`).
///
/// Exported because `scheduler::claim::give_up` refuses a task that is not
/// waiting and has to name the state it found. One spelling for both messages,
/// which is the whole reason this is a function and not a `format!("{:?}")`.
pub fn run_state_spelling(state: RunState) -> &'static str {
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
    use crate::testing::TestContext;
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

    // -----------------------------------------------------------------------
    // `transition`, the conditional write, against a real database
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn a_transition_from_the_state_the_task_is_in_moves_it_and_answers_true() {
        let h = TestContext::new().await;
        let id = seed_task(&h).await;
        h.clock.advance(chrono::Duration::seconds(30));

        let mut conn = h.context.pool.acquire().await.expect("a connection");
        let moved = transition(&mut conn, &h.clock, &id, RunState::Idle, RunState::Queued)
            .await
            .expect("idle -> queued is legal");
        drop(conn);

        assert!(moved);
        assert_eq!(
            row(&h, &id).await,
            (RunState::Queued, h.clock.now()),
            "moved, and stamped now",
        );
    }

    #[tokio::test]
    async fn a_transition_from_a_state_the_task_has_left_moves_nothing_and_answers_false() {
        let h = TestContext::new().await;
        let id = seed_task(&h).await;
        for state in [RunState::Queued, RunState::Running, RunState::Failed] {
            set_run_state(&h.context, &id, state)
                .await
                .unwrap_or_else(|error| panic!("walk to {state:?}: {error}"));
        }
        let before = row(&h, &id).await;
        h.clock.advance(chrono::Duration::seconds(30));

        // Idle -> Queued is a legal edge; the task is simply no longer idle.
        let mut conn = h.context.pool.acquire().await.expect("a connection");
        let moved = transition(&mut conn, &h.clock, &id, RunState::Idle, RunState::Queued)
            .await
            .expect("a lost race is an answer, not an error");
        drop(conn);

        assert!(!moved);
        assert_eq!(row(&h, &id).await, before, "nothing was written");
        assert_eq!(before.0, RunState::Failed);
    }

    #[tokio::test]
    async fn a_transition_naming_a_task_that_does_not_exist_answers_false() {
        let h = TestContext::new().await;

        let mut conn = h.context.pool.acquire().await.expect("a connection");
        let moved = transition(
            &mut conn,
            &h.clock,
            "no-such-task",
            RunState::Idle,
            RunState::Queued,
        )
        .await
        .expect("a missing row is an answer, not an error");

        assert!(!moved);
    }

    #[tokio::test]
    async fn an_illegal_transition_is_refused_before_any_write() {
        // The task *is* idle, so the conditional `UPDATE` would match it: only
        // the table check standing in front of it keeps it from writing.
        let h = TestContext::new().await;
        let id = seed_task(&h).await;
        let before = row(&h, &id).await;
        h.clock.advance(chrono::Duration::seconds(30));

        let mut conn = h.context.pool.acquire().await.expect("a connection");
        let error = transition(&mut conn, &h.clock, &id, RunState::Idle, RunState::Running)
            .await
            .expect_err("idle -> running skips queued");
        drop(conn);

        assert_eq!(error.code(), crate::ErrorCode::Invalid);
        assert_eq!(
            error.to_string(),
            format!(
                "cannot move task {id} from run state \"idle\" to \"running\": not a legal transition"
            ),
        );
        assert_eq!(
            row(&h, &id).await,
            before,
            "run_state and updated_at unchanged"
        );
        assert_eq!(before.0, RunState::Idle);
    }

    /// An idle task in a repository seeded directly: this module's subject is
    /// `run_state`, and `repo::register` would drag a real checkout into it.
    async fn seed_task(h: &TestContext) -> String {
        let repository_id = crate::db::new_id();
        sqlx::query(
            "INSERT INTO repositories (id, team_id, name, default_branch, created_at)
             VALUES (?1, ?2, 'rimaia', 'main', ?3)",
        )
        .bind(&repository_id)
        .bind(h.context.scope.sole().expect("the harness's solo team"))
        .bind(h.clock.now())
        .execute(&h.context.pool)
        .await
        .expect("seed a repository");

        let task = crate::tasks::create_task(
            &h.context,
            crate::tasks::NewTask {
                repository_id,
                title: "Move me".to_string(),
                plan: None,
                extra_instructions: None,
                column: None,
                links: vec![],
            },
        )
        .await
        .expect("create a task");
        assert_eq!(task.run_state, RunState::Idle);
        task.id
    }

    /// The two columns a transition writes, read straight off the row.
    async fn row(h: &TestContext, id: &str) -> (RunState, chrono::DateTime<chrono::Utc>) {
        let task = crate::tasks::get_task(&h.context, id)
            .await
            .expect("read the task")
            .task;
        (task.run_state, task.updated_at)
    }
}
