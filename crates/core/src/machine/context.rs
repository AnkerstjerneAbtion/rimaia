//! The runner-side counterpart of [`ServiceContext`](crate::ServiceContext).

use std::fmt;
use std::sync::Arc;

use tokio::sync::broadcast;

use crate::clock::Clock;
use crate::events::{ChangeEvent, TeamId};

use super::port::MachineStore;

/// What a function owning a machine fact runs in: the store, the clock and
/// the channel its writes are announced on.
///
/// No pool, because the store is the only storage it reaches. No team scope
/// and no [`MutationSource`](crate::db::MutationSource), because a runner's own
/// state has neither a team nor a door: the queue switch is this machine's,
/// whoever pressed the button and whichever teams its tasks belong to.
///
/// Cheap to clone, like `ServiceContext`: the store and the clock are `Arc`s
/// and the sender is a handle. Built once in each place (D31 point 8's
/// shape): the shell's `setup()` over `runner.db`, and
/// `TestContext::machine()` over a `MemoryMachine`.
#[derive(Clone)]
pub struct MachineContext {
    pub store: Arc<dyn MachineStore>,
    pub clock: Arc<dyn Clock>,
    /// In solo, the board context's own sender, so the window hears a machine
    /// write exactly as it heard it when these facts lived in `rimaia.db`: a
    /// runner key as `Settings`, a schedule as `Schedules(ids)`.
    pub changes: broadcast::Sender<ChangeEvent>,
    /// The solo team, from `solo_identity`. Used only to build `ChangeEvent`s, which
    /// carry a team from 038 on, until 048 moves machine events to `LocalEvents`.
    pub event_team: TeamId,
}

impl MachineContext {
    /// Announces a machine write. Call it after the write, for
    /// [`ServiceContext::publish`](crate::ServiceContext::publish)'s reason.
    ///
    /// Takes the event's builder rather than an event, so the team is always
    /// [`event_team`](Self::event_team) and never one a caller chose: machine
    /// state belongs to no team, and the team here only lets the event ride
    /// the board's channel until task 048 gives it `LocalEvents`.
    ///
    /// Infallible for `publish`'s reason: a write that committed is never
    /// reported as failed because nothing was listening.
    pub fn publish(&self, event: impl FnOnce(TeamId) -> ChangeEvent) {
        let event = event(self.event_team.clone());
        if event.is_empty() {
            return;
        }
        let _ = self.changes.send(event);
    }
}

impl fmt::Debug for MachineContext {
    /// By hand because neither a store nor a clock is `Debug`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MachineContext")
            .field("event_team", &self.event_team)
            .field("subscribers", &self.changes.receiver_count())
            .finish_non_exhaustive()
    }
}
