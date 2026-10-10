//! This machine's checkouts and worktree records, read and written the way
//! every caller needs them (task 066, ADR-0033 points 2 and 3).
//!
//! [`MachineStore`](super::MachineStore) is storage only. What a missing
//! checkout means to a reader, which writes announce what, and which
//! repositories this runner offers to the queue are rules, and they live here,
//! once, so the Tauri commands, the MCP tools, the queue and the services all
//! give the same answer.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::db::{OnArchive, Repository};
use crate::error::{Error, Result};
use crate::events::ChangeEvent;

use super::types::{Checkout, CheckoutPatch, WorktreeRecord};
use super::MachineContext;

/// What the window and the operator's MCP client may know about one checkout:
/// [`Checkout`] without the credential metadata, which has its own pane and its
/// own read (`get_repository_credential_status`), and without `created_at`.
///
/// A local DTO. It carries an absolute path on purpose, because it describes
/// this machine to this machine's own window; no board DTO does (task 066).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutView {
    pub repository_id: String,
    pub path: String,
    pub worktree_root: String,
    pub max_concurrency: i64,
    pub unattended_consent: bool,
    pub on_archive: OnArchive,
    pub on_archive_script: Option<String>,
}

impl From<Checkout> for CheckoutView {
    fn from(checkout: Checkout) -> Self {
        Self {
            repository_id: checkout.repository_id,
            path: checkout.path,
            worktree_root: checkout.worktree_root,
            max_concurrency: checkout.max_concurrency,
            unattended_consent: checkout.unattended_consent,
            on_archive: checkout.on_archive,
            on_archive_script: checkout.on_archive_script,
        }
    }
}

/// The refusal every reader gives a board repository this machine has no
/// clone of (ADR-0033 point 2). A legitimate state rather than a corruption:
/// in team mode most repositories on a board are not on any one machine.
pub fn not_set_up(repository_name: &str) -> Error {
    Error::invalid(format!(
        "\"{repository_name}\" is not set up on this computer"
    ))
}

/// This machine's checkout of `repository`, or [`not_set_up`].
pub async fn checkout_of(machine: &MachineContext, repository: &Repository) -> Result<Checkout> {
    machine
        .store
        .get_checkout(&repository.id)
        .await?
        .ok_or_else(|| not_set_up(&repository.name))
}

/// Every repository this runner consented to run unattended in (ADR-0032
/// point 4): the set the queue offers, read once per pass.
///
/// The runner's consent and nothing else. The board's `allow_unattended_runs`
/// is the team ceiling, and its check on the claim is task 045's; reading it
/// here would refuse every repository registered from task 066 on, whose
/// ceiling is the board default `0`. Task 042 sends this same set as
/// `ClaimTarget::Next.repositories`.
pub async fn consented_repositories(machine: &MachineContext) -> Result<BTreeSet<String>> {
    Ok(machine
        .store
        .list_checkouts()
        .await?
        .into_iter()
        .filter(|checkout| checkout.unattended_consent)
        .map(|checkout| checkout.repository_id)
        .collect())
}

/// Inserts a checkout and announces it.
pub async fn insert_checkout(machine: &MachineContext, checkout: &Checkout) -> Result<()> {
    machine.store.insert_checkout(checkout).await?;
    announce_checkout(machine, &checkout.repository_id);
    Ok(())
}

/// Applies `patch` to `repository`'s checkout, announces it and returns the
/// result, or [`not_set_up`] when there is none.
pub async fn patch_checkout(
    machine: &MachineContext,
    repository: &Repository,
    patch: &CheckoutPatch,
) -> Result<Checkout> {
    if !machine.store.patch_checkout(&repository.id, patch).await? {
        return Err(not_set_up(&repository.name));
    }
    announce_checkout(machine, &repository.id);
    checkout_of(machine, repository).await
}

/// Removes a repository's checkout and announces it when there was one.
pub async fn remove_checkout(machine: &MachineContext, repository_id: &str) -> Result<()> {
    if machine.store.remove_checkout(repository_id).await? {
        announce_checkout(machine, repository_id);
    }
    Ok(())
}

/// Where `task_id`'s worktree is on this machine, if it has one.
pub async fn worktree_path(machine: &MachineContext, task_id: &str) -> Result<Option<String>> {
    Ok(machine
        .store
        .get_worktree(task_id)
        .await?
        .map(|record| record.path))
}

/// Records where a task's worktree is, and announces the task.
pub async fn record_worktree(machine: &MachineContext, record: &WorktreeRecord) -> Result<()> {
    machine.store.record_worktree(record).await?;
    announce_worktree(machine, &record.task_id);
    Ok(())
}

/// Forgets a task's worktree, and announces the task when there was one.
pub async fn forget_worktree(machine: &MachineContext, task_id: &str) -> Result<()> {
    if machine.store.forget_worktree(task_id).await? {
        announce_worktree(machine, task_id);
    }
    Ok(())
}

/// A checkout write, as the window has always heard a repository change.
fn announce_checkout(machine: &MachineContext, repository_id: &str) {
    // Rides the board's channel under `event_team` until task 048 turns it into
    // `LocalChange::Checkouts`, which keeps this wire name.
    machine.publish(|team| ChangeEvent::repositories(team, [repository_id.to_string()]));
}

/// A worktree-record write, as the window has always heard a task change.
fn announce_worktree(machine: &MachineContext, task_id: &str) {
    // Task 048 turns this into `LocalChange::Worktrees`, keeping this wire name.
    machine.publish(|team| ChangeEvent::tasks(team, [task_id.to_string()]));
}
