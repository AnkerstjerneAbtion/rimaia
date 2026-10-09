//! A manual start's preflight: everything Run now and Retry now settle before
//! a process is spawned (seam-contract D31 point 5).
//!
//! It is a rule, so it lives here and not in the shell (ADR-0006). The two
//! commands call [`claim_manual_start`] and spawn [`run_task`](super::run_task)
//! with what it returns; nothing about the order below is theirs to get wrong.

use crate::board::{BoardPort, Claim, ClaimTarget};
use crate::context::ServiceContext;
use crate::db::settings;
use crate::error::{Error, Result};
use crate::paths::AppPaths;
use crate::repo;
use crate::scheduler::{InFlight, Lease, LeaseOwner};

use super::process::{implementation_intent, probe_cli, session_intent, RunTrigger, RunnerConfig};
use super::provider;

/// The conversation id the preflight negotiates with, before the claim says
/// which one the run continues. [`negotiate`](provider::negotiate) judges
/// whether a session is opened or continued, never which session it is.
const NOT_YET_CLAIMED: &str = "not-yet-claimed";

/// What a manual start asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualStart {
    pub task_id: String,
    /// Always [`RunTrigger::Manual`] from a button. A parameter so ADR-0026's
    /// "an unattended refusal writes nothing" keeps a caller that can assert
    /// it, although no production caller starts a queued run through here.
    pub trigger: RunTrigger,
    /// `false` for Run now, `true` for Retry now.
    pub continue_session: bool,
}

/// A start the board granted, with D19's slot held for it.
///
/// Drop the slot only once the run it was taken for has finished: it is what
/// a second click, the queue and Plan now all fail against.
pub struct Started {
    pub slot: Lease,
    pub claim: Claim,
}

/// Previews, takes the slot, checks the opt-in, negotiates, probes the CLI,
/// then claims — and only the claim writes anything.
///
/// So every refusal before it leaves the task exactly as it was (task 008's
/// "refused before any run state is written", ADR-0026's for a provider that
/// cannot honour an unattended run). A claim lost to another starter is
/// answered in the sentence each button has always given. Task 035's refusal
/// of a review or fix waiting to be resumed arrives as the claim's error and
/// is returned as it is.
pub async fn claim_manual_start(
    board: &dyn BoardPort,
    ctx: &ServiceContext,
    paths: &AppPaths,
    config: &RunnerConfig,
    in_flight: &InFlight,
    start: ManualStart,
) -> Result<Started> {
    let preview = board.preview(&start.task_id).await?;

    // `acquire_unbounded`, not `acquire`: the concurrency caps bound what the
    // *scheduler* starts, and a person clicking Run now with the app in front
    // of them is not the mis-set-configuration failure those settings exist
    // for. The per-task exclusion and the absolute ceiling still apply.
    let slot = in_flight
        .acquire_unbounded(&start.task_id, &preview.repository.id, LeaseOwner::Manual)
        .map_err(|refused| Error::invalid(refused.message()))?;

    repo::ensure_unattended_runs_allowed(&preview.repository)?;

    let run_environment = settings::run_environment(&ctx.pool).await?;
    let home = paths.provider_home(config.provider.id(), &start.task_id);
    let intent = implementation_intent(
        &preview,
        config,
        start.trigger,
        run_environment,
        session_intent(start.continue_session, NOT_YET_CLAIMED, &home),
    );
    provider::negotiate(config.provider.capabilities(), &intent)?;

    probe_cli(config.provider.as_ref(), &config.program).await?;

    let claim = board
        .claim(ClaimTarget::Run {
            task_id: start.task_id.clone(),
            trigger: start.trigger,
            continue_session: start.continue_session,
        })
        .await?
        .ok_or_else(|| Error::invalid(lost_start(start.continue_session)))?;

    Ok(Started { slot, claim })
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
