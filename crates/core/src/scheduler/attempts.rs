//! How much of a task's retry budget is already spent, read off its `runs`
//! rows.
//!
//! # The attempt count is derived, never stored
//!
//! There is no attempt-count column and there must not be one. Seam-contract D4
//! forbids a migration anyway, but the deeper reason is that a counter is a
//! second source of truth for something the rows already answer exactly: every
//! attempt is a `runs` row, `runs.attempt` is per-task and monotonic
//! (`idx_runs_task_attempt` is UNIQUE and `start_run` computes `max + 1` inside
//! its transaction), and a counter maintained beside them would eventually
//! disagree with them.
//!
//! # `session_id` is the boundary of a budget, not the task
//!
//! ADR-0011 has "each attempt is a row sharing the task's session id", and this
//! is what that sentence *means* operationally: counting stops at the first row
//! whose session differs. So a task the user re-queued in the morning starts a
//! new session and gets a fresh five transient attempts, while last night's
//! four are still on the board as history. That is the correct reading of both
//! ADR-0011 and ADR-0007's "failed tasks accumulate in `ready` unless the user
//! acts" — the user acting *is* the thing that grants a new budget.
//!
//! # Why the ending attempt is a parameter
//!
//! [`history`] is called at the one moment the newest row cannot answer for
//! itself: after `execute` has returned and *before* `finish_run` closes the
//! row, because what `finish_run` writes — `resume_after` — is the thing this
//! history is being read to decide. Two of ADR-0011's inputs are only in the
//! outcome at that point: `exit_class`, which is still NULL on the row, and the
//! reported reset time, which has no column at all
//! (`RunOutcome::usage_limit_resets_at` is what the CLI *said*, and only
//! `resume_after` — what the policy decided — is persisted). Every other field
//! comes off the rows.

use chrono::{DateTime, Utc};

use crate::context::ServiceContext;
use crate::db::ExitClass;
use crate::error::Result;
use crate::scheduler::retry::AttemptHistory;

/// The facts about the attempt that just ended which its own row cannot yet
/// supply. See this module's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ending {
    pub exit_class: ExitClass,
    /// What the `rate_limit_event` reported, unjittered — `RunOutcome::usage_limit_resets_at`.
    pub usage_limit_resets_at: Option<DateTime<Utc>>,
}

/// One `runs` row, as far as the budget is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AttemptRow {
    session_id: String,
    /// NULL for a row that has not been closed out — the attempt that is
    /// ending, and any a crash left open.
    exit_class: Option<ExitClass>,
}

/// What `task_id`'s current session has spent, with `ending` as its latest
/// attempt.
///
/// `None` for a task that has never been run, which is not a condition any
/// caller has to handle specially: there is nothing to resume and nothing to
/// give up on.
pub async fn history(
    ctx: &ServiceContext,
    task_id: &str,
    ending: Ending,
) -> Result<Option<AttemptHistory>> {
    let rows = attempt_rows(ctx, task_id).await?;
    Ok(fold(&rows, ending))
}

/// The session a resume of `task_id` would continue, or `None` when there is
/// nothing to continue.
///
/// The newest attempt's session, which is the same one [`history`] counts
/// backwards from — read separately because the queue needs it at claim time,
/// after the decision has already been made and stored.
pub async fn resumable_session(ctx: &ServiceContext, task_id: &str) -> Result<Option<String>> {
    Ok(attempt_rows(ctx, task_id)
        .await?
        .into_iter()
        .next()
        .map(|row| row.session_id))
}

/// Newest attempt first. The order is what makes "count backwards while the
/// session matches" a single pass.
async fn attempt_rows(ctx: &ServiceContext, task_id: &str) -> Result<Vec<AttemptRow>> {
    let rows = sqlx::query!(
        r#"SELECT session_id AS "session_id!: String",
                  exit_class AS "exit_class: ExitClass"
             FROM runs
            WHERE task_id = ?1
            ORDER BY attempt DESC"#,
        task_id,
    )
    .fetch_all(&ctx.pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| AttemptRow {
            session_id: row.session_id,
            exit_class: row.exit_class,
        })
        .collect())
}

/// The pure half, so the counting rule is testable without a pool.
///
/// `rows` is newest first. The first row is the attempt that is ending, and its
/// class comes from `ending` rather than from the row — see this module's
/// header.
fn fold(rows: &[AttemptRow], ending: Ending) -> Option<AttemptHistory> {
    let (newest, older) = rows.split_first()?;
    let session_id = newest.session_id.clone();

    let mut history = AttemptHistory {
        exit_class: ending.exit_class,
        session_id: session_id.clone(),
        attempts_in_session: 1,
        transient_attempts: u32::from(ending.exit_class == ExitClass::Transient),
        interrupted_attempts: u32::from(ending.exit_class == ExitClass::Interrupted),
        usage_limit_resets_at: ending.usage_limit_resets_at,
    };

    for row in older {
        // The boundary, and the whole of it: the first row from another session
        // ends the budget, and everything before it is a previous night's
        // history rather than this night's spend.
        if row.session_id != session_id {
            break;
        }

        history.attempts_in_session += 1;
        match row.exit_class {
            Some(ExitClass::Transient) => history.transient_attempts += 1,
            Some(ExitClass::Interrupted) => history.interrupted_attempts += 1,
            // A row still open — a crash caught it, and the reconciler has not
            // reached it yet — counts as an attempt and spends no budget. It
            // will be closed as `interrupted` and counted then; counting it
            // twice would shorten the budget by the length of the crash.
            _ => {}
        }
    }

    Some(history)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const SESSION: &str = "0b6d3e2e-0000-4000-8000-00000000c0de";
    const OTHER_SESSION: &str = "0b6d3e2e-0000-4000-8000-00000000feed";

    fn row(session_id: &str, exit_class: Option<ExitClass>) -> AttemptRow {
        AttemptRow {
            session_id: session_id.to_string(),
            exit_class,
        }
    }

    fn ending(exit_class: ExitClass) -> Ending {
        Ending {
            exit_class,
            usage_limit_resets_at: None,
        }
    }

    #[test]
    fn a_task_that_has_never_run_has_no_history_to_read() {
        assert_eq!(fold(&[], ending(ExitClass::Transient)), None);
    }

    #[test]
    fn the_attempt_count_comes_from_the_session_and_not_from_the_task() {
        // The rule this module exists for. Five rows, two sessions: last
        // night's three are history, and only tonight's two are spend — so a
        // task the user re-queued in the morning is not one attempt away from
        // the cap.
        let rows = [
            row(SESSION, None),
            row(SESSION, Some(ExitClass::Transient)),
            row(OTHER_SESSION, Some(ExitClass::Transient)),
            row(OTHER_SESSION, Some(ExitClass::Transient)),
            row(OTHER_SESSION, Some(ExitClass::Transient)),
        ];

        let history = fold(&rows, ending(ExitClass::Transient)).expect("a task with runs");

        assert_eq!(history.session_id, SESSION);
        assert_eq!(history.attempts_in_session, 2);
        assert_eq!(
            history.transient_attempts, 2,
            "the previous session's three failures are history, not budget",
        );
    }

    #[test]
    fn the_ending_attempt_is_counted_from_the_outcome_rather_than_from_its_own_row() {
        // Its row is still open — closing it is what this history is being read
        // to decide — so a fold that trusted the column would count zero.
        let history = fold(&[row(SESSION, None)], ending(ExitClass::Transient))
            .expect("a task with one run");

        assert_eq!(history.exit_class, ExitClass::Transient);
        assert_eq!(history.transient_attempts, 1);
        assert_eq!(history.attempts_in_session, 1);
    }

    #[test]
    fn each_class_is_counted_against_its_own_budget_and_nothing_else_is() {
        let rows = [
            row(SESSION, None),
            row(SESSION, Some(ExitClass::UsageLimit)),
            row(SESSION, Some(ExitClass::Interrupted)),
            row(SESSION, Some(ExitClass::Transient)),
            row(SESSION, Some(ExitClass::UsageLimit)),
        ];

        let history = fold(&rows, ending(ExitClass::Interrupted)).expect("a task with runs");

        assert_eq!(history.attempts_in_session, 5);
        assert_eq!(history.transient_attempts, 1);
        assert_eq!(history.interrupted_attempts, 2);
    }

    #[test]
    fn a_reported_reset_time_rides_in_on_the_ending_rather_than_off_a_column() {
        // There is no column for it: `runs.resume_after` holds what the policy
        // decided, and the reset the CLI reported reaches this only through the
        // outcome (`runner::outcome`'s own note on the split).
        let reset = "2026-08-20T06:00:00Z".parse::<DateTime<Utc>>().expect("a literal timestamp");
        let history = fold(
            &[row(SESSION, None)],
            Ending {
                exit_class: ExitClass::UsageLimit,
                usage_limit_resets_at: Some(reset),
            },
        )
        .expect("a task with one run");

        assert_eq!(history.usage_limit_resets_at, Some(reset));
    }
}
