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
//! contains that team (D31 point 13). Then, inside the transaction of the
//! first write it guards, it reads the live lease with
//! [`lease::current`](super::lease): a task that does not exist, or that is
//! not in the lease's team, is `NotFound` in the sentence a never-issued task
//! gets, and a lease that is not the current one for this runner is
//! `Conflict` (D8's 043 amendment). One that also takes a run id refuses a run
//! of another task as `NotFound`, after the fence.
//!
//! Every method also takes the runner the adapter serves, which is its scope
//! and never a request field (D31 point 3): the fence compares the lease's
//! holder with it.

use chrono::{DateTime, Utc};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use crate::context::{ScopedTx, ServiceContext, TeamScope};
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
use crate::scheduler::retry;
use crate::scheduler::selection::{self, RunnerView};
use crate::strategy::{self, catalogue};
use crate::tasks;
use crate::tasks::strategy::{prepare_strategy, write_strategy, StrategyPlan};
use crate::worktree::base_ref;

use super::lease::{self, Candidate, ClaimRequest, Door, Edges, LeaseTerm, PhaseModels};
use super::types::{
    Claim, ClaimTarget, FinishReceipt, FinishRun, FreeCapacity, Heartbeat, LeaseRef, NextStep,
    RunAuthorship, RunContext, StartRun, TeamLimits, TranscriptAck, TranscriptChunk,
};
use crate::consent::ceiling::StrategyCeiling;
use crate::consent::{self, Composition};
use crate::db::RunKind;

/// The context a claim of `task_id` would carry. Writes nothing.
///
/// Refuses, with the claim's own sentence, a task `lease::eligible` would
/// refuse this runner for what a claim would compose now (task 045): the
/// inline planner's claim includes the implementation after it. It takes no
/// ceiling: it passes `StrategyCeiling::default()`, so it reports eligibility
/// and consent refusals only, and a ceiling refusal comes from the claim.
pub async fn preview(
    ctx: &ServiceContext,
    runner: Runner<'_>,
    task_id: &str,
) -> Result<RunContext> {
    let context = read_context(ctx, runner, task_id).await?;
    let composes = consent::composes_now(ctx, &context.task.task).await?;
    let composition = Composition::of(&context);
    let models = PhaseModels::of(&context);
    let ceiling = StrategyCeiling::default();
    let candidate = Candidate {
        runner_id: runner.id,
        provider: runner.provider.id(),
        catalogue: &context.catalogue,
        models: &models,
        ceiling: &ceiling,
        composition: &composition,
    };
    let mut conn = ctx.pool.acquire().await?;
    let verdict = lease::eligible(&mut conn, task_id, &candidate, composes).await?;
    drop(conn);
    match verdict.refusal() {
        Some(refusal) => Err(Error::invalid(refusal)),
        None => Ok(context),
    }
}

/// Whether a runner's owner is at the machine when they ask it to start a run
/// (ADR-0031 point 7).
///
/// Decided by the door, never by a request field. Every desktop command and
/// the loopback operator MCP server run on the runner's own machine and pass
/// [`AtRunner`](Self::AtRunner); task 052's browser route passes
/// [`Remote`](Self::Remote). Not `Presence`: task 058's
/// `rimaia_runner::host::Presence` is the runner's own view of its machine
/// and never crosses to the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerPresence {
    /// At the runner, with the app in front of them: ADR-0012 point 6's
    /// interactive run.
    AtRunner,
    /// Somewhere else: nobody is there to answer a prompt, so the run is an
    /// unattended one.
    Remote,
}

/// Whether the caller may ask `runner_id` to start a run, and the permission
/// posture the run takes if so (ADR-0031 point 7).
///
/// Every start door calls this before it claims: Run now and Retry now
/// (`runner::start::claim_manual_start`), and Plan now and a planning pass
/// (`runner::strategy::claim_for_planning`). None restates a check here
/// (ADR-0006). In order:
///
/// 1. **The runner** must exist, and its owner must be a member of a team
///    `ctx` reaches. Otherwise `NotFound`, in the sentence a never-issued id
///    gets (ADR-0029 point 5).
/// 2. A runner that has been **unpaired** starts nothing: `Invalid`.
/// 3. **Only its owner** may ask: `Invalid` for anyone else. A teammate makes
///    a task claimable by assigning it; starting a process on someone else's
///    machine is not a board action.
/// 4. **Presence decides the posture.** [`OwnerPresence::AtRunner`] is
///    [`RunTrigger::Manual`] (`acceptEdits`); [`OwnerPresence::Remote`] is
///    [`RunTrigger::Queued`], an unattended run, held to ADR-0012's
///    per-repository opt-in (and from task 045 to consent) exactly as a queued
///    run is.
///
/// Capacity is not asked about (D19 point 5): a named start takes no
/// `FreeCapacity`, and the board applies none.
///
/// `ctx` is the caller's own context, whose `actor` is the person asking,
/// never the in-process adapter's `System` one. Writes nothing.
pub async fn authorize_start(
    ctx: &ServiceContext,
    runner_id: &str,
    presence: OwnerPresence,
) -> Result<RunTrigger> {
    let scope = ctx.scope.json();
    let runner = sqlx::query!(
        r#"SELECT r.user_id, r.unpaired_at AS "unpaired_at: DateTime<Utc>"
             FROM runners r
            WHERE r.id = ?1
              AND EXISTS (SELECT 1 FROM team_memberships m
                           WHERE m.user_id = r.user_id
                             AND m.team_id IN (SELECT value FROM json_each(?2)))"#,
        runner_id,
        scope,
    )
    .fetch_optional(&ctx.pool)
    .await?
    .ok_or_else(|| Error::not_found(format!("no runner with id {runner_id}")))?;

    if runner.unpaired_at.is_some() {
        return Err(Error::invalid(
            "this runner has been unpaired and can no longer start runs",
        ));
    }
    if runner.user_id != ctx.actor {
        return Err(Error::invalid(
            "only the owner of this runner can start a run on it; assign the task to them, \
             or leave it ready for their queue",
        ));
    }

    Ok(match presence {
        OwnerPresence::AtRunner => RunTrigger::Manual,
        OwnerPresence::Remote => RunTrigger::Queued,
    })
}

/// The runner an adapter serves: who the board's leases name, and what it
/// leases for.
#[derive(Debug, Clone, Copy)]
pub struct Runner<'a> {
    pub id: &'a str,
    pub provider: &'a dyn AgentProvider,
    /// Whether this board's leases expire. Solo's never do (ADR-0031 point 5).
    pub term: LeaseTerm,
}

/// The single path for every process a runner starts (D31 point 4).
///
/// `Run` and `Plan` name their task. `Next` asks the board to choose one, and
/// is the only place in the workspace that runs selection for a claim (task
/// 042): task 045 adds its eligibility predicates to `lease::eligible`, which
/// both `selection::plan` and the claim transaction ask. Every form takes its
/// edges and its lease in one transaction (`board::lease`, task 043).
pub async fn claim(
    ctx: &ServiceContext,
    runner: Runner<'_>,
    target: ClaimTarget,
) -> Result<Option<Claim>> {
    match target {
        ClaimTarget::Next {
            capacity,
            repositories,
            wait,
            ceiling,
        } => claim_next(ctx, runner, &capacity, &repositories, wait, &ceiling).await,
        ClaimTarget::Run {
            task_id,
            trigger,
            continue_session,
            ceiling,
        } => {
            claim_task(
                ctx,
                runner,
                &task_id,
                Route::Run {
                    trigger,
                    continue_session,
                },
                Door::Named,
                &ceiling,
            )
            .await
        }
        ClaimTarget::Plan { task_id, ceiling } => {
            claim_task(ctx, runner, &task_id, Route::Plan, Door::Named, &ceiling).await
        }
    }
}

/// Which claim [`claim_task`] makes of the task it was named.
#[derive(Debug, Clone, Copy)]
enum Route {
    /// Both `run_state` edges for a fresh start, or `waiting_retry ->
    /// running` and the point it resumes for a retry.
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
    runner: Runner<'_>,
    capacity: &FreeCapacity,
    repositories: &[String],
    wait: std::time::Duration,
    ceiling: &StrategyCeiling,
) -> Result<Option<Claim>> {
    let view = RunnerView {
        runner_id: runner.id.to_string(),
        provider: runner.provider.id(),
        repositories: repositories.iter().cloned().collect(),
        ceiling: ceiling.clone(),
    };
    if wait.is_zero() {
        return try_next(ctx, runner, capacity, &view).await;
    }

    let mut changes = ctx.subscribe();
    let now = ctx.clock.now();
    let deadline = chrono::Duration::from_std(wait)
        .ok()
        .and_then(|wait| now.checked_add_signed(wait))
        .unwrap_or(DateTime::<Utc>::MAX_UTC);

    loop {
        drain(&mut changes);
        if let Some(claim) = try_next(ctx, runner, capacity, &view).await? {
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
/// 1. [`selection::plan`] over the runner's view. A repository not in its list
///    is skipped as [`SkipReason::UnattendedRunsNotAllowed`], exactly as
///    today's opt-in is, and a task this runner is not eligible for (pinned
///    to another runner) is passed over before positions are numbered.
/// 2. [`selection::first_startable`]: the first entry in board order with no
///    skip reason and a free slot in its repository, while `capacity.total`
///    is above zero. Capacity is not a skip reason (D21 point 3).
/// 3. The claim transaction, by what the plan says the entry is: a fresh
///    start, or a due retry when `resume_after` is set. A lost race, or an
///    eligibility the transaction no longer agrees with, moves to the next
///    entry rather than returning, and costs no slot.
///
/// [`SkipReason::UnattendedRunsNotAllowed`]: crate::scheduler::SkipReason::UnattendedRunsNotAllowed
async fn try_next(
    ctx: &ServiceContext,
    runner: Runner<'_>,
    capacity: &FreeCapacity,
    view: &RunnerView,
) -> Result<Option<Claim>> {
    let mut plan = selection::plan(ctx, view).await?;

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

        if let Some(claim) =
            claim_task(ctx, runner, &task_id, route, Door::Next, &view.ceiling).await?
        {
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

/// A claim of a named task: **reads first, then one transaction**.
///
/// Everything a claim returns is read before the transaction opens: the
/// context, the task's team, and for a retry the point it resumes, of
/// whatever kind was waiting (task 021). Reading them first is safe for the
/// reason the transaction is conditional: if another starter takes the task in
/// between, the transaction is the write that loses, and nothing read here is
/// used. The transaction itself reads nothing over the pool, because it holds
/// the write lock.
///
/// A task that is gone by the time it is read is a lost claim, never an
/// error.
async fn claim_task(
    ctx: &ServiceContext,
    runner: Runner<'_>,
    task_id: &str,
    route: Route,
    door: Door,
    ceiling: &StrategyCeiling,
) -> Result<Option<Claim>> {
    let context = match read_context(ctx, runner, task_id).await {
        Ok(context) => context,
        Err(error) if error.code() == ErrorCode::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    // The lease's team is the task's own row's (D31 point 2), read with the
    // rest, before the transaction.
    let team_id = match tasks::service::team_of(ctx, task_id).await {
        Ok(team_id) => team_id,
        Err(error) if error.code() == ErrorCode::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };

    let (edges, trigger, resume) = match route {
        Route::Run {
            trigger,
            continue_session: false,
        } => (Edges::Fresh, trigger, None),
        Route::Run {
            trigger,
            continue_session: true,
        } => {
            // A review or fix resumes as itself (D29 point 3): the kind
            // travels on the point, and the lease's purpose is that kind.
            let point = attempts::resume_point(ctx, task_id).await?;
            let kind = point.as_ref().map(|point| point.kind);
            (Edges::Resume { kind }, trigger, point)
        }
        Route::Plan => (Edges::Plan, RunTrigger::Manual, None),
    };

    let models = PhaseModels::of(&context);
    let composition = Composition::of(&context);
    let granted = lease::claim(
        ctx,
        ClaimRequest {
            task_id,
            candidate: Candidate {
                runner_id: runner.id,
                provider: runner.provider.id(),
                catalogue: &context.catalogue,
                models: &models,
                ceiling,
                composition: &composition,
            },
            term: runner.term,
            edges,
            door,
            mode: context.strategy.mode,
        },
    )
    .await?;
    let Some(granted) = granted else {
        tracing::debug!(%task_id, "the claim was lost; another starter reached this task first");
        return Ok(None);
    };

    Ok(Some(Claim {
        lease: LeaseRef::new(task_id, granted.generation, team_id),
        purpose: granted.purpose,
        trigger,
        resume,
        context,
    }))
}

/// Renews the leases this runner still holds and fences the rest, in one
/// transaction (D31 point 4). `cancel` is empty in process: a solo Cancel
/// reaches `InFlight::cancel` directly rather than through here.
pub async fn heartbeat(
    ctx: &ServiceContext,
    runner: Runner<'_>,
    held: &[LeaseRef],
) -> Result<Heartbeat> {
    lease::heartbeat(ctx, runner.id, runner.term, held).await
}

/// The context re-read under a lease.
///
/// Read over the pool, which a held transaction would block on the
/// one-connection test pool, then fenced in a transaction of its own that
/// re-checks consent on the context it returns (D31 point 6), and ends the
/// lease when consent was lost. A context an edit overtook in between is read
/// again rather than judged.
pub async fn run_context(
    ctx: &ServiceContext,
    runner: Runner<'_>,
    lease: &LeaseRef,
) -> Result<RunContext> {
    fenced_context(ctx, runner, lease, || std::future::ready(())).await
}

/// [`run_context`], awaiting `between` after each read of the context and
/// before the fence's transaction opens: how a test lands an edit in the one
/// window a read over the pool leaves. Nothing in production calls it.
#[cfg(any(test, feature = "testing"))]
pub async fn run_context_with_edit_between<F, Fut>(
    ctx: &ServiceContext,
    runner: Runner<'_>,
    lease: &LeaseRef,
    between: F,
) -> Result<RunContext>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    fenced_context(ctx, runner, lease, between).await
}

/// How many times [`run_context`] reads the context before it gives up on
/// content that changes between every read and its fence. An edit lands in
/// that window only by racing a read that takes milliseconds, so a second
/// read settles any real board; the bound only keeps a pathological one from
/// spinning.
const CONTEXT_READS: usize = 3;

async fn fenced_context<F, Fut>(
    ctx: &ServiceContext,
    runner: Runner<'_>,
    lease: &LeaseRef,
    mut between: F,
) -> Result<RunContext>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let ctx = &lease_context(ctx, lease)?;
    for _ in 0..CONTEXT_READS {
        // Read first, over the pool, because the consent re-check below needs
        // the composition, and the fence's transaction holds the one test
        // connection. A stale lease's read is thrown away by the fence.
        let context = read_context(ctx, runner, &lease.task_id).await?;
        between().await;
        let composition = Composition::of(&context);

        let mut tx = ctx.begin_immediate().await?;
        let held = lease::current(&mut tx, lease, runner.id).await?;
        let run_state = tasks::service::fetch_task_row(&mut tx, &lease.task_id)
            .await?
            .run_state;
        // Consent is judged on the revisions the transaction reads, so it
        // must be the text this read returns. An edit that landed in between
        // makes this read stale: drop it, writing nothing, and read again.
        if !consent::context_is_current(&mut tx, &lease.task_id, &context).await? {
            continue;
        }
        // D31 point 6: a plan edited between the claim and the composition
        // is never composed. The lease really ends, so the runner's reaction
        // to `Conflict` is the right one, and a solo lease is not left
        // `running` with no holder.
        if let Some(refused) = lease::consent_refusal(
            &mut tx,
            &lease.task_id,
            runner.id,
            lease::composes(held.purpose, run_state),
            &composition,
        )
        .await?
        {
            return Err(end_before_composition(ctx, tx, lease, &refused.refusal()).await?);
        }
        tx.commit().await?;
        return Ok(context);
    }

    // Never once current: nothing read can be shown to be what consent
    // judged, so it ends as a lost consent does.
    let mut tx = ctx.begin_immediate().await?;
    lease::current(&mut tx, lease, runner.id).await?;
    Err(end_before_composition(
        ctx,
        tx,
        lease,
        "what it would run changed each time it was read.",
    )
    .await?)
}

/// Ends `lease` in `tx` with `release`'s semantics, commits, and answers the
/// `Conflict` the runner is sent, naming `why`.
async fn end_before_composition(
    ctx: &ServiceContext,
    mut tx: ScopedTx,
    lease: &LeaseRef,
    why: &str,
) -> Result<Error> {
    lease::end_within(ctx, &mut tx, lease).await?;
    let team_id = tasks::team_of_task(&mut tx, &lease.task_id).await?;
    tx.commit().await?;
    ctx.publish(crate::ChangeEvent::tasks(team_id, [lease.task_id.clone()]));
    Ok(Error::conflict(format!(
        "this runner's lease on task {} (generation {}) has ended: {why}",
        lease.task_id, lease.generation,
    )))
}

/// Writes `tasks.branch` and nothing else: the worktree path is the runner's
/// (ADR-0028 point 2). Its production caller is `worktree::prepare` (task 066).
pub async fn record_branch(
    ctx: &ServiceContext,
    runner_id: &str,
    lease: &LeaseRef,
    branch: &str,
) -> Result<()> {
    let ctx = &lease_context(ctx, lease)?;
    let mut tx = ctx.begin_immediate().await?;
    lease::current(&mut tx, lease, runner_id).await?;

    let now = ctx.clock.now();
    let team_id = sqlx::query_scalar!(
        "UPDATE tasks SET branch = ?1, updated_at = ?2 WHERE id = ?3 RETURNING team_id",
        branch,
        now,
        lease.task_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    ctx.publish(crate::ChangeEvent::tasks(team_id, [lease.task_id.clone()]));
    Ok(())
}

/// Opens the `runs` row under the id the runner minted, naming `runner_id`,
/// the runner the adapter serves, and moves the lease onto it: its `run_id`
/// and, as its purpose, the run's kind, under the same generation (task 043).
/// In process the transcript path comes from this board's own `paths` (D31
/// point 4).
pub async fn start_run(
    ctx: &ServiceContext,
    paths: &AppPaths,
    runner_id: &str,
    lease: &LeaseRef,
    run: StartRun,
) -> Result<()> {
    let ctx = &lease_context(ctx, lease)?;
    let mut tx = ctx.begin_immediate().await?;
    lease::current(&mut tx, lease, runner_id).await?;

    let team_id = outcome::insert_run_within(
        &mut tx,
        ctx.clock.as_ref(),
        paths,
        &run.run_id,
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
    lease::open_run(&mut tx, lease, &run.run_id, run.kind).await?;
    tx.commit().await?;

    outcome::publish_opened(ctx, team_id, &run.run_id, &lease.task_id);
    Ok(())
}

/// Acknowledges through the chunk's end and copies nothing: in solo the
/// runner's file *is* the board's copy (ADR-0028 point 4).
pub async fn append_transcript(
    ctx: &ServiceContext,
    runner_id: &str,
    lease: &LeaseRef,
    chunk: TranscriptChunk,
) -> Result<TranscriptAck> {
    let ctx = &lease_context(ctx, lease)?;
    let mut tx = ctx.begin_immediate().await?;
    lease::current(&mut tx, lease, runner_id).await?;
    ensure_run_of_task(&mut tx, &lease.task_id, &chunk.run_id).await?;
    tx.commit().await?;

    Ok(TranscriptAck {
        stored_through: chunk.offset + chunk.bytes.len() as u64,
    })
}

/// Drops a tail for a lease whose team the adapter's scope does not hold:
/// synchronous and infallible, it has no `NotFound` to answer with, and a
/// dropped tail costs nothing (D14).
///
/// **Not fenced.** It is synchronous (D31 point 2) and the fence is an async
/// read, and D14 makes a stale tail worth nothing. Over HTTP the server's
/// handler is async, and task 052 drops a tail there when `lease::current`
/// refuses its lease.
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
/// it is refused. The usage-limit pause is runner-owned state, and the runner
/// raises it from the [`NextStep`](super::NextStep) this answers. The loop's
/// next step is the task-side step's answer, `review_loop::decide`.
///
/// Two transactions, both fenced, with the decision read between them (see
/// `outcome::finish_run_within` for why it cannot be one):
///
/// 1. the fence, then the row closed. Until task 056 a row already closed is
///    refused as "already finalized" after the fence (D31 point 12);
/// 2. the fence again, then the task landed with the lease deleted or kept
///    and the pin set or cleared, in `lease::land`.
pub async fn finish_run(
    ctx: &ServiceContext,
    runner: Runner<'_>,
    lease: &LeaseRef,
    run_id: &str,
    finish: FinishRun,
) -> Result<FinishReceipt> {
    let ctx = &lease_context(ctx, lease)?;
    let runner_id = runner.id;

    let FinishRun {
        mut outcome,
        head_sha,
        bundle,
        window_closes_at,
        // Acknowledged either way in process: the file never left the machine.
        transcript: _,
        ceiling,
    } = finish;

    if outcome.resume_after.is_some() {
        return Err(Error::invalid(
            "a finished run reports what happened, never when to try again: \
             `resume_after` is the board's to decide (ADR-0011)",
        ));
    }

    // Read before the first transaction opens: the attempt history is read
    // over the pool, which a held write lock would block.
    outcome.resume_after =
        decide_resume_after(ctx, &lease.task_id, run_id, &outcome, window_closes_at).await;

    let mut tx = ctx.begin_immediate().await?;
    lease::current(&mut tx, lease, runner_id).await?;
    ensure_run_of_task(&mut tx, &lease.task_id, run_id).await?;
    let run = outcome::close_within(
        &mut tx,
        ctx.clock.as_ref(),
        run_id,
        &outcome,
        &RunCapture { head_sha, bundle },
    )
    .await?;
    let team_id = tasks::team_of_task(&mut tx, &lease.task_id).await?;
    tx.commit().await?;
    outcome::publish_closed(ctx, team_id.clone(), &run);

    let decision = outcome::decide(ctx, &run, &outcome, window_closes_at).await?;

    // What the model rule reads for the next phase, over the pool and before
    // the write lock, as a claim reads it. Only a `Continue` consults it, so a
    // finish that ends the lease reads nothing more.
    let (catalogue, models, composition) = match decision.next {
        NextStep::Continue { kind } => {
            let (catalogue, models) = model_inputs(ctx, runner.provider, &lease.task_id).await?;
            let findings = matches!(kind, RunKind::Review | RunKind::Fix);
            let composition = consent::composition(ctx, &lease.task_id, findings).await?;
            (catalogue, models, composition)
        }
        NextStep::Released { .. } => (
            catalogue::Catalogue::default(),
            PhaseModels::default(),
            Composition::default(),
        ),
    };
    let candidate = Candidate {
        runner_id,
        provider: runner.provider.id(),
        catalogue: &catalogue,
        models: &models,
        ceiling: &ceiling,
        composition: &composition,
    };

    let mut tx = ctx.begin_immediate().await?;
    let (next, rebalanced) =
        lease::land(ctx, &mut tx, &candidate, lease, &run, &outcome, decision).await?;
    tx.commit().await?;
    // A `Continue` lands nothing and keeps the lease, so there is nothing to
    // announce; a `Released` landed the task, its lease and its pin together.
    if let NextStep::Released { .. } = next {
        outcome::publish_landed(ctx, team_id, &lease.task_id, rebalanced);
    }

    Ok(FinishReceipt {
        run: outcome::read_back(ctx, run_id).await?,
        next,
    })
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

/// Ends a claim no `finish_run` ended: a task still `running` becomes
/// `failed`, a verdict already written is kept, and the lease is deleted, in
/// one transaction (`lease::release`).
pub async fn release(ctx: &ServiceContext, runner_id: &str, lease: &LeaseRef) -> Result<()> {
    let ctx = &lease_context(ctx, lease)?;
    lease::release(ctx, runner_id, lease).await
}

pub async fn record_strategy(
    ctx: &ServiceContext,
    runner_id: &str,
    lease: &LeaseRef,
    plan: StrategyPlan,
) -> Result<()> {
    let ctx = &lease_context(ctx, lease)?;
    // The defaults are read over the pool, before the write lock is taken.
    let prepared = prepare_strategy(ctx, &lease.task_id, plan).await?;

    let mut tx = ctx.begin_immediate().await?;
    lease::current(&mut tx, lease, runner_id).await?;
    write_strategy(
        &mut tx,
        ctx.clock.as_ref(),
        &lease.task_id,
        prepared,
        StrategySource::Planner,
    )
    .await?;
    let team_id = tasks::team_of_task(&mut tx, &lease.task_id).await?;
    tx.commit().await?;

    ctx.publish(crate::ChangeEvent::tasks(team_id, [lease.task_id.clone()]));
    Ok(())
}

pub async fn record_review_findings(
    ctx: &ServiceContext,
    runner_id: &str,
    lease: &LeaseRef,
    run_id: &str,
    findings: Vec<NewReviewFinding>,
) -> Result<()> {
    let ctx = &lease_context(ctx, lease)?;
    let mut tx = ctx.begin_immediate().await?;
    lease::current(&mut tx, lease, runner_id).await?;
    ensure_run_of_task(&mut tx, &lease.task_id, run_id).await?;
    findings::record_within(
        &mut tx,
        ctx.clock.as_ref(),
        &lease.task_id,
        run_id,
        findings,
    )
    .await?;
    let team_id = tasks::team_of_task(&mut tx, &lease.task_id).await?;
    tx.commit().await?;

    ctx.publish(crate::ChangeEvent::tasks(team_id, [lease.task_id.clone()]));
    Ok(())
}

/// What a run of `task_id` is composed from and bounded by, read once.
async fn read_context(
    ctx: &ServiceContext,
    runner: Runner<'_>,
    task_id: &str,
) -> Result<RunContext> {
    let provider = runner.provider;
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

    // Decided here, under the context's scope, so the runner creates the
    // worktree from it without reading the dependency graph (D31 point 6).
    let base = base_ref::resolve(ctx, &task.task, &repository).await?;
    let authorship = authorship(ctx, &task.task, runner.id).await?;

    Ok(RunContext {
        task,
        repository,
        base_instructions,
        strategy,
        catalogue,
        limits,
        review: Some(review),
        base,
        authorship,
    })
}

/// ADR-0032 point 7's two facts, from the task's row and the runner's: who
/// wrote the plan revision and whose machine runs it. `None` in a personal
/// team, where every author and the machine's owner are one person and the
/// facts would say nothing.
async fn authorship(
    ctx: &ServiceContext,
    task: &crate::db::Task,
    runner_id: &str,
) -> Result<Option<RunAuthorship>> {
    let row = sqlx::query!(
        r#"SELECT tm.personal_user_id, a.login AS "plan_author?",
                  o.login AS "runner_owner?", r.label AS "runner_label?"
             FROM tasks t
             JOIN teams tm ON tm.id = t.team_id
             LEFT JOIN users a ON a.id = t.plan_updated_by
             LEFT JOIN runners r ON r.id = ?2
             LEFT JOIN users o ON o.id = r.user_id
            WHERE t.id = ?1"#,
        task.id,
        runner_id,
    )
    .fetch_one(&ctx.pool)
    .await?;
    if row.personal_user_id.is_some() {
        return Ok(None);
    }
    let (Some(runner_owner), Some(runner_label)) = (row.runner_owner, row.runner_label) else {
        return Ok(None);
    };
    Ok(Some(RunAuthorship {
        plan_revision: task.plan_revision,
        plan_author: row.plan_author,
        runner_owner,
        runner_label,
    }))
}

/// What the model rule reads about `task_id` for `provider`'s runner: the
/// catalogue the board resolves for that provider in the task's team, and the
/// models the task's phases would spawn with. The same reads
/// [`read_context`] makes for these fields, for a `Continue`, which carries no
/// context (task 067).
async fn model_inputs(
    ctx: &ServiceContext,
    provider: &dyn AgentProvider,
    task_id: &str,
) -> Result<(catalogue::Catalogue, PhaseModels)> {
    let task = tasks::get_task(ctx, task_id).await?;
    let repository_id = task.task.repository_id.as_str();
    let team_id = tasks::service::team_of(ctx, task_id).await?;

    let global = strategy::settings::global_default_for(ctx, &team_id).await?;
    let per_repository = strategy::settings::repository_default(ctx, repository_id).await?;
    let effective = strategy::effective_strategy(&task.task, &per_repository, &global);
    let review = review_loop::config::resolve(ctx, task_id, repository_id).await?;

    Ok((
        catalogue::catalogue_for(ctx, &team_id, provider).await?,
        PhaseModels {
            strategy: effective.model,
            review: review.config.review_model,
            effort: effective.effort,
            review_effort: review.config.review_effort,
        },
    ))
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

/// `NotFound` for a run that does not exist or belongs to another task: the
/// lease bounds what a call may touch, and a run outside it is not there.
/// Inside the fenced transaction, after the fence.
async fn ensure_run_of_task(tx: &mut ScopedTx, task_id: &str, run_id: &str) -> Result<()> {
    let scope = tx.scope().json();
    let owner: Option<String> = sqlx::query_scalar!(
        "SELECT r.task_id FROM runs r JOIN tasks t ON t.id = r.task_id
          WHERE r.id = ?1 AND t.team_id IN (SELECT value FROM json_each(?2))",
        run_id,
        scope,
    )
    .fetch_optional(&mut **tx)
    .await?;
    match owner {
        Some(owner) if owner == task_id => Ok(()),
        Some(_) => Err(Error::not_found(format!(
            "run {run_id} is not a run of task {task_id}"
        ))),
        None => Err(Error::not_found(format!("no run with id {run_id}"))),
    }
}
