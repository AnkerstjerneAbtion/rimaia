//! Requests to stop a run on whichever runner holds it (ADR-0031 point 3,
//! seam-contract D31 point 4, task 052).
//!
//! Apart from the claim, the heartbeat is the board's only channel to a
//! runner, so a Cancel pressed on another client is held here until the
//! holding runner's next heartbeat lists it in `Heartbeat::cancel`. Each entry
//! names a lease, `(task_id, generation)`, never a task alone: a request made
//! of one lease must not stop the run a later lease of the same task starts.
//!
//! **In memory, on purpose.** A Cancel is a button press that a runner online
//! at that moment collects within one heartbeat. If the server restarts first
//! the press is lost, the card is unchanged, and the person presses again. D4
//! reserves no migration for it, and none is needed.
//!
//! The runner's reaction to a listed cancel is task 053's.

use std::collections::BTreeSet;
use std::sync::Mutex;

/// The pending cancels, keyed by lease.
#[derive(Debug, Default)]
pub struct CancelRequests {
    requested: Mutex<BTreeSet<(String, i64)>>,
}

impl CancelRequests {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the runner holding `task_id`'s lease of `generation` to stop its
    /// run. Asking again is the same request.
    pub fn request(&self, task_id: &str, generation: i64) {
        self.lock().insert((task_id.to_string(), generation));
    }

    /// Whether someone asked to stop the run under this lease.
    pub fn is_requested(&self, task_id: &str, generation: i64) -> bool {
        self.lock().contains(&(task_id.to_string(), generation))
    }

    /// Every pending request, `(task_id, generation)`: what the heartbeat
    /// walks to drop the ones whose lease has ended.
    pub fn pending(&self) -> Vec<(String, i64)> {
        self.lock().iter().cloned().collect()
    }

    pub(crate) fn forget(&self, task_id: &str, generation: i64) {
        self.lock().remove(&(task_id.to_string(), generation));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeSet<(String, i64)>> {
        // A panic while holding the set cannot leave it half-written: every
        // operation is one insert, lookup or removal.
        self.requested
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_names_one_lease_and_not_the_task() {
        let cancels = CancelRequests::new();
        cancels.request("task-1", 3);
        cancels.request("task-1", 3);

        assert!(cancels.is_requested("task-1", 3));
        assert!(!cancels.is_requested("task-1", 4), "a later lease");
        assert_eq!(cancels.pending(), vec![("task-1".to_string(), 3)]);

        cancels.forget("task-1", 3);
        assert!(cancels.pending().is_empty());
    }
}
