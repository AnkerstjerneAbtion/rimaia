//! The runner-side counterpart of [`ServiceContext`](crate::ServiceContext).

use std::fmt;
use std::sync::Arc;

use crate::clock::Clock;
use crate::events::{LocalChange, LocalEvents};

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
/// and the channel is a handle. Built once in each place (D31 point 8's
/// shape): the shell's `setup()` over `runner.db`, and
/// `TestContext::machine()` over a `MemoryMachine`.
#[derive(Clone)]
pub struct MachineContext {
    pub store: Arc<dyn MachineStore>,
    pub clock: Arc<dyn Clock>,
    /// Where this machine's writes are announced (task 048): its own channel,
    /// never the board's. No machine writer can reach a `ChangeEvent` sender,
    /// so no machine fact can ride a server's stream, and none needs a team.
    pub local: LocalEvents,
}

impl MachineContext {
    /// Announces a machine write. Call it after the write, for
    /// [`ServiceContext::publish`](crate::ServiceContext::publish)'s reason.
    ///
    /// Infallible for `publish`'s reason: a write that committed is never
    /// reported as failed because nothing was listening.
    pub fn publish(&self, change: LocalChange) {
        self.local.publish(change);
    }
}

impl fmt::Debug for MachineContext {
    /// By hand because neither a store nor a clock is `Debug`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MachineContext")
            .field("subscribers", &self.local.receiver_count())
            .finish_non_exhaustive()
    }
}
