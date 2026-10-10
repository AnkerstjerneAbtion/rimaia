//! An in-memory [`MachineStore`], for core's own tests (task 041).
//!
//! Core's tests cannot reach `rimaia-runner`'s store on `runner.db`: a
//! dev-dependency on the runner would give the test binary two copies of
//! every core type. So core tests a machine fact's rules against this, and
//! `machine_store_contract!` runs the same cases against both, which is the
//! only reason this can be trusted: every constraint the runner schema
//! enforces, this enforces with the same refusal.
//!
//! CLAUDE.md's "never fake git or the filesystem" still holds. This fakes a
//! store, the fake is bound to the real one by a shared suite, and production
//! never builds it.

use std::collections::BTreeMap;
use std::future::ready;
use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, Utc};

use crate::board::LeasePurpose;
use crate::db::Schedule;
use crate::machine::port::{
    checkout_in_use, duplicate_checkout, duplicate_schedule, unknown_checkout,
};
use crate::machine::{
    Checkout, CheckoutPatch, HeldLease, MachineFuture, MachineStore, WorktreeRecord,
};

#[derive(Debug, Default)]
pub struct MemoryMachine {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    settings: BTreeMap<String, String>,
    checkouts: BTreeMap<String, Checkout>,
    worktrees: BTreeMap<String, WorktreeRecord>,
    held_leases: BTreeMap<String, HeldLease>,
    schedules: BTreeMap<String, Schedule>,
}

impl MemoryMachine {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("the memory machine's lock is poisoned")
    }
}

/// Every method answers at once: the state is behind a lock held only for the
/// call, never across an `await`.
fn done<'a, T: Send + 'a>(value: crate::Result<T>) -> MachineFuture<'a, T> {
    Box::pin(ready(value))
}

impl MachineStore for MemoryMachine {
    fn get_setting<'a>(&'a self, key: &'a str) -> MachineFuture<'a, Option<String>> {
        done(Ok(self.state().settings.get(key).cloned()))
    }

    fn set_setting<'a>(&'a self, key: &'a str, value: &'a str) -> MachineFuture<'a, ()> {
        self.state()
            .settings
            .insert(key.to_string(), value.to_string());
        done(Ok(()))
    }

    fn clear_setting<'a>(&'a self, key: &'a str) -> MachineFuture<'a, ()> {
        self.state().settings.remove(key);
        done(Ok(()))
    }

    fn list_checkouts(&self) -> MachineFuture<'_, Vec<Checkout>> {
        done(Ok(self.state().checkouts.values().cloned().collect()))
    }

    fn get_checkout<'a>(&'a self, repository_id: &'a str) -> MachineFuture<'a, Option<Checkout>> {
        done(Ok(self.state().checkouts.get(repository_id).cloned()))
    }

    fn insert_checkout<'a>(&'a self, checkout: &'a Checkout) -> MachineFuture<'a, ()> {
        let mut state = self.state();
        if state.checkouts.contains_key(&checkout.repository_id) {
            return done(Err(duplicate_checkout(&checkout.repository_id)));
        }
        state
            .checkouts
            .insert(checkout.repository_id.clone(), checkout.clone());
        done(Ok(()))
    }

    fn patch_checkout<'a>(
        &'a self,
        repository_id: &'a str,
        patch: &'a CheckoutPatch,
    ) -> MachineFuture<'a, bool> {
        let mut state = self.state();
        let Some(checkout) = state.checkouts.get_mut(repository_id) else {
            return done(Ok(false));
        };
        *checkout = patch.applied_to(checkout);
        done(Ok(true))
    }

    fn remove_checkout<'a>(&'a self, repository_id: &'a str) -> MachineFuture<'a, bool> {
        let mut state = self.state();
        if state
            .worktrees
            .values()
            .any(|worktree| worktree.repository_id == repository_id)
        {
            return done(Err(checkout_in_use(repository_id)));
        }
        done(Ok(state.checkouts.remove(repository_id).is_some()))
    }

    fn get_worktree<'a>(&'a self, task_id: &'a str) -> MachineFuture<'a, Option<WorktreeRecord>> {
        done(Ok(self.state().worktrees.get(task_id).cloned()))
    }

    fn list_worktrees(&self) -> MachineFuture<'_, Vec<WorktreeRecord>> {
        done(Ok(self.state().worktrees.values().cloned().collect()))
    }

    fn record_worktree<'a>(&'a self, record: &'a WorktreeRecord) -> MachineFuture<'a, ()> {
        let mut state = self.state();
        if !state.checkouts.contains_key(&record.repository_id) {
            return done(Err(unknown_checkout(&record.repository_id)));
        }
        state
            .worktrees
            .insert(record.task_id.clone(), record.clone());
        done(Ok(()))
    }

    fn forget_worktree<'a>(&'a self, task_id: &'a str) -> MachineFuture<'a, bool> {
        done(Ok(self.state().worktrees.remove(task_id).is_some()))
    }

    fn record_held_lease<'a>(&'a self, lease: &'a HeldLease) -> MachineFuture<'a, ()> {
        self.state()
            .held_leases
            .insert(lease.task_id.clone(), lease.clone());
        done(Ok(()))
    }

    fn set_held_lease_run<'a>(
        &'a self,
        task_id: &'a str,
        run_id: Option<&'a str>,
        purpose: LeasePurpose,
    ) -> MachineFuture<'a, bool> {
        let mut state = self.state();
        let Some(lease) = state.held_leases.get_mut(task_id) else {
            return done(Ok(false));
        };
        lease.run_id = run_id.map(str::to_string);
        lease.purpose = purpose;
        done(Ok(true))
    }

    fn forget_held_lease<'a>(&'a self, task_id: &'a str) -> MachineFuture<'a, bool> {
        done(Ok(self.state().held_leases.remove(task_id).is_some()))
    }

    fn list_held_leases(&self) -> MachineFuture<'_, Vec<HeldLease>> {
        done(Ok(self.state().held_leases.values().cloned().collect()))
    }

    fn list_schedules(&self) -> MachineFuture<'_, Vec<Schedule>> {
        let mut schedules: Vec<Schedule> = self.state().schedules.values().cloned().collect();
        schedules.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        done(Ok(schedules))
    }

    fn get_schedule<'a>(&'a self, id: &'a str) -> MachineFuture<'a, Option<Schedule>> {
        done(Ok(self.state().schedules.get(id).cloned()))
    }

    fn insert_schedule<'a>(&'a self, schedule: &'a Schedule) -> MachineFuture<'a, ()> {
        let mut state = self.state();
        if state.schedules.contains_key(&schedule.id) {
            return done(Err(duplicate_schedule(&schedule.id)));
        }
        state
            .schedules
            .insert(schedule.id.clone(), schedule.clone());
        done(Ok(()))
    }

    fn update_schedule<'a>(&'a self, schedule: &'a Schedule) -> MachineFuture<'a, bool> {
        let mut state = self.state();
        let Some(stored) = state.schedules.get_mut(&schedule.id) else {
            return done(Ok(false));
        };
        *stored = Schedule {
            last_fired_at: stored.last_fired_at,
            ..schedule.clone()
        };
        done(Ok(true))
    }

    fn set_schedule_enabled<'a>(
        &'a self,
        id: &'a str,
        enabled: bool,
        armed_at: Option<DateTime<Utc>>,
    ) -> MachineFuture<'a, bool> {
        let mut state = self.state();
        let Some(stored) = state.schedules.get_mut(id) else {
            return done(Ok(false));
        };
        stored.enabled = enabled;
        stored.armed_at = armed_at;
        done(Ok(true))
    }

    fn record_schedule_fire<'a>(
        &'a self,
        id: &'a str,
        fired_at: DateTime<Utc>,
    ) -> MachineFuture<'a, bool> {
        let mut state = self.state();
        let Some(stored) = state.schedules.get_mut(id) else {
            return done(Ok(false));
        };
        stored.last_fired_at = Some(fired_at);
        done(Ok(true))
    }

    fn delete_schedule<'a>(&'a self, id: &'a str) -> MachineFuture<'a, bool> {
        done(Ok(self.state().schedules.remove(id).is_some()))
    }
}

/// A checkout of `repository_id` at `path`, with every other field at what
/// registration gives a new one: a cap of one, no consent, no archive policy,
/// no credential, and its worktrees in a sibling directory of the clone.
///
/// For a test that seeds a board row directly and needs this machine to have
/// the clone it names (task 066).
pub fn checkout_at(repository_id: &str, path: &std::path::Path) -> Checkout {
    Checkout {
        repository_id: repository_id.to_string(),
        path: path.to_string_lossy().into_owned(),
        worktree_root: format!("{}-worktrees", path.to_string_lossy()),
        max_concurrency: 1,
        unattended_consent: false,
        on_archive: crate::db::OnArchive::None,
        on_archive_script: None,
        credential_login: None,
        credential_label: None,
        credential_added_at: None,
        created_at: crate::testing::test_epoch(),
    }
}
