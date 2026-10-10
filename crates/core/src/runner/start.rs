//! A manual start's preflight: everything Run now and Retry now settle before
//! a process is spawned (seam-contract D31 point 5).
//!
//! It is a rule, so it lives here and not in the shell (ADR-0006). The two
//! commands call [`claim_manual_start`] and spawn [`run_task`](super::run_task)
//! with what it returns; nothing about the order below is theirs to get wrong.
//!
//! Run now, Retry now and Plan now each name a runner, and only its owner may
//! ask (ADR-0031 point 7): a [`Starter`] says who is asking, of which runner,
//! and from where, and both this preflight and
//! [`claim_for_planning`](super::strategy::claim_for_planning) hand it to
//! [`authorize_start`] before anything else.

use crate::board::{authorize_start, BoardPort, Claim, ClaimTarget, OwnerPresence};
use crate::context::ServiceContext;
use crate::db::settings;
use crate::error::{Error, Result};
use crate::machine::{leases, MachineContext};
use crate::paths::AppPaths;
use crate::repo;
use crate::scheduler::{InFlight, LocalSlot, SlotOwner};

use super::process::{implementation_intent, probe_cli, session_intent, RunTrigger, RunnerConfig};
use super::provider;

/// The conversation id the preflight negotiates with, before the claim says
/// which one the run continues. [`negotiate`](provider::negotiate) judges
/// whether a session is opened or continued, never which session it is.
const NOT_YET_CLAIMED: &str = "not-yet-claimed";

/// Who asks a runner to start a process, which runner, and whether they are
/// at it (ADR-0031 point 7): what every start door hands
/// [`authorize_start`].
#[derive(Debug, Clone, Copy)]
pub struct Starter<'a> {
    /// The caller's own context, whose `actor` is the person asking. Never the
    /// board adapter's `System` one.
    pub ctx: &'a ServiceContext,
    /// The runner the start is for: the one the door's board port serves.
    pub runner_id: &'a str,
    /// Decided by the door, never by a request field.
    pub presence: OwnerPresence,
}

impl<'a> Starter<'a> {
    /// A door on the runner's own machine: each desktop command and each tool
    /// of the loopback operator MCP server. All of them are
    /// [`OwnerPresence::AtRunner`].
    pub fn at_runner(ctx: &'a ServiceContext, runner_id: &'a str) -> Self {
        Self {
            ctx,
            runner_id,
            presence: OwnerPresence::AtRunner,
        }
    }

    /// [`authorize_start`] for this caller: the permission posture the start
    /// takes, or why it may not happen. Writes nothing.
    pub async fn authorize(&self) -> Result<RunTrigger> {
        authorize_start(self.ctx, self.runner_id, self.presence).await
    }
}

/// What a manual start asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualStart {
    pub task_id: String,
    /// `false` for Run now, `true` for Retry now.
    pub continue_session: bool,
}

/// A start the board granted, with D19's slot held for it.
///
/// Drop the slot only once the run it was taken for has finished: it is what
/// a second click, the queue and Plan now all fail against.
pub struct Started {
    pub slot: LocalSlot,
    pub claim: Claim,
}

/// Authorizes the starter, previews, takes the slot, checks the opt-in,
/// negotiates, probes the CLI, then claims — and only the claim writes
/// anything — and records the lease in this runner's own store before it
/// hands the claim back.
///
/// [`Starter::authorize`] decides the permission posture: `acceptEdits` for an
/// owner at the runner, an unattended run for one who is not, negotiated and
/// claimed as such. Capacity is not consulted (D19 point 5): the slot is
/// unbounded and the claim carries no `FreeCapacity`.
///
/// So every refusal before it leaves the task exactly as it was (task 008's
/// "refused before any run state is written", ADR-0026's for a provider that
/// cannot honour an unattended run). A claim lost to another starter is
/// answered in the sentence each button has always given. A review or fix
/// waiting to be resumed is claimed like any retry, and `run_task` enters the
/// loop at that kind (task 021).
///
/// The run environment it negotiates with is this machine's, from `machine`.
pub async fn claim_manual_start(
    starter: Starter<'_>,
    board: &dyn BoardPort,
    machine: &MachineContext,
    paths: &AppPaths,
    config: &RunnerConfig,
    in_flight: &InFlight,
    start: ManualStart,
) -> Result<Started> {
    let trigger = starter.authorize().await?;
    let preview = board.preview(&start.task_id).await?;

    // `acquire_unbounded`, not `acquire`: the concurrency caps bound what the
    // *scheduler* starts, and a start its owner named is not the
    // mis-set-configuration failure those settings exist for, at the machine
    // or not (D19 point 5, ADR-0031 point 7). The per-task exclusion and the
    // absolute ceiling still apply.
    let slot = in_flight
        .acquire_unbounded(&start.task_id, &preview.repository.id, SlotOwner::Manual)
        .map_err(|refused| Error::invalid(refused.message()))?;

    // This runner's consent, from its checkout; a repository with no checkout
    // here is refused as not set up on this computer. Either way before the
    // claim, so nothing is written (task 066).
    repo::ensure_unattended_runs_allowed(machine, &preview.repository).await?;

    let run_environment = settings::run_environment(machine).await?;
    // The same two halves a queued run is held to, so a manual run is
    // negotiated against the limits `run_task` will spawn it with.
    let runner_limits = super::limits::runner_limits(machine).await?;
    let home = paths.provider_home(config.provider.id(), &start.task_id);
    let intent = implementation_intent(
        &preview,
        &runner_limits,
        config,
        trigger,
        run_environment,
        session_intent(start.continue_session, NOT_YET_CLAIMED, &home),
    );
    provider::negotiate(config.provider.capabilities(), &intent)?;

    probe_cli(config.provider.as_ref(), &config.program).await?;

    let claim = board
        .claim(ClaimTarget::Run {
            task_id: start.task_id.clone(),
            trigger,
            continue_session: start.continue_session,
        })
        .await?
        .ok_or_else(|| Error::invalid(lost_start(start.continue_session)))?;

    // Recorded before anything is spawned, so a crash from here on is this
    // runner's to reconcile at its next launch (ADR-0031 point 5).
    record_claim(board, machine, &claim).await?;

    Ok(Started { slot, claim })
}

/// Notes the lease `claim` granted in this runner's own store, or gives the
/// claim back and says why not.
///
/// Every starter calls this after its claim returns and before it spawns
/// anything (task 043). A claim the runner could not note is released rather
/// than run: a run whose lease this runner has no record of would be left for
/// the solo arm of startup reconciliation to find, and a run this runner cannot
/// account for is not worth starting.
pub async fn record_claim(
    board: &dyn BoardPort,
    machine: &MachineContext,
    claim: &Claim,
) -> Result<()> {
    if let Err(error) = leases::record(machine, claim).await {
        if let Err(released) = board.release(&claim.lease).await {
            tracing::error!(task_id = %claim.lease.task_id, %released, "could not release a claim this runner could not record");
        }
        return Err(error);
    }
    Ok(())
}

/// The sentence a person reads when another starter got there first.
fn lost_start(continue_session: bool) -> &'static str {
    if continue_session {
        "this task is not waiting to be retried; the queue may have already picked it up, \
         or its retries may have run out"
    } else {
        "the run queue is already working on this task; pause or stop the queue, \
         or wait for it to finish, before starting it by hand"
    }
}
