//! Run now, Retry now and Cancel, for one runner (ADR-0031 point 7, task 052,
//! seam-contract D32's appendix rows `start_task_run`, `retry_task_now` and
//! `cancel_task_run`).
//!
//! A person asks the board to start a task on one runner of theirs, or to stop
//! a run wherever it is. How the request reaches the runner depends on where
//! the board is, so it goes through one trait, [`RunnerRelay`], held on the
//! board host:
//!
//! - **Solo** relays through the shell (`rimaia_runner::relay::ShellRelay`):
//!   the board and its one runner are one process, so Run now runs the
//!   starter there and then, and answers with its refusals synchronously, as
//!   it always did.
//! - **A server** relays through [`RelayedRequests`]: a Run now is held for
//!   the runner named, after the same checks the claim would make, and the
//!   runner's next `claim(Next)` collects it ([`claim_next`]); a Cancel is
//!   held in [`CancelRequests`] and listed on the holding runner's heartbeats.
//!
//! **Both stores live in memory, on purpose.** Each is a button press that a
//! runner online at that moment collects within seconds, or within one
//! heartbeat. If the server restarts first, the press is lost, the card is
//! unchanged, and the person presses again. A persisted Run now would start
//! work hours later, on a machine whose owner has since moved on. D4 reserves
//! no migration for this.

use std::collections::BTreeMap;
use std::sync::Mutex;

use chrono::{DateTime, Utc};

use crate::api::Door;
use crate::context::ServiceContext;
use crate::error::{Error, ErrorCode, Result};
use crate::identity;
use crate::runner::RunTrigger;
use crate::tasks;

use super::cancel::CancelRequests;
use super::lease::{self, LEASE_LIFETIME};
use super::port::BoardFuture;
use super::runners;
use super::service::{self, authorize_start, OwnerPresence, Runner};
use super::types::{Claim, ClaimTarget, LeaseRef};

/// What a person asked one runner to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunNowRequest {
    pub task_id: String,
    pub team_id: String,
    pub runner_id: String,
    /// Retry now: continue the session the task is waiting to resume.
    pub continue_session: bool,
    /// The person who asked: `users.id`.
    pub requested_by: String,
}

/// How a Run now or a Cancel reaches the runner it is for.
pub trait RunnerRelay: Send + Sync + 'static {
    /// Start `request.task_id` on `request.runner_id`, or refuse with a
    /// sentence.
    fn run_now<'a>(
        &'a self,
        ctx: &'a ServiceContext,
        request: RunNowRequest,
    ) -> BoardFuture<'a, ()>;

    /// Stop `task_id`'s run. `lease` is its current lease, if it holds one.
    /// Idempotent.
    fn cancel<'a>(
        &'a self,
        ctx: &'a ServiceContext,
        task_id: &'a str,
        lease: Option<LeaseRef>,
    ) -> BoardFuture<'a, ()>;
}

/// The refusal when a Run now names no runner and the task has no pin to
/// default to.
pub const NAME_THE_RUNNER: &str = "name the runner to start this task on";

/// The refusal when the named runner has not been heard from within one
/// [`LEASE_LIFETIME`]: a request it will not collect would answer `Ok` and do
/// nothing.
pub const RUNNER_OFFLINE: &str =
    "this runner has not been in touch for more than three minutes, so it cannot start a run now";

/// What the person asked of the board: which task, on which runner if they
/// named one, and whether it continues the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
    pub task_id: String,
    pub runner_id: Option<String>,
    pub continue_session: bool,
}

/// Run now and Retry now, as board commands: the task read in the caller's
/// scope, the runner it is for, whether the caller may ask, then the relay.
///
/// - **Which runner.** Through [`Door::Shell`] it is the solo runner, so a
///   solo invoke stays `{ taskId }`. Through any other door it is the runner
///   the request names, or, for a retry that names none, the task's pin,
///   since a retry is a claim through the pin (D32's appendix). Otherwise it
///   is refused with [`NAME_THE_RUNNER`].
/// - **Who may start.** Every door but the shell asks [`authorize_start`]
///   with [`OwnerPresence::Remote`]: a runner outside the caller's teams is
///   `not_found`, an unpaired runner and one the caller does not own are
///   refused in task 067's words. The shell does not ask here: its relay's
///   starter asks with [`OwnerPresence::AtRunner`].
pub async fn request_start(
    ctx: &ServiceContext,
    door: &Door,
    relay: &dyn RunnerRelay,
    start: StartRequest,
) -> Result<()> {
    let team_id = tasks::service::team_of(ctx, &start.task_id).await?;
    let runner_id = match door {
        Door::Shell => solo_runner(ctx).await?,
        _ => {
            let runner_id = match start.runner_id {
                Some(runner_id) => Some(runner_id),
                None if start.continue_session => {
                    lease::state_of(ctx, &start.task_id).await?.pinned_runner_id
                }
                None => None,
            }
            .ok_or_else(|| Error::invalid(NAME_THE_RUNNER))?;
            authorize_start(ctx, &runner_id, OwnerPresence::Remote).await?;
            runner_id
        }
    };
    relay
        .run_now(
            ctx,
            RunNowRequest {
                task_id: start.task_id,
                team_id,
                runner_id,
                continue_session: start.continue_session,
                requested_by: ctx.actor.clone(),
            },
        )
        .await
}

/// The installation's own runner, which a solo window's buttons start on.
async fn solo_runner(ctx: &ServiceContext) -> Result<String> {
    identity::solo_identity(ctx)
        .await?
        .map(|solo| solo.runner_id)
        .ok_or_else(|| {
            Error::internal("a request through the desktop window found no solo identity")
        })
}

/// One Run now the server holds for a runner.
#[derive(Debug, Clone)]
struct Pending {
    runner_id: String,
    continue_session: bool,
    requested_by: String,
    requested_at: DateTime<Utc>,
}

/// The server's relay: pending Run nows in memory, one per task, and the
/// pending cancels.
#[derive(Debug, Default)]
pub struct RelayedRequests {
    /// Keyed by task, so a second press replaces the first rather than adding
    /// a second request. Each names its runner.
    pending: Mutex<BTreeMap<String, Pending>>,
    cancels: CancelRequests,
}

impl RelayedRequests {
    pub fn new() -> Self {
        Self::default()
    }

    /// The cancels `board::service::heartbeat` lists for a runner.
    pub fn cancels(&self) -> &CancelRequests {
        &self.cancels
    }

    fn record(&self, task_id: String, pending: Pending) {
        self.lock().insert(task_id, pending);
    }

    /// `runner_id`'s oldest request that has not expired, taken out of the
    /// store. Every request older than one [`LEASE_LIFETIME`] is dropped on the
    /// way, whoever it was for: a press no claim collected in that time was
    /// for a runner that is not coming.
    fn take_oldest(&self, runner_id: &str, now: DateTime<Utc>) -> Option<(String, Pending)> {
        let lifetime = chrono::Duration::from_std(LEASE_LIFETIME).unwrap_or(chrono::Duration::MAX);
        let mut pending = self.lock();
        pending.retain(|task_id, request| {
            let live = now.signed_duration_since(request.requested_at) <= lifetime;
            if !live {
                tracing::debug!(
                    %task_id,
                    runner_id = %request.runner_id,
                    "a Run now no claim collected within a lease lifetime was dropped",
                );
            }
            live
        });
        let task_id = pending
            .iter()
            .filter(|(_, request)| request.runner_id == runner_id)
            .min_by_key(|(task_id, request)| (request.requested_at, (*task_id).clone()))
            .map(|(task_id, _)| task_id.clone())?;
        pending.remove_entry(&task_id)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Pending>> {
        // Every operation is one insert, removal or retain: a panic cannot
        // leave the map half-written.
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl RunnerRelay for RelayedRequests {
    /// Refuses an offline runner, then what the claim would refuse, in the
    /// claim's own words ([`service::check_run_now`]), and otherwise records
    /// one request for the task, replacing any earlier one.
    fn run_now<'a>(
        &'a self,
        ctx: &'a ServiceContext,
        request: RunNowRequest,
    ) -> BoardFuture<'a, ()> {
        Box::pin(async move {
            if !runners::is_online(ctx, &request.runner_id).await? {
                return Err(Error::invalid(RUNNER_OFFLINE));
            }
            service::check_run_now(
                ctx,
                &request.runner_id,
                &request.task_id,
                request.continue_session,
            )
            .await?;
            tracing::debug!(
                task_id = %request.task_id,
                team_id = %request.team_id,
                runner_id = %request.runner_id,
                "a Run now is held for its runner's next claim",
            );
            self.record(
                request.task_id,
                Pending {
                    runner_id: request.runner_id,
                    continue_session: request.continue_session,
                    requested_by: request.requested_by,
                    requested_at: ctx.clock.now(),
                },
            );
            Ok(())
        })
    }

    /// With no lease there is nothing running to stop, which is today's
    /// idempotence. With one, the holding runner's heartbeats list it.
    fn cancel<'a>(
        &'a self,
        _ctx: &'a ServiceContext,
        _task_id: &'a str,
        lease: Option<LeaseRef>,
    ) -> BoardFuture<'a, ()> {
        Box::pin(async move {
            if let Some(lease) = lease {
                self.cancels.request(&lease.task_id, lease.generation);
            }
            Ok(())
        })
    }
}

/// `claim` as the runner protocol serves it: for `Next`, this runner's oldest
/// relayed Run now first, then selection as usual.
///
/// The request is claimed through the ordinary `claim(Run { trigger: Queued,
/// continue_session })` path, in one transaction, with every check that path
/// applies. If it is lost or refused, which after `run_now`'s check means a
/// race since the press, the request is dropped and selection proceeds.
///
/// - **Capacity does not apply** to the relayed request (D19 point 5): a
///   `Next` with no free capacity is answered with one if there is one, and
///   with `None` otherwise, since selection never picks past zero capacity.
///   That is how a busy runner still hears a Run now.
/// - **A relayed Run now is unattended**: the person who pressed it is not at
///   the runner, so the claim carries [`RunTrigger::Queued`], the posture
///   `authorize_start` answers for [`OwnerPresence::Remote`].
///
/// Every other target is the service's claim, unchanged.
pub async fn claim_next(
    ctx: &ServiceContext,
    runner: Runner<'_>,
    relayed: &RelayedRequests,
    target: ClaimTarget,
) -> Result<Option<Claim>> {
    if let ClaimTarget::Next { ceiling, .. } = &target {
        if let Some((task_id, request)) = relayed.take_oldest(runner.id, ctx.clock.now()) {
            let relayed_target = ClaimTarget::Run {
                task_id: task_id.clone(),
                trigger: RunTrigger::Queued,
                continue_session: request.continue_session,
                ceiling: ceiling.clone(),
            };
            match service::claim(ctx, runner, relayed_target).await {
                Ok(Some(claim)) => return Ok(Some(claim)),
                Ok(None) => tracing::debug!(
                    %task_id,
                    requested_by = %request.requested_by,
                    "a relayed Run now was lost to another starter; it is dropped",
                ),
                Err(error)
                    if matches!(
                        error.code(),
                        ErrorCode::Invalid | ErrorCode::NotFound | ErrorCode::Conflict
                    ) =>
                {
                    tracing::debug!(
                        %task_id,
                        requested_by = %request.requested_by,
                        %error,
                        "a relayed Run now is no longer this runner's to claim; it is dropped",
                    );
                }
                Err(error) => return Err(error),
            }
        }
    }
    service::claim(ctx, runner, target).await
}
