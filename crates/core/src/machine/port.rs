//! The machine store: the storage half of every fact one runner keeps for
//! itself (ADR-0028 point 2, seam-contract D31's 2026-10-10 amendment).

use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};

use crate::db::Schedule;
use crate::error::Result;

use crate::board::LeasePurpose;

use super::types::{Checkout, CheckoutPatch, HeldLease, WorktreeRecord};

/// What every store method returns.
///
/// A boxed future rather than an `async fn`, for the reason
/// [`BoardFuture`](crate::board::BoardFuture) gives: the trait stays
/// object-safe without an `async-trait` dependency, which D6 forbids and D34
/// does not approve.
pub type MachineFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Where one machine's own state is stored. Held as `Arc<dyn MachineStore>`.
///
/// **Storage only.** Every method reads or writes exactly what it names and
/// decides nothing: what an absent key means, what a value parses to, what a
/// schedule may hold and what is announced after a write all stay with the
/// `rimaia-core` function that owns the fact (D3). That is what lets
/// `rimaia-runner` implement this on `runner.db` without holding a rule, and
/// `testing::machine::MemoryMachine` implement it for core's own tests, bound
/// to the real one by `machine_store_contract!`.
///
/// A refusal the schema enforces (a duplicate key, a worktree whose checkout
/// is unknown, a checkout that still has worktrees) is `Error::invalid` with
/// the sentence each method names, in both implementations, so a caller can
/// branch on it the same way whichever store it holds.
pub trait MachineStore: Send + Sync + 'static {
    // Settings: `runner_settings`, D3's key/value shape.

    /// The stored value, or `None` when the key was never written or was
    /// cleared. An empty string is a value, not an absence.
    fn get_setting<'a>(&'a self, key: &'a str) -> MachineFuture<'a, Option<String>>;

    /// Writes `value`, replacing any earlier one.
    fn set_setting<'a>(&'a self, key: &'a str, value: &'a str) -> MachineFuture<'a, ()>;

    /// Removes the key, so it reads as absent. Clearing an absent key is not
    /// an error.
    fn clear_setting<'a>(&'a self, key: &'a str) -> MachineFuture<'a, ()>;

    // Checkouts: this machine's clone of each repository it runs.

    /// Every checkout, ordered by repository id.
    fn list_checkouts(&self) -> MachineFuture<'_, Vec<Checkout>>;

    fn get_checkout<'a>(&'a self, repository_id: &'a str) -> MachineFuture<'a, Option<Checkout>>;

    /// Refuses a second checkout of one repository: "this machine already has
    /// a checkout of repository {id}".
    fn insert_checkout<'a>(&'a self, checkout: &'a Checkout) -> MachineFuture<'a, ()>;

    /// Applies every field `patch` sets, and answers whether the checkout
    /// exists.
    fn patch_checkout<'a>(
        &'a self,
        repository_id: &'a str,
        patch: &'a CheckoutPatch,
    ) -> MachineFuture<'a, bool>;

    /// Removes the checkout, and answers whether it existed. Refuses one that
    /// still has worktrees: "repository {id} still has worktrees on this
    /// machine, so its checkout cannot be removed".
    fn remove_checkout<'a>(&'a self, repository_id: &'a str) -> MachineFuture<'a, bool>;

    // Worktrees: which directory on this machine holds each task's work.

    fn get_worktree<'a>(&'a self, task_id: &'a str) -> MachineFuture<'a, Option<WorktreeRecord>>;

    /// Every worktree record, ordered by task id.
    fn list_worktrees(&self) -> MachineFuture<'_, Vec<WorktreeRecord>>;

    /// Records where a task's worktree is, replacing an earlier record for the
    /// same task. Refuses one whose checkout is unknown: "this machine has no
    /// checkout of repository {id}".
    fn record_worktree<'a>(&'a self, record: &'a WorktreeRecord) -> MachineFuture<'a, ()>;

    /// Forgets a task's worktree, and answers whether there was one.
    fn forget_worktree<'a>(&'a self, task_id: &'a str) -> MachineFuture<'a, bool>;

    // Held leases: the board leases this runner holds (task 043).

    /// Records a lease, replacing an earlier record for the same task: one
    /// row per task, because the board holds at most one lease per task, and
    /// a record a missed forget left behind must not refuse the next claim's.
    fn record_held_lease<'a>(&'a self, lease: &'a HeldLease) -> MachineFuture<'a, ()>;

    /// Writes a held lease's run and purpose together, and answers whether
    /// the task has a record.
    fn set_held_lease_run<'a>(
        &'a self,
        task_id: &'a str,
        run_id: Option<&'a str>,
        purpose: LeasePurpose,
    ) -> MachineFuture<'a, bool>;

    /// Forgets a task's held lease, and answers whether there was one.
    fn forget_held_lease<'a>(&'a self, task_id: &'a str) -> MachineFuture<'a, bool>;

    /// Every held lease, ordered by task id.
    fn list_held_leases(&self) -> MachineFuture<'_, Vec<HeldLease>>;

    // Schedules: task 013's table, one operation per write `schedule::`
    // performs.

    /// Every schedule, ordered by name and then id.
    fn list_schedules(&self) -> MachineFuture<'_, Vec<Schedule>>;

    fn get_schedule<'a>(&'a self, id: &'a str) -> MachineFuture<'a, Option<Schedule>>;

    /// Refuses a second schedule with one id: "a schedule with id {id} already
    /// exists".
    fn insert_schedule<'a>(&'a self, schedule: &'a Schedule) -> MachineFuture<'a, ()>;

    /// Replaces a schedule's configuration (every column but `id` and
    /// `last_fired_at`), and answers whether it exists.
    fn update_schedule<'a>(&'a self, schedule: &'a Schedule) -> MachineFuture<'a, bool>;

    /// Writes `enabled` and `armed_at` together, and answers whether the
    /// schedule exists.
    fn set_schedule_enabled<'a>(
        &'a self,
        id: &'a str,
        enabled: bool,
        armed_at: Option<DateTime<Utc>>,
    ) -> MachineFuture<'a, bool>;

    /// Writes `last_fired_at`, and answers whether the schedule exists.
    fn record_schedule_fire<'a>(
        &'a self,
        id: &'a str,
        fired_at: DateTime<Utc>,
    ) -> MachineFuture<'a, bool>;

    /// Deletes a schedule, and answers whether it existed.
    fn delete_schedule<'a>(&'a self, id: &'a str) -> MachineFuture<'a, bool>;
}

/// The refusal both stores give a second checkout of one repository.
pub fn duplicate_checkout(repository_id: &str) -> crate::Error {
    crate::Error::invalid(format!(
        "this machine already has a checkout of repository {repository_id}"
    ))
}

/// The refusal both stores give a worktree whose checkout is unknown.
pub fn unknown_checkout(repository_id: &str) -> crate::Error {
    crate::Error::invalid(format!(
        "this machine has no checkout of repository {repository_id}"
    ))
}

/// The refusal both stores give a checkout that still has worktrees.
pub fn checkout_in_use(repository_id: &str) -> crate::Error {
    crate::Error::invalid(format!(
        "repository {repository_id} still has worktrees on this machine, so its checkout cannot \
         be removed"
    ))
}

/// The refusal both stores give a second schedule with one id.
pub fn duplicate_schedule(id: &str) -> crate::Error {
    crate::Error::invalid(format!("a schedule with id {id} already exists"))
}
