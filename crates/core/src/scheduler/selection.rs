//! What the queue would do next, and why it will pass over what it passes over
//! (ADR-0010's Selection, ADR-0012's opt-in).
//!
//! # Board order is read from the board
//!
//! [`plan`] calls [`tasks::list_tasks`] filtered to
//! [`BoardColumn::Ready`](crate::db::BoardColumn::Ready) rather than issuing an
//! ordering of its own. ADR-0007 says "board order is execution order", and the
//! cheapest way for that to stay true is for the two to be one query: the order
//! here is literally the order the cards are drawn in, tiebreakers included, so
//! a card dragged to the top is picked up next without anybody keeping two
//! `ORDER BY` clauses in agreement. "Highest position" therefore means the
//! *top* of the column — `position` ascends downwards, which is why
//! `outcome::move_to_in_review` looks for the bottom card with `DESC`.
//!
//! With the board's repository filter set to "All repositories" the visible
//! order groups by repository before position, and so does this. That is the
//! same list, not a second policy; ADR-0010's per-repository interleaving is a
//! parallel-mode concern (task 012) and there is nothing to interleave while
//! one run happens at a time.
//!
//! # The one predicate task 011 has to add is already here
//!
//! Task 009's Out of scope asks for selection "written so 011 can add one
//! predicate rather than rewriting it". [`skip_reason`] reads
//! [`TaskSummary::blocked_by_incomplete`], which seam-contract D12 ships as a
//! constant `false` until task 011 turns the `0` literal in `list_tasks`' query
//! into real SQL. So the predicate is live code today that simply never fires,
//! and task 011 adds **no** line here at all — it changes one expression in one
//! query, exactly as D12 promises.
//!
//! # Skipping is never silent
//!
//! A `ready` task the queue will not start stays in the plan, carrying its
//! [`SkipReason`], rather than being filtered out of it. ADR-0012 makes the
//! per-repository opt-in the whole security posture, and a posture the user
//! cannot see is one they cannot fix at 09:00 when nothing ran overnight.

use std::collections::HashSet;

use serde::Serialize;

use crate::context::ServiceContext;
use crate::db::{BoardColumn, RunState};
use crate::error::Result;
use crate::repo;
use crate::tasks::{self, TaskFilter, TaskSummary};

/// Why the queue will not start a `ready` task it can otherwise see.
///
/// A closed set, serialized for the Runs view and the card badge. Every value
/// is a *reason*, not a severity: the UI decides which of them is worth a
/// colour, and only [`UnattendedRunsNotAllowed`](SkipReason::UnattendedRunsNotAllowed)
/// is something the user has to act on before the queue can ever start the
/// task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// ADR-0012's per-repository opt-in is off. Un-opted repositories hold
    /// tasks; those tasks cannot be started.
    UnattendedRunsNotAllowed,
    /// A dependency has not succeeded yet (ADR-0008). Reached from
    /// [`RunState::Blocked`] and from [`TaskSummary::blocked_by_incomplete`] —
    /// two spellings of one condition, and neither is computed until task 011.
    DependencyNotSatisfied,
    /// Something already has this task: it is `queued`, `running`, or waiting
    /// out a retry. The queue never starts a second process for it, and this is
    /// where a manual "Run now" that beat the queue to a card shows up.
    AlreadyInFlight,
    /// The last attempt ended `failed` or `cancelled`. ADR-0007 leaves such a
    /// card in `ready` deliberately — "a failure should interrupt the morning
    /// review, not hide in a column" — and the queue does not retry it on its
    /// own, because that is ADR-0011's `waiting_retry` path and task 014's
    /// policy, not a second automatic attempt at the same wall.
    NeedsAttention,
}

impl SkipReason {
    /// The phrase a card shows next to a task the queue passed over.
    ///
    /// Short, because it is a badge. Deliberately not the sentence
    /// [`repo::ensure_unattended_runs_allowed`] produces: that one answers "why
    /// was this start refused" and names the repository, this one answers "why
    /// is this card not in the queue" on a board where the repository is
    /// already visible. One rule, two audiences — but the rule itself is only
    /// in [`skip_reason`].
    pub const fn explanation(self) -> &'static str {
        match self {
            SkipReason::UnattendedRunsNotAllowed => {
                "this repository has not enabled unattended agent runs"
            }
            SkipReason::DependencyNotSatisfied => "waiting on a dependency",
            SkipReason::AlreadyInFlight => "already started",
            SkipReason::NeedsAttention => "the last run did not succeed",
        }
    }
}

/// One `ready` task, as the queue sees it.
///
/// Ids and a title, never a row: whoever renders this already re-reads the
/// board on `tasks:changed` and would otherwise have two copies of a task to
/// decide between (ADR-0018's argument for ids-only events, applied to a
/// projection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub task_id: String,
    pub title: String,
    pub repository_id: String,
    /// Where in the queue this task sits, counting only what the queue will
    /// actually start: `Some(1)` is next up. `None` for a skipped task, which
    /// has no place in a queue it is not in — task 009's "board cards show
    /// `queued` position" is this number.
    pub queue_position: Option<i64>,
    /// `None` when the queue would start this task right now.
    pub skip: Option<SkipReason>,
}

/// Every `ready` task in board order, with the reason the queue will pass over
/// each one it cannot start.
///
/// Re-read from scratch on every pass of the queue loop — never a snapshot
/// taken when the queue was started. That is what makes "a task dragged to the
/// top mid-queue is picked up next" true rather than aspirational.
pub async fn plan(ctx: &ServiceContext) -> Result<Vec<QueueEntry>> {
    let ready = tasks::list_tasks(
        ctx,
        TaskFilter {
            column: Some(BoardColumn::Ready),
            ..TaskFilter::default()
        },
    )
    .await?;

    // One read of the repository table rather than one per task: a board of
    // fifty cards is served by a handful of repositories, and this runs on
    // every pass of the loop.
    let opted_in: HashSet<String> = repo::list(ctx)
        .await?
        .into_iter()
        .filter(repo::allows_unattended_runs)
        .map(|repository| repository.id)
        .collect();

    let mut plan = Vec::with_capacity(ready.len());
    let mut claimable = 0;
    for summary in &ready {
        let skip = skip_reason(summary, opted_in.contains(&summary.task.repository_id));
        let queue_position = skip.is_none().then(|| {
            claimable += 1;
            claimable
        });

        plan.push(QueueEntry {
            task_id: summary.task.id.clone(),
            title: summary.task.title.clone(),
            repository_id: summary.task.repository_id.clone(),
            queue_position,
            skip,
        });
    }

    Ok(plan)
}

/// The task the queue would claim next, or `None` when there is nothing to do.
pub fn next_to_start(plan: &[QueueEntry]) -> Option<&QueueEntry> {
    plan.iter().find(|entry| entry.skip.is_none())
}

/// Why the queue will not start `task`, or `None` when it will.
///
/// **The whole eligibility rule, in one pure function.** Order matters only for
/// which reason a card shows when more than one applies, and ADR-0012's opt-in
/// comes first because it is the one the user has to act on.
///
/// [`RunState::Idle`] is the only state the queue claims from. ADR-0010's list
/// names `queued`, `running` and `waiting_retry` as ineligible and ADR-0008
/// adds `blocked`; `failed` and `cancelled` are ineligible because ADR-0007
/// says failed tasks "accumulate in `ready` unless the user acts", and a queue
/// that re-selected them would work the top of the board forever instead of
/// moving on.
pub fn skip_reason(task: &TaskSummary, unattended_runs_allowed: bool) -> Option<SkipReason> {
    if !unattended_runs_allowed {
        return Some(SkipReason::UnattendedRunsNotAllowed);
    }
    // Constant `false` until task 011 computes it (seam-contract D12). Live
    // code rather than a comment, so that task adds a query expression and not
    // a predicate here.
    if task.blocked_by_incomplete {
        return Some(SkipReason::DependencyNotSatisfied);
    }

    match task.task.run_state {
        RunState::Idle => None,
        RunState::Queued | RunState::Running | RunState::WaitingRetry => {
            Some(SkipReason::AlreadyInFlight)
        }
        RunState::Blocked => Some(SkipReason::DependencyNotSatisfied),
        RunState::Failed | RunState::Cancelled => Some(SkipReason::NeedsAttention),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{StrategyMode, Task};
    use chrono::{DateTime, Utc};
    use pretty_assertions::assert_eq;

    /// A `ready`, idle, never-run task — the one shape the queue claims.
    fn summary(run_state: RunState) -> TaskSummary {
        TaskSummary {
            task: Task {
                id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
                repository_id: "3f2b1c00-0000-4000-8000-000000000002".to_string(),
                title: "Add truncate_slug".to_string(),
                plan: Some("1. Add the function".to_string()),
                extra_instructions: None,
                column: BoardColumn::Ready,
                position: 1.0,
                run_state,
                branch: None,
                worktree_path: None,
                strategy_mode: StrategyMode::Default,
                model: None,
                effort: None,
                strategy_plan: None,
                strategy_source: None,
                strategy_updated_at: None,
                created_at: at("2026-08-20T12:00:00Z"),
                updated_at: at("2026-08-20T12:00:00Z"),
            },
            link_count: 0,
            dependency_count: 0,
            blocked_by_incomplete: false,
            last_run: None,
        }
    }

    fn at(rfc3339: &str) -> DateTime<Utc> {
        rfc3339.parse().expect("a literal timestamp must parse")
    }

    #[test]
    fn an_idle_task_in_an_opted_in_repository_is_the_only_thing_the_queue_claims() {
        assert_eq!(skip_reason(&summary(RunState::Idle), true), None);
    }

    #[test]
    fn the_repository_opt_in_outranks_every_other_reason() {
        // ADR-0012 point 1. It comes first because it is the only reason the
        // user has to act on before the queue can *ever* start the task — the
        // others clear on their own.
        for run_state in [RunState::Idle, RunState::Running, RunState::Failed] {
            assert_eq!(
                skip_reason(&summary(run_state), false),
                Some(SkipReason::UnattendedRunsNotAllowed),
                "{run_state:?} in an un-opted repository"
            );
        }
    }

    #[test]
    fn a_task_something_else_already_started_is_not_started_again() {
        for run_state in [RunState::Queued, RunState::Running, RunState::WaitingRetry] {
            assert_eq!(
                skip_reason(&summary(run_state), true),
                Some(SkipReason::AlreadyInFlight),
                "{run_state:?}"
            );
        }
    }

    #[test]
    fn a_task_whose_last_run_failed_waits_for_the_user_rather_than_being_retried() {
        // ADR-0007: failed tasks accumulate in `ready` unless the user acts. A
        // queue that read this as "claimable" would restart the top card
        // forever and never reach the second one.
        for run_state in [RunState::Failed, RunState::Cancelled] {
            assert_eq!(
                skip_reason(&summary(run_state), true),
                Some(SkipReason::NeedsAttention),
                "{run_state:?}"
            );
        }
    }

    #[test]
    fn a_blocked_dependency_is_one_reason_whichever_of_its_two_spellings_says_so() {
        // The predicate task 011 turns on, exercised from both sides today:
        // `run_state = blocked` is what ADR-0010's selection filter names, and
        // `blocked_by_incomplete` is what seam-contract D12 reserved on the
        // board's own read. Neither is computed until task 011 — this is what
        // proves the scheduler needs no edit when it is.
        assert_eq!(
            skip_reason(&summary(RunState::Blocked), true),
            Some(SkipReason::DependencyNotSatisfied)
        );

        let mut blocked = summary(RunState::Idle);
        blocked.blocked_by_incomplete = true;
        assert_eq!(
            skip_reason(&blocked, true),
            Some(SkipReason::DependencyNotSatisfied)
        );
    }

    #[test]
    fn next_to_start_takes_the_first_claimable_entry_and_never_a_skipped_one() {
        let plan = vec![
            entry("skipped", Some(SkipReason::UnattendedRunsNotAllowed), None),
            entry("first", None, Some(1)),
            entry("second", None, Some(2)),
        ];

        assert_eq!(
            next_to_start(&plan).map(|entry| entry.task_id.as_str()),
            Some("first")
        );
        assert_eq!(next_to_start(&plan[..1]), None);
        assert_eq!(next_to_start(&[]), None);
    }

    fn entry(task_id: &str, skip: Option<SkipReason>, queue_position: Option<i64>) -> QueueEntry {
        QueueEntry {
            task_id: task_id.to_string(),
            title: task_id.to_string(),
            repository_id: "repository".to_string(),
            queue_position,
            skip,
        }
    }
}
