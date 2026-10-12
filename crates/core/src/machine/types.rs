//! The rows a machine store holds besides settings and schedules.
//!
//! [`Schedule`](crate::db::Schedule) keeps its type in `db::models`: the
//! table moved whole (ADR-0031 point 6), and its rules in [`crate::schedule`]
//! did not move with it.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::board::{LeasePurpose, LeaseRef};
use crate::db::OnArchive;
use crate::events::TeamId;
use crate::tasks::Patch;

/// One row of `checkouts`: this machine's clone of one board repository
/// (ADR-0028 point 2, ADR-0033 point 2).
///
/// Keyed by the board's repository id, with no foreign key into the board,
/// which is another file in solo and another machine in team mode. Since task
/// 066 this is the only copy any reader reads: the board's retired columns are
/// read by nothing but adoption, until task 065 drops them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkout {
    pub repository_id: String,
    /// Where the clone is on this machine.
    pub path: String,
    /// Where this machine creates the repository's worktrees.
    pub worktree_root: String,
    /// How many runs this machine holds in the repository at once (ADR-0010).
    pub max_concurrency: i64,
    /// ADR-0032 point 4's runner consent to unattended runs: what the board's
    /// `allow_unattended_runs` was until it became a team ceiling.
    pub unattended_consent: bool,
    pub on_archive: OnArchive,
    pub on_archive_script: Option<String>,
    pub credential_login: Option<String>,
    pub credential_label: Option<String>,
    pub credential_added_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// A change to a [`Checkout`]. Only a field set to something other than unset
/// changes; `repository_id` and `created_at` never do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckoutPatch {
    pub path: Option<String>,
    pub worktree_root: Option<String>,
    pub max_concurrency: Option<i64>,
    pub unattended_consent: Option<bool>,
    pub on_archive: Option<OnArchive>,
    pub on_archive_script: Patch<String>,
    pub credential_login: Patch<String>,
    pub credential_label: Patch<String>,
    pub credential_added_at: Patch<DateTime<Utc>>,
}

impl CheckoutPatch {
    /// `checkout` with every field this patch sets applied, so both stores
    /// share one reading of "unset".
    pub fn applied_to(&self, checkout: &Checkout) -> Checkout {
        let patch = self.clone();
        Checkout {
            repository_id: checkout.repository_id.clone(),
            path: patch.path.unwrap_or_else(|| checkout.path.clone()),
            worktree_root: patch
                .worktree_root
                .unwrap_or_else(|| checkout.worktree_root.clone()),
            max_concurrency: patch.max_concurrency.unwrap_or(checkout.max_concurrency),
            unattended_consent: patch
                .unattended_consent
                .unwrap_or(checkout.unattended_consent),
            on_archive: patch.on_archive.unwrap_or(checkout.on_archive),
            on_archive_script: patch
                .on_archive_script
                .apply(checkout.on_archive_script.clone()),
            credential_login: patch
                .credential_login
                .apply(checkout.credential_login.clone()),
            credential_label: patch
                .credential_label
                .apply(checkout.credential_label.clone()),
            credential_added_at: patch
                .credential_added_at
                .apply(checkout.credential_added_at),
            created_at: checkout.created_at,
        }
    }
}

/// One row of `worktrees`: where a task's work lives on this machine
/// (ADR-0033 point 3). The path never crosses the board port (D31 point 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeRecord {
    pub task_id: String,
    /// The checkout the worktree was made from, on this machine.
    pub repository_id: String,
    pub path: String,
    /// ADR-0031 point 4's fence after "run elsewhere" (task 057): the worktree
    /// is kept and never pushed from.
    pub fenced_at: Option<DateTime<Utc>>,
}

/// One row of `held_leases`: a lease this runner holds on the board, so its
/// startup reconciles only its own (ADR-0031 point 5) and a heartbeat can name
/// each one as a [`LeaseRef`] (seam-contract D28).
///
/// A record of what the board granted, never an authority: the board's
/// `runner_leases` row decides, and a record the board no longer agrees with
/// is answered `Conflict` or `NotFound` and dropped (task 043).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldLease {
    pub task_id: String,
    /// The task's team, which the runner store cannot join for.
    pub team_id: TeamId,
    pub purpose: LeasePurpose,
    /// The run the lease is open for, once `start_run` has reported it.
    pub run_id: Option<String>,
    pub generation: i64,
    pub acquired_at: DateTime<Utc>,
}

impl HeldLease {
    /// The reference every report under this lease carries.
    pub fn lease(&self) -> LeaseRef {
        LeaseRef::new(self.task_id.clone(), self.generation, self.team_id.clone())
    }
}
