//! The board's side of the port: one function per method, and the only code
//! either adapter calls (seam-contract D31 point 1).
//!
//! These are the board's decisions, **moved** out of the runner rather than
//! copied (ADR-0006): what a run is composed from, where a claim may go, when
//! a task is tried again and where it lands. The runner reports facts; this
//! module decides what they mean.
//!
//! # Scope
//!
//! A method that takes a [`LeaseRef`] runs under the adapter's context
//! narrowed to the lease's team, and only after checking the adapter's scope
//! contains that team (D31 point 13). It refuses a task that does not exist,
//! or that is not in the lease's team, with `NotFound`, in the sentence a
//! never-issued task gets, and one that also takes a run id refuses a run of
//! another task the same way. That is all "scoped by `lease`" means before
//! task 043: there is no lease row to compare a generation against yet, so no
//! `Conflict` (D8).

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use crate::context::{ServiceContext, TeamScope};
use crate::db::{settings, StrategySource};
use crate::error::{Error, ErrorCode, Result};
use crate::paths::AppPaths;
use crate::review::findings::{self, NewReviewFinding};
use crate::review_loop;
use crate::runner::events::RunTail;
use crate::runner::limits::{self, DEFAULT_MAX_TURNS, DISALLOWED_TOOLS, MAX_TURNS};
use crate::runner::outcome::{self, NewRun, RunOutcome};
use crate::runner::process::DEFAULT_DISALLOWED_TOOLS;
use crate::runner::provider::AgentProvider;
use crate::runner::RunTrigger;
use crate::runs::bundle::RunCapture;
use crate::scheduler::attempts::{self, Ending};
use crate::scheduler::claim::{self as edges, ClaimOutcome};
use crate::scheduler::{retry, selection};
use crate::strategy::{self, catalogue};
use crate::tasks;
use crate::tasks::strategy::{set_task_strategy, StrategyPlan};

use super::types::{
    Claim, ClaimTarget, FinishReceipt, FinishRun, FreeCapacity, Heartbeat, LeasePurpose, LeaseRef,
    RunContext, StartRun, TeamLimits, TranscriptAck, TranscriptChunk,
};

/// The context a claim of `task_id` would carry. Writes nothing.
pub async fn preview(
    ctx: &ServiceContext,
    provider: &dyn AgentProvider,
    task_id: &str,
) -> Result<RunContext> {
    read_context(ctx, provider, task_id).await
}

/// The single path for every process a runner starts (D31 point 4).
///
/// `Run` and `Plan` name their task. `Next` asks the board to choose one, and
/// is the only place in the workspace that runs selection for a claim (task
/// 042): task 045 adds its eligibility predicates beside the listed-repository
/// check in [`claim_next`], and task 043 replaces the edges in [`claim_task`].
/// Neither touches the runner.
pub async fn claim(
    ctx: &ServiceContext,
    provider: &dyn AgentProvider,
    target: ClaimTarget,
) -> Result<Option<Claim>> {
    match target {
        ClaimTarget::Next {
            capacity,
            repositories,
            wait,
        } => claim_next(ctx, provider, &capacity, &repositories, wait).await,
        ClaimTarget::Run {
            task_id,
            trigger,
            continue_session,
        } => {
            claim_task(
                ctx,
                provider,
                &task_id,
                Route::Run {
                    trigger,
                    continue_session,
                },
            )
            .await
        }
        ClaimTarget::Plan { task_id } => claim_task(ctx, provider, &task_id, Route::Plan).await,
    }
}

/// Which claim [`claim_task`] makes of the task it was named.
#[derive(Debug, Clone, Copy)]
enum Route {
    /// Both `run_state` edges: a fresh start through `claim::claim`, or a due
    /// retry through `claim_retry` and the point it resumes.
    Run {
        trigger: RunTrigger,
        continue_session: bool,
    },
    /// D17's planner: a lease and nothing else.
    Plan,
}

/// `ClaimTarget::Next`'s body: the top startable task in board order, among
/// the repositories the runner listed and within the capacity it reported.
///
/// Tries once. With a `wait` it then sleeps until a change event or the
/// deadline, whichever comes first, and tries again, returning `None` once the
/// deadline has passed with nothing claimed. Both waits are the context's, its
/// change channel and its injected clock, never a `tokio` timer (D31 point 4),
/// so a test drives it by publishing and by advancing a `TestClock`. The
/// channel is subscribed before the first try, so a change that lands between
/// a try and the wait is still buffered and wakes it.
///
/// The solo loop always sends a zero `wait`, and so never subscribes here; the
/// waiting form is task 053's long poll, which adds the clamp and the
/// `earliest_due` wake to this body (D31's 2026-10-10 amendment).
async fn claim_next(
    ctx: &ServiceContext,
    provider: &dyn AgentProvider,
    capacity: &FreeCapacity,
    repositories: &[String],
    wait: std::time::Duration,
) -> Result<Option<Claim>> {
    let listed: BTreeSet<String> = repositories.iter().cloned().collect();
    if wait.is_zero() {
        return try_next(ctx, provider, capacity, &listed).await;
    }

    let mut changes = ctx.subscribe();
    let now = ctx.clock.now();
    let deadline = chrono::Duration::from_std(wait)
        .ok()
        .and_then(|wait| now.checked_add_signed(wait))
        .unwrap_or(DateTime::<Utc>::MAX_UTC);

    loop {
        drain(&mut changes);
        if let Some(claim) = try_next(ctx, provider, capacity, &listed).await? {
            return Ok(Some(claim));
        }
        if ctx.clock.now() >= deadline {
            return Ok(None);
        }

        let due = ctx.clock.sleep_until(deadline);
        tokio::select! {
            // Every arm is cancel-safe: `recv` keeps its place in the channel,
            // and a clock wait holds nothing. `Lagged` is a wake like any
            // other, since the answer to both is to look again.
            event = changes.recv() => {
                if matches!(event, Err(RecvError::Closed)) {
                    // Nothing can publish any more; only the clock can change
                    // the answer now.
                    ctx.clock.sleep_until(deadline).await;
                }
            }
            () = due => {}
        }
    }
}

/// One look at the board for [`claim_next`]: selection, then the claim, then
/// the next entry if the claim was lost to another starter.
///
/// 1. [`selection::plan`] over the listed repositories. A repository not in
///    the list is skipped as [`SkipReason::UnattendedRunsNotAllowed`], exactly
///    as today's opt-in is.
/// 2. [`selection::first_startable`]: the first entry in board order with no
///    skip reason and a free slot in its repository, while `capacity.total`
///    is above zero. Capacity is not a skip reason (D21 point 3).
/// 3. The claim, by today's two routes, chosen by what the plan says the entry
///    is: a fresh start, or a due retry when `resume_after` is set. A lost
///    race moves to the next entry rather than returning, and costs no slot.
///
/// [`SkipReason::UnattendedRunsNotAllowed`]: crate::scheduler::SkipReason::UnattendedRunsNotAllowed
async fn try_next(
    ctx: &ServiceContext,
    provider: &dyn AgentProvider,
    capacity: &FreeCapacity,
    listed: &BTreeSet<String>,
) -> Result<Option<Claim>> {
    let mut plan = selection::plan(ctx, listed).await?;

    while let Some(entry) = selection::first_startable(&plan, capacity) {
        let task_id = entry.task_id.clone();
        let route = Route::Run {
            // ADR-0012: the unattended path, behind the opt-in the plan just
            // applied.
            trigger: RunTrigger::Queued,
            // `resume_after` is populated only for a task in `waiting_retry`
            // (`QueueEntry::resume_after`), so this is what the entry is, not
            // a guess.
            continue_session: entry.resume_after.is_some(),
        };

        if let Some(claim) = claim_task(ctx, provider, &task_id, route).await? {
            return Ok(Some(claim));
        }
        plan.retain(|entry| entry.task_id != task_id);
    }

    Ok(None)
}

/// Throws away every change event already buffered: the look that follows
/// re-reads the board, which is all any of them asks for.
fn drain(changes: &mut broadcast::Receiver<crate::ChangeEvent>) {
    loop {
        match changes.try_recv() {
            Ok(_) | Err(TryRecvError::Lagged(_)) => continue,
            Err(TryRecvError::Empty | TryRecvError::Closed) => return,
        }
    }
}

/// Today's claim routes for a named task, **reads first, edges last**.
///
/// Everything that can refuse is read before either edge is taken, so a
/// refusal writes nothing: the context, and for a retry the point it resumes,
/// of whatever kind was waiting (task 021). Reading the point before the edge
/// is safe for the same reason the edge is conditional: if another starter
/// takes the task in between, the edge is the write that loses, and nothing
/// read here is used. Nothing is read after the edges; a read a later task
/// adds there must release on `Err`.
///
/// A task that is gone by the time it is read is a lost claim, as
/// `ClaimOutcome::Lost` always said it was, never an error.
async fn claim_task(
    ctx: &ServiceContext,
    provider: &dyn AgentProvider,
    task_id: &str,
    route: Route,
) -> Result<Option<Claim>> {
    let context = match read_context(ctx, provider, task_id).await {
        Ok(context) => context,
        Err(error) if error.code() == ErrorCode::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    // The lease's team is the task's own row's (D31 point 2), read with the
    // rest, before any edge.
    let team_id = match tasks::service::team_of(ctx, task_id).await {
        Ok(team_id) => team_id,
        Err(error) if error.code() == ErrorCode::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };

    let (purpose, trigger, resume) = match route {
        Route::Run {
            trigger,
            continue_session: false,
        } => {
            if edges::claim(ctx, task_id).await? == ClaimOutcome::Lost {
                return Ok(None);
            }
            (LeasePurpose::Implementation, trigger, None)
        }
        Route::Run {
            trigger,
            continue_session: true,
        } => {
            // A review or fix resumes as itself (D29 point 3): the kind
            // travels on the point, and the lease's purpose is that kind.
            let point = attempts::resume_point(ctx, task_id).await?;
            if edges::claim_retry(ctx, task_id).await? == ClaimOutcome::Lost {
                return Ok(None);
            }
            let purpose = point
                .as_ref()
                .map_or(LeasePurpose::Implementation, |point| point.kind.into());
            (purpose, trigger, point)
        }
        // D17's planner: a lease and nothing else. No `run_state` edge, because
        // a planner that took one would need `Running -> Running` later.
        Route::Plan => (LeasePurpose::Strategy, RunTrigger::Manual, None),
    };

    Ok(Some(Claim {
        lease: LeaseRef::solo(task_id, team_id),
        purpose,
        trigger,
        resume,
        context,
    }))
}

/// Nothing to renew and nothing to fence before task 043, and a solo Cancel
/// reaches `InFlight::cancel` directly rather than through here.
pub fn heartbeat(_held: &[LeaseRef]) -> Heartbeat {
    Heartbeat::default()
}

pub async fn run_context(
    ctx: &ServiceContext,
    provider: &dyn AgentProvider,
    lease: &LeaseRef,
) -> Result<RunContext> {
    let ctx = &lease_context(ctx, lease)?;
    read_context(ctx, provider, &lease.task_id).await
}

/// Writes `tasks.branch` and nothing else: the worktree path is the runner's
/// (ADR-0028 point 2). Its production caller is `worktree::prepare` (task 066).
pub async fn record_branch(ctx: &ServiceContext, lease: &LeaseRef, branch: &str) -> Result<()> {
    let ctx = &lease_context(ctx, lease)?;
    ensure_task(ctx, &lease.task_id).await?;

    let now = ctx.clock.now();
    let team_id = sqlx::query_scalar!(
        "UPDATE tasks SET branch = ?1, updated_at = ?2 WHERE id = ?3 RETURNING team_id",
        branch,
        now,
        lease.task_id,
    )
    .fetch_one(&ctx.pool)
    .await?;

    ctx.publish(crate::ChangeEvent::tasks(team_id, [lease.task_id.clone()]));
    Ok(())
}

/// Opens the `runs` row under the id the runner minted, naming `runner_id`,
/// the runner the adapter serves. In process the transcript path comes from
/// this board's own `paths` (D31 point 4).
pub async fn start_run(
    ctx: &ServiceContext,
    paths: &AppPaths,
    runner_id: &str,
    lease: &LeaseRef,
    run: StartRun,
) -> Result<()> {
    let ctx = &lease_context(ctx, lease)?;
    ensure_task(ctx, &lease.task_id).await?;

    outcome::insert_run(
        ctx,
        paths,
        run.run_id,
        Some(runner_id),
        NewRun {
            task_id: lease.task_id.clone(),
            kind: run.kind,
            session_id: run.session_id,
            prompt: run.prompt,
            base_ref: run.base_ref,
            base_sha: run.base_sha,
        },
    )
    .await?;
    Ok(())
}

/// Acknowledges through the chunk's end and copies nothing: in solo the
/// runner's file *is* the board's copy (ADR-0028 point 4).
pub async fn append_transcript(
    ctx: &ServiceContext,
    lease: &LeaseRef,
    chunk: TranscriptChunk,
) -> Result<TranscriptAck> {
    let ctx = &lease_context(ctx, lease)?;
    ensure_task(ctx, &lease.task_id).await?;
    ensure_run_of_task(ctx, &lease.task_id, &chunk.run_id).await?;

    Ok(TranscriptAck {
        stored_through: chunk.offset + chunk.bytes.len() as u64,
    })
}

/// Drops a tail for a lease whose team the adapter's scope does not hold:
/// synchronous and infallible, it has no `NotFound` to answer with, and a
/// dropped tail costs nothing (D14).
pub fn publish_tail(ctx: &ServiceContext, lease: &LeaseRef, tail: RunTail) {
    if ctx.scope.contains(&lease.team_id) {
        ctx.publish_tail(tail);
    }
}

/// Closes the run and lands the task, deciding `resume_after` here, and
/// whether a review loop continues (ADR-0017).
///
/// A runner that chose its own retry time would be a second copy of
/// ADR-0011's table on a machine the board does not control, so one that sends
/// it is refused. The decision is the one `runner::process` used to make
/// before task 036, minus the usage-limit pause: that is runner-owned state,
/// and the runner raises it from the [`NextStep`](super::NextStep) this answers. The loop's
/// next step is the task-side step's answer, `review_loop::decide`.
pub async fn finish_run(
    ctx: &ServiceContext,
    lease: &LeaseRef,
    run_id: &str,
    finish: FinishRun,
) -> Result<FinishReceipt> {
    let ctx = &lease_context(ctx, lease)?;
    ensure_task(ctx, &lease.task_id).await?;
    ensure_run_of_task(ctx, &lease.task_id, run_id).await?;

    let FinishRun {
        mut outcome,
        head_sha,
        bundle,
        window_closes_at,
        // Acknowledged either way in process: the file never left the machine.
        transcript: _,
    } = finish;

    if outcome.resume_after.is_some() {
        return Err(Error::invalid(
            "a finished run reports what happened, never when to try again: \
             `resume_after` is the board's to decide (ADR-0011)",
        ));
    }

    outcome.resume_after =
        decide_resume_after(ctx, &lease.task_id, run_id, &outcome, window_closes_at).await;

    let (run, next) = outcome::finish_run_within(
        ctx,
        run_id,
        &outcome,
        &RunCapture { head_sha, bundle },
        window_closes_at,
    )
    .await?;

    Ok(FinishReceipt { run, next })
}

/// When — or whether — this task is tried again (ADR-0011).
///
/// Infallible by construction, as it always was: a history that cannot be
/// read leaves the attempt un-retried. An outcome recorded with no retry is a
/// card a human sees in the morning, where a propagated error would abandon
/// the `runs` row and leave the task `running` with no process.
async fn decide_resume_after(
    ctx: &ServiceContext,
    task_id: &str,
    run_id: &str,
    outcome: &RunOutcome,
    window_closes_at: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    let ending = Ending {
        exit_class: outcome.exit_class,
        usage_limit_resets_at: outcome.usage_limit_resets_at,
    };

    let history = match attempts::history(ctx, task_id, ending).await {
        Ok(history) => history,
        Err(error) => {
            tracing::error!(
                %task_id, %run_id, %error,
                "could not read this task's attempt history; it will not be retried",
            );
            return None;
        }
    };

    // `None` is no rows at all, for a run that has one. Nothing to resume is
    // the safe reading. The run id seeds the jitter: see `retry::jitter`.
    history.and_then(|history| {
        retry::decide(&history, ctx.clock.now(), run_id, window_closes_at).resume_after()
    })
}

/// `scheduler::claim::release`'s rule, unchanged: a task still `running`
/// becomes `failed`, and a verdict already written is kept.
///
/// It does not branch on purpose. Before task 043 there is no lease row to
/// read one from, and a strategy lease meets a `running` task only when a
/// crash stranded it, which the next launch's reconcile fails anyway.
pub async fn release(ctx: &ServiceContext, lease: &LeaseRef) -> Result<()> {
    let ctx = &lease_context(ctx, lease)?;
    ensure_task(ctx, &lease.task_id).await?;
    edges::release(ctx, &lease.task_id).await;
    Ok(())
}

pub async fn record_strategy(
    ctx: &ServiceContext,
    lease: &LeaseRef,
    plan: StrategyPlan,
) -> Result<()> {
    let ctx = &lease_context(ctx, lease)?;
    ensure_task(ctx, &lease.task_id).await?;
    set_task_strategy(ctx, &lease.task_id, plan, StrategySource::Planner).await?;
    Ok(())
}

pub async fn record_review_findings(
    ctx: &ServiceContext,
    lease: &LeaseRef,
    run_id: &str,
    findings: Vec<NewReviewFinding>,
) -> Result<()> {
    let ctx = &lease_context(ctx, lease)?;
    ensure_task(ctx, &lease.task_id).await?;
    ensure_run_of_task(ctx, &lease.task_id, run_id).await?;
    findings::record(ctx, &lease.task_id, run_id, findings).await?;
    Ok(())
}

/// What a run of `task_id` is composed from and bounded by, read once.
async fn read_context(
    ctx: &ServiceContext,
    provider: &dyn AgentProvider,
    task_id: &str,
) -> Result<RunContext> {
    let task = tasks::get_task(ctx, task_id).await?;
    let repository = crate::repo::get(ctx, &task.task.repository_id).await?;
    // Every team setting below is the task's own team's, so the prompt, the
    // planner's catalogue and the run's limits all come from the team that
    // owns the card, whichever teams the context reaches.
    let team_id = tasks::service::team_of(ctx, task_id).await?;
    let base_instructions = settings::base_instructions_for(ctx, &team_id).await?;

    let global = strategy::settings::global_default_for(ctx, &team_id).await?;
    let per_repository = strategy::settings::repository_default(ctx, &repository.id).await?;
    let strategy = strategy::effective_strategy(&task.task, &per_repository, &global);

    let catalogue = catalogue::catalogue_for(ctx, &team_id, provider).await?;
    let limits = team_limits(ctx, &team_id).await?;

    let resolved = review_loop::config::resolve(ctx, task_id, &repository.id).await?;
    let review = review_loop::context(ctx, task_id, resolved).await?;

    Ok(RunContext {
        task,
        repository,
        base_instructions,
        strategy,
        catalogue,
        limits,
        review: Some(review),
    })
}

/// The team's half of what bounds a run, read once, here, for the team that
/// owns the task (ADR-0028 point 2, task 042).
///
/// The only reader of the team's `max_turns` and `disallowed_tools`. A runner
/// receives them as `RunContext::limits` and combines them with its own
/// override in `runner::limits::effective`; nothing on the runner reads the
/// team's settings itself.
pub async fn team_limits(ctx: &ServiceContext, team_id: &str) -> Result<TeamLimits> {
    Ok(TeamLimits {
        max_turns: team_max_turns(ctx, team_id).await?,
        disallowed_tools: settings::get_team(ctx, team_id, DISALLOWED_TOOLS)
            .await?
            .map(|stored| limits::rules(&stored)),
    })
}

/// The team's per-attempt turn budget, or [`DEFAULT_MAX_TURNS`] when nobody
/// has set one.
///
/// Tolerant on read, like every other key in this codebase and for ADR-0003's
/// reason — but note what "tolerant" costs here and does not: an unusable
/// value falls back to a budget that is generous, never to *no* budget,
/// because "no budget" is the runaway ADR-0011 asked for a bound against.
pub async fn team_max_turns(ctx: &ServiceContext, team_id: &str) -> Result<u32> {
    let Some(stored) = settings::get_team(ctx, team_id, MAX_TURNS).await? else {
        return Ok(DEFAULT_MAX_TURNS);
    };

    match stored.trim().parse::<u32>() {
        Ok(0) | Err(_) => {
            tracing::warn!(
                value = stored,
                default = DEFAULT_MAX_TURNS,
                "unusable max_turns; falling back to the default"
            );
            Ok(DEFAULT_MAX_TURNS)
        }
        Ok(value) => Ok(value),
    }
}

/// The team's blocklist as rules, or [`DEFAULT_DISALLOWED_TOOLS`] when nobody
/// has set one.
///
/// An explicitly empty setting means an empty list — the operator turning the
/// blocklist off is a thing they are allowed to do, and silently restoring the
/// default would be the same defect `settings::base_instructions` documents.
/// [`team_limits`] carries the same distinction as `None` against
/// `Some(vec![])`.
pub async fn team_disallowed_tools(ctx: &ServiceContext, team_id: &str) -> Result<Vec<String>> {
    Ok(team_limits(ctx, team_id)
        .await?
        .disallowed_tools
        .unwrap_or_else(|| {
            DEFAULT_DISALLOWED_TOOLS
                .iter()
                .map(|pattern| (*pattern).to_string())
                .collect()
        }))
}

/// The context a lease's calls run under: the adapter's, narrowed to the
/// lease's team (D31 point 13).
///
/// Narrowed only after checking the adapter's scope contains that team, so a
/// lease naming another team is `NotFound` in the sentence a never-issued task
/// gets, never a refusal that names a team. A lease naming the right team on
/// another team's task is refused by the scoped query in [`ensure_task`].
fn lease_context(ctx: &ServiceContext, lease: &LeaseRef) -> Result<ServiceContext> {
    if !ctx.scope.contains(&lease.team_id) {
        return Err(Error::not_found(format!(
            "no task with id {}",
            lease.task_id
        )));
    }
    Ok(ctx.with_scope(TeamScope::one(lease.team_id.clone())))
}

/// `NotFound` for a lease on a task that does not exist in the context's
/// scope, in the sentence `tasks::get_task` answers the same question with.
async fn ensure_task(ctx: &ServiceContext, task_id: &str) -> Result<()> {
    tasks::service::team_of(ctx, task_id).await?;
    Ok(())
}

/// `NotFound` for a run that does not exist or belongs to another task: the
/// lease bounds what a call may touch, and a run outside it is not there.
async fn ensure_run_of_task(ctx: &ServiceContext, task_id: &str, run_id: &str) -> Result<()> {
    let scope = ctx.scope.json();
    let owner: Option<String> = sqlx::query_scalar!(
        "SELECT r.task_id FROM runs r JOIN tasks t ON t.id = r.task_id
          WHERE r.id = ?1 AND t.team_id IN (SELECT value FROM json_each(?2))",
        run_id,
        scope,
    )
    .fetch_optional(&ctx.pool)
    .await?;
    match owner {
        Some(owner) if owner == task_id => Ok(()),
        Some(_) => Err(Error::not_found(format!(
            "run {run_id} is not a run of task {task_id}"
        ))),
        None => Err(Error::not_found(format!("no run with id {run_id}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_solo_heartbeat_renews_nothing_and_fences_nothing() {
        assert_eq!(
            heartbeat(&[LeaseRef::solo("t", "3f2b1c00-0000-4000-8000-0000000000a1")]),
            Heartbeat::default()
        );
    }
}
