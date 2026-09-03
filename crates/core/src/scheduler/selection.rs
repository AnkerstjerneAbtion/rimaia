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
//! same list, not a second policy. Task 012 did not change it either:
//! [`next_batch`] walks that one order and skips whatever is at its cap, so a
//! repository capped at one contributes its top card and the batch fills from
//! the next repository down — which is ADR-0010's interleaving, arrived at by
//! reading the board rather than by a second ordering that would have to be
//! kept in agreement with it.
//!
//! # Eligibility and capacity are different questions
//!
//! [`skip_reason`] answers "may this task ever start"; [`next_batch`] answers
//! "may it start right now". Only the first is a [`SkipReason`], and
//! `next_batch`'s own doc comment gives the argument for why capacity must not
//! become one.
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

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::context::ServiceContext;
use crate::db::{BoardColumn, RunState};
use crate::error::Result;
use crate::repo;
use crate::scheduler::inflight::Counts;
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
    /// Something already has this task: it is `queued` or `running`. The queue
    /// never starts a second process for it, and this is where a manual "Run
    /// now" that beat the queue to a card shows up.
    ///
    /// Also where a `waiting_retry` task with **no** `resume_after` lands. That
    /// is a task somebody else's decision put there — a hand-edited row, or a
    /// future starter this module has not met — and the conservative reading of
    /// a wait nobody scheduled is that it is not ours to end.
    AlreadyInFlight,
    /// ADR-0011's `waiting_retry`, with a deadline that has not arrived. The
    /// queue *will* start this task; it is not time yet.
    ///
    /// A fifth variant in a set whose own doc calls it closed, and the
    /// justification is that this is genuinely a different answer from
    /// [`AlreadyInFlight`](SkipReason::AlreadyInFlight): nothing is running,
    /// nothing is wrong, and the card can say *when*. Collapsing it into
    /// "already started" is what the MVP did while nothing resumed a waiting
    /// task, and it is the reading that would leave a morning reviewer unable
    /// to tell a task that is coming back at 06:00 from one that is stuck.
    WaitingForRetry,
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
            SkipReason::WaitingForRetry => "waiting to resume",
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
    /// When ADR-0011's policy scheduled this task's next attempt.
    ///
    /// **Populated only for a task in `waiting_retry`**, which is what makes it
    /// safe for `try_step` to read `resume_after.is_some()` as "this entry is a
    /// resume": a task that failed last night and was started again by hand
    /// still has an old deadline on its last run, and copying it here
    /// unconditionally would make a fresh start look like a continuation.
    ///
    /// Carried on the entry rather than looked up per claim because the loop
    /// needs the *earliest* of them to know when to wake, and a second query
    /// per pass to find that would be a poll wearing a different hat.
    pub resume_after: Option<DateTime<Utc>>,
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

    // One instant for the whole pass, not one per entry: two tasks whose
    // deadlines straddle the microsecond between two `now()` calls would
    // otherwise be judged against different clocks, and the plan a card renders
    // would not be the plan the batch was taken from.
    let now = ctx.clock.now();

    let mut plan = Vec::with_capacity(ready.len());
    let mut claimable = 0;
    for summary in &ready {
        let skip = skip_reason(summary, opted_in.contains(&summary.task.repository_id), now);
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
            resume_after: scheduled_resume(summary),
        });
    }

    Ok(plan)
}

/// The deadline ADR-0011's policy wrote for this task's next attempt, or `None`
/// for a task that is not waiting on one.
///
/// Gated on `run_state` rather than read straight off the last run — see
/// [`QueueEntry::resume_after`] for the failure that gate closes.
fn scheduled_resume(task: &TaskSummary) -> Option<DateTime<Utc>> {
    if task.task.run_state != RunState::WaitingRetry {
        return None;
    }
    task.last_run.as_ref().and_then(|run| run.resume_after)
}

/// The earliest instant at which anything in `plan` becomes startable, or
/// `None` when nothing is waiting on the clock.
///
/// This is the queue loop's timer, and the reason [`QueueEntry`] carries a
/// deadline at all: an entry skipped for [`SkipReason::WaitingForRetry`] will
/// become claimable with no mutation to announce it, so something has to know
/// *when* to look again. Entries skipped for any other reason contribute
/// nothing — a blocked or un-opted task is not waiting for a clock.
pub fn next_deadline(plan: &[QueueEntry]) -> Option<DateTime<Utc>> {
    plan.iter()
        .filter(|entry| entry.skip == Some(SkipReason::WaitingForRetry))
        .filter_map(|entry| entry.resume_after)
        .min()
}

/// Every task the queue may start right now, in board order, bounded by the
/// global limit and by each repository's own cap (ADR-0010's Selection).
///
/// `per_repository` is [`capacity::Resolved::per_repository`]: a **missing key
/// is [`DEFAULT_PER_REPOSITORY`], never "unbounded"**, so a task whose
/// repository has gone missing between two reads is capped rather than
/// promoted.
///
/// # Capacity is deliberately not a [`SkipReason`]
///
/// Eligibility ("may this task ever start") and capacity ("may it start right
/// now") are different questions with different lifetimes, and only the first
/// belongs in a set the card renders as a *problem*. The second is already
/// answered, better, by [`QueueEntry::queue_position`]: the third entry of a
/// repository capped at one reads `queue_position: 3, skip: None`, which is
/// exactly "third in line" and needs no badge. A `RepositoryAtCapacity` variant
/// would put a fact that is true for ninety seconds next to
/// [`UnattendedRunsNotAllowed`](SkipReason::UnattendedRunsNotAllowed), which is
/// true until the user acts — and the morning review would then have to tell
/// them apart. So [`skip_reason`] learns nothing here, and this function reads
/// the plan rather than changing it.
///
/// [`capacity::Resolved::per_repository`]: super::capacity::Resolved::per_repository
/// [`DEFAULT_PER_REPOSITORY`]: super::capacity::DEFAULT_PER_REPOSITORY
pub fn next_batch<'a>(
    plan: &'a [QueueEntry],
    in_flight: &Counts,
    global: usize,
    per_repository: &HashMap<String, usize>,
) -> Vec<&'a QueueEntry> {
    let mut free = global.saturating_sub(in_flight.total);
    let mut taken: HashMap<&str, usize> = HashMap::new();
    let mut batch = Vec::new();

    for entry in plan {
        if free == 0 {
            break;
        }
        if entry.skip.is_some() {
            continue;
        }
        // A lease is taken *before* the claim (see `queue`'s header), so
        // between those two points a task the button already holds still reads
        // `idle` on the board and carries no skip reason. Passing over it here
        // costs nothing — `acquire` would refuse it anyway — but counting its
        // slot as free would hand the batch one more entry than there is room
        // for, and the last of them would be refused after the whole board had
        // been walked for it.
        if in_flight.task_ids.contains(&entry.task_id) {
            continue;
        }

        let cap = per_repository
            .get(&entry.repository_id)
            .copied()
            .unwrap_or(super::capacity::DEFAULT_PER_REPOSITORY);
        let used = in_flight.in_repository(&entry.repository_id)
            + taken
                .get(entry.repository_id.as_str())
                .copied()
                .unwrap_or(0);
        if used >= cap {
            continue;
        }

        *taken.entry(entry.repository_id.as_str()).or_insert(0) += 1;
        free -= 1;
        batch.push(entry);
    }

    batch
}

/// The task the queue would claim next, or `None` when there is nothing to do.
///
/// One rule, not two: this is [`next_batch`] with nothing in flight and a
/// single slot, which is what [`Capacity::SEQUENTIAL`](super::Capacity::SEQUENTIAL)
/// resolves to. Keeping it as a wrapper rather than the separate `find` it used
/// to be is what stops "what does the queue start next" and "what does the
/// queue start next when there is room for three" from drifting apart.
pub fn next_to_start(plan: &[QueueEntry]) -> Option<&QueueEntry> {
    next_batch(plan, &Counts::default(), 1, &HashMap::new())
        .into_iter()
        .next()
}

/// Why the queue will not start `task`, or `None` when it will.
///
/// **The whole eligibility rule, in one pure function.** Order matters only for
/// which reason a card shows when more than one applies, and ADR-0012's opt-in
/// comes first because it is the one the user has to act on.
///
/// [`RunState::Idle`] is one of the two states the queue claims from.
/// ADR-0010's list names `queued`, `running` and `waiting_retry` as ineligible
/// and ADR-0008 adds `blocked`; `failed` and `cancelled` are ineligible because
/// ADR-0007 says failed tasks "accumulate in `ready` unless the user acts", and
/// a queue that re-selected them would work the top of the board forever
/// instead of moving on.
///
/// **`waiting_retry` is the other one, and it is task 014's whole point.**
/// ADR-0010 wrote that list before anything resumed a waiting task, so
/// "ineligible" was the only correct reading of it; now a task whose
/// `resume_after` has passed is exactly the task the queue is supposed to pick
/// up, and one whose deadline is still ahead is [`WaitingForRetry`](SkipReason::WaitingForRetry)
/// rather than a failure. Nothing here waits — `now` is passed in, the *loop*
/// is what sleeps.
pub fn skip_reason(
    task: &TaskSummary,
    unattended_runs_allowed: bool,
    now: DateTime<Utc>,
) -> Option<SkipReason> {
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
        RunState::WaitingRetry => match scheduled_resume(task) {
            // Due: this is ours to claim, through `claim::claim_retry`.
            Some(at) if at <= now => None,
            Some(_) => Some(SkipReason::WaitingForRetry),
            // Waiting on nothing. Whoever put it here did not schedule a
            // resume, so ending the wait is not this module's call to make —
            // see `SkipReason::AlreadyInFlight`.
            None => Some(SkipReason::AlreadyInFlight),
        },
        RunState::Queued | RunState::Running => Some(SkipReason::AlreadyInFlight),
        RunState::Blocked => Some(SkipReason::DependencyNotSatisfied),
        RunState::Failed | RunState::Cancelled => Some(SkipReason::NeedsAttention),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{MutationSource, RunStatus, StrategyMode, Task};
    use crate::strategy::StrategyOrigin;
    use crate::tasks::LastRunSummary;
    use pretty_assertions::assert_eq;

    /// The instant every test below judges a deadline against.
    const NOW: &str = "2026-08-20T02:00:00Z";

    fn now() -> DateTime<Utc> {
        at(NOW)
    }

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
                source: MutationSource::Ui,
            },
            link_count: 0,
            dependency_count: 0,
            blocked_by_incomplete: false,
            blocking_title: None,
            last_run: None,
            // Nothing configured anywhere, which is what the queue sees for a
            // task nobody has given a strategy: eligibility does not read these
            // and must not start doing so — what a task runs *with* is task
            // 020's, what the queue may claim is ADR-0007's.
            effective_model: None,
            effective_effort: None,
            effective_origin: StrategyOrigin::ClaudeCode,
        }
    }

    fn at(rfc3339: &str) -> DateTime<Utc> {
        rfc3339.parse().expect("a literal timestamp must parse")
    }

    /// A `waiting_retry` task whose policy decision is `resume_after`.
    fn waiting(resume_after: Option<DateTime<Utc>>) -> TaskSummary {
        TaskSummary {
            last_run: Some(LastRunSummary {
                status: RunStatus::Failed,
                exit_class: Some(crate::db::ExitClass::UsageLimit),
                ended_at: Some(at("2026-08-20T01:59:00Z")),
                resume_after,
            }),
            ..summary(RunState::WaitingRetry)
        }
    }

    #[test]
    fn an_idle_task_in_an_opted_in_repository_is_the_only_thing_the_queue_claims() {
        assert_eq!(skip_reason(&summary(RunState::Idle), true, now()), None);
    }

    #[test]
    fn the_repository_opt_in_outranks_every_other_reason() {
        // ADR-0012 point 1. It comes first because it is the only reason the
        // user has to act on before the queue can *ever* start the task — the
        // others clear on their own.
        for run_state in [RunState::Idle, RunState::Running, RunState::Failed] {
            assert_eq!(
                skip_reason(&summary(run_state), false, now()),
                Some(SkipReason::UnattendedRunsNotAllowed),
                "{run_state:?} in an un-opted repository"
            );
        }
    }

    #[test]
    fn a_task_something_else_already_started_is_not_started_again() {
        for run_state in [RunState::Queued, RunState::Running] {
            assert_eq!(
                skip_reason(&summary(run_state), true, now()),
                Some(SkipReason::AlreadyInFlight),
                "{run_state:?}"
            );
        }
    }

    #[test]
    fn a_waiting_task_is_passed_over_until_its_deadline_and_claimed_once_it_arrives() {
        // The predicate the whole of task 014 hangs off. Before task 014 this
        // task read `already_in_flight` and the queue never came back to it,
        // which is a night that ends at the first wall.
        assert_eq!(
            skip_reason(&waiting(Some(at("2026-08-20T06:00:00Z"))), true, now()),
            Some(SkipReason::WaitingForRetry),
        );
        assert_eq!(
            skip_reason(&waiting(Some(at("2026-08-20T02:00:00Z"))), true, now()),
            None,
            "a deadline exactly now is due, not still waiting",
        );
        assert_eq!(
            skip_reason(&waiting(Some(at("2026-08-20T01:00:00Z"))), true, now()),
            None,
        );
    }

    #[test]
    fn a_task_waiting_on_nothing_is_not_the_queues_to_resume() {
        // A hand-edited row, or a starter this module has not met. The wait was
        // not scheduled here, so ending it is not decided here either.
        assert_eq!(
            skip_reason(&waiting(None), true, now()),
            Some(SkipReason::AlreadyInFlight),
        );
        assert_eq!(
            skip_reason(&summary(RunState::WaitingRetry), true, now()),
            Some(SkipReason::AlreadyInFlight),
            "and neither is one with no run at all",
        );
    }

    #[test]
    fn a_task_whose_last_run_failed_waits_for_the_user_rather_than_being_retried() {
        // ADR-0007: failed tasks accumulate in `ready` unless the user acts. A
        // queue that read this as "claimable" would restart the top card
        // forever and never reach the second one.
        for run_state in [RunState::Failed, RunState::Cancelled] {
            assert_eq!(
                skip_reason(&summary(run_state), true, now()),
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
            skip_reason(&summary(RunState::Blocked), true, now()),
            Some(SkipReason::DependencyNotSatisfied)
        );

        let mut blocked = summary(RunState::Idle);
        blocked.blocked_by_incomplete = true;
        assert_eq!(
            skip_reason(&blocked, true, now()),
            Some(SkipReason::DependencyNotSatisfied)
        );
    }

    #[test]
    fn only_a_task_that_is_waiting_carries_a_deadline_into_the_plan() {
        // The gate that stops last night's deadline making tomorrow's fresh
        // start look like a continuation.
        let due = at("2026-08-20T06:00:00Z");
        assert_eq!(scheduled_resume(&waiting(Some(due))), Some(due));

        let restarted = TaskSummary {
            last_run: waiting(Some(due)).last_run,
            ..summary(RunState::Idle)
        };
        assert_eq!(scheduled_resume(&restarted), None);
    }

    #[test]
    fn the_loop_wakes_for_the_earliest_deadline_and_for_nothing_else() {
        let plan = vec![
            waiting_entry("late", at("2026-08-20T06:00:00Z")),
            waiting_entry("early", at("2026-08-20T03:00:00Z")),
            entry("blocked", Some(SkipReason::DependencyNotSatisfied), None),
            entry("runnable", None, Some(1)),
        ];

        assert_eq!(next_deadline(&plan), Some(at("2026-08-20T03:00:00Z")));
        assert_eq!(
            next_deadline(&plan[2..]),
            None,
            "a blocked task is not waiting for a clock",
        );
        assert_eq!(next_deadline(&[]), None);
    }

    fn waiting_entry(task_id: &str, resume_after: DateTime<Utc>) -> QueueEntry {
        QueueEntry {
            resume_after: Some(resume_after),
            ..entry(task_id, Some(SkipReason::WaitingForRetry), None)
        }
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
            resume_after: None,
        }
    }

    // -----------------------------------------------------------------------
    // next_batch (task 012, ADR-0010's Selection)
    // -----------------------------------------------------------------------

    fn in_repository(task_id: &str, repository_id: &str) -> QueueEntry {
        QueueEntry {
            repository_id: repository_id.to_string(),
            ..entry(task_id, None, Some(1))
        }
    }

    fn caps(pairs: &[(&str, usize)]) -> HashMap<String, usize> {
        pairs
            .iter()
            .map(|(id, cap)| ((*id).to_string(), *cap))
            .collect()
    }

    fn ids(batch: Vec<&QueueEntry>) -> Vec<&str> {
        batch.iter().map(|entry| entry.task_id.as_str()).collect()
    }

    #[test]
    fn a_batch_is_taken_in_board_order_up_to_the_global_limit() {
        let plan = vec![
            in_repository("first", "a"),
            in_repository("second", "b"),
            in_repository("third", "c"),
        ];

        assert_eq!(
            ids(next_batch(&plan, &Counts::default(), 2, &caps(&[]))),
            vec!["first", "second"],
        );
    }

    #[test]
    fn a_repository_at_its_cap_is_stepped_over_rather_than_stopping_the_batch() {
        // ADR-0010's interleaving, and the reason it needs no second ordering:
        // the board's own order is walked, and a repository with no room left
        // simply contributes nothing more.
        let plan = vec![
            in_repository("a1", "a"),
            in_repository("a2", "a"),
            in_repository("b1", "b"),
        ];

        assert_eq!(
            ids(next_batch(&plan, &Counts::default(), 3, &caps(&[]))),
            vec!["a1", "b1"],
            "the default cap of one lets `a` contribute exactly one",
        );
        assert_eq!(
            ids(next_batch(&plan, &Counts::default(), 3, &caps(&[("a", 2)]))),
            vec!["a1", "a2", "b1"],
            "and its own opt-out lets it contribute two",
        );
    }

    #[test]
    fn what_is_already_in_flight_is_counted_against_both_limits() {
        let plan = vec![in_repository("a2", "a"), in_repository("b1", "b")];
        let running = Counts {
            total: 1,
            per_repository: caps(&[("a", 1)]),
            task_ids: HashSet::from(["a1".to_string()]),
        };

        assert_eq!(
            ids(next_batch(&plan, &running, 3, &caps(&[]))),
            vec!["b1"],
            "`a` is full because of a run this pass did not start",
        );
        assert_eq!(
            next_batch(&plan, &running, 1, &caps(&[("a", 2)])),
            Vec::<&QueueEntry>::new(),
            "and the global limit binds even where a repository has room",
        );
    }

    #[test]
    fn a_task_something_already_holds_a_lease_on_never_takes_a_slot() {
        // The window between `acquire` and the claim: a task the button holds
        // still reads `idle` on the board and carries no skip reason. Counting
        // its slot as free would hand the batch one more entry than there is
        // room for.
        let plan = vec![in_repository("held", "a"), in_repository("free", "b")];
        let running = Counts {
            total: 1,
            per_repository: caps(&[("a", 1)]),
            task_ids: HashSet::from(["held".to_string()]),
        };

        assert_eq!(
            ids(next_batch(&plan, &running, 4, &caps(&[("a", 4)]))),
            vec!["free"]
        );
    }

    #[test]
    fn a_repository_the_caps_do_not_name_is_still_capped() {
        // Never "unbounded": a task whose repository vanished between the board
        // read and the capacity read must not become the one thing with no
        // limit.
        let plan = vec![in_repository("g1", "gone"), in_repository("g2", "gone")];

        assert_eq!(
            ids(next_batch(&plan, &Counts::default(), 4, &caps(&[("a", 4)]))),
            vec!["g1"],
        );
    }

    #[test]
    fn a_skipped_task_is_passed_over_without_spending_a_slot() {
        let plan = vec![
            entry("skipped", Some(SkipReason::UnattendedRunsNotAllowed), None),
            in_repository("first", "a"),
            in_repository("second", "b"),
        ];

        assert_eq!(
            ids(next_batch(&plan, &Counts::default(), 2, &caps(&[]))),
            vec!["first", "second"],
        );
    }

    #[test]
    fn a_full_queue_takes_nothing_rather_than_wrapping_around() {
        // `global.saturating_sub(total)`: an in-flight count above the limit is
        // reachable the moment somebody lowers `max_concurrency` with runs
        // already going, and an unsaturated subtraction would underflow into
        // "start everything".
        let plan = vec![in_repository("first", "a")];
        let over = Counts {
            total: 5,
            per_repository: caps(&[("z", 5)]),
            task_ids: HashSet::new(),
        };

        assert_eq!(
            next_batch(&plan, &over, 2, &caps(&[])),
            Vec::<&QueueEntry>::new()
        );
    }

    #[test]
    fn next_to_start_is_the_first_entry_of_a_single_slot_batch() {
        // One rule, not two. If these ever disagreed, "what runs next" and
        // "what runs next when there is room for three" would have drifted.
        let plan = vec![
            entry("skipped", Some(SkipReason::NeedsAttention), None),
            in_repository("first", "a"),
            in_repository("second", "a"),
        ];

        assert_eq!(
            next_to_start(&plan).map(|entry| entry.task_id.as_str()),
            ids(next_batch(&plan, &Counts::default(), 1, &HashMap::new()))
                .first()
                .copied(),
        );
    }
}
