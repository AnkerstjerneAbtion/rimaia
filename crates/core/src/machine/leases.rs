//! The leases this runner holds, as it recorded them (ADR-0031 point 5, task
//! 043).
//!
//! The board's `runner_leases` row is the authority on who holds a task. This
//! is the runner's own note of what the board granted it, kept for one reason:
//! after a crash, startup reconciles exactly the leases this runner held
//! (`scheduler::reconcile::reconcile_held`) and never another runner's. Two
//! stores cannot share a transaction, so a note can lag the board in both
//! directions, and both are harmless by construction:
//!
//! - **A note the board no longer agrees with** (a forget that was missed) is
//!   answered `Conflict` or `NotFound` at the next reconcile, which then drops
//!   it without touching the board.
//! - **A lease the board granted that was never noted** (a crash between the
//!   claim's commit and [`record`]) is the solo arm's to find:
//!   `reconcile_unrecorded` reads the board's leases for this runner.
//!
//! Every starter records after its claim returns and before anything is
//! spawned: the manual starter, the planner's claim and the runner loop.
//! `run_task` notes the run once `start_run` has reported it, and forgets the
//! lease when the board ends it.

use crate::board::{Claim, LeasePurpose};
use crate::error::{ErrorCode, Result};

use super::context::MachineContext;
use super::types::HeldLease;

/// Notes the lease `claim` granted.
pub async fn record(machine: &MachineContext, claim: &Claim) -> Result<()> {
    machine
        .store
        .record_held_lease(&HeldLease {
            task_id: claim.lease.task_id.clone(),
            team_id: claim.lease.team_id.clone(),
            purpose: claim.purpose,
            run_id: None,
            generation: claim.lease.generation,
            acquired_at: machine.clock.now(),
        })
        .await
}

/// Notes the run a held lease is now open for, and the purpose the board
/// moved it to: the run's kind after a `start_run`, or the next phase's kind
/// with no run after a `Continue`, so a crash between phases is released
/// rather than finished again.
///
/// Best effort. A note that could not be written costs a crash its precise
/// reconcile (the lease is released rather than its run closed), never the run.
pub async fn note_run(
    machine: &MachineContext,
    task_id: &str,
    run_id: Option<&str>,
    purpose: LeasePurpose,
) {
    if let Err(error) = machine
        .store
        .set_held_lease_run(task_id, run_id, purpose)
        .await
    {
        tracing::warn!(%task_id, ?run_id, %error, "could not note a held lease's run");
    }
}

/// Forgets a lease the board has ended. Best effort, for [`note_run`]'s
/// reason: a note left behind is dropped by the next reconcile.
pub async fn forget(machine: &MachineContext, task_id: &str) {
    if let Err(error) = machine.store.forget_held_lease(task_id).await {
        tracing::warn!(%task_id, %error, "could not forget a held lease");
    }
}

/// Forgets a lease once its release was reported, unless the report failed in
/// a way that leaves the board's lease standing.
///
/// A release that succeeded ended the lease; one answered `Conflict` or
/// `NotFound` found no such lease to end. Either way there is nothing left to
/// reconcile. Any other failure keeps the record, so the next launch settles
/// it.
pub async fn forget_released(machine: &MachineContext, task_id: &str, released: &Result<()>) {
    let ended = match released {
        Ok(()) => true,
        Err(error) => matches!(error.code(), ErrorCode::Conflict | ErrorCode::NotFound),
    };
    if ended {
        forget(machine, task_id).await;
    }
}

/// Every lease this runner noted, ordered by task id.
pub async fn held(machine: &MachineContext) -> Result<Vec<HeldLease>> {
    machine.store.list_held_leases().await
}
