//! The run queue: what runs next, what claims it, and what a crash left behind
//! (ADR-0010, ADR-0007, ADR-0011, ADR-0012).
//!
//! One long-lived task ([`QueueTask`]) works the `ready` column top-down, one
//! run at a time, for the process lifetime. Everything it decides is
//! recomputable from the database, so a crash loses at most the in-flight
//! process — which is what [`reconcile`] then repairs on the next launch.
//!
//! # The four pieces, and why they are separate
//!
//! [`selection`] is pure ordering and eligibility over a board read; [`claim`]
//! is the conditional write that decides who owns a task; [`state`] is the
//! on/off switch, stored in `settings` rather than in a struct field; [`queue`]
//! is the loop that ties them together and the handle the shell holds. Only the
//! last of those needs a runtime, which is what makes the other three testable
//! as functions.
//!
//! # Nothing here waits on the clock
//!
//! The module this replaced said "everything here that waits or times out does
//! so against an injected [`Clock`](crate::Clock)". Nothing here waits at all:
//! the loop is woken by [`ChangeEvent`](crate::ChangeEvent) publications
//! (ADR-0018's "adding the scheduler's own view later is another
//! `subscribe()`") and by its own control calls, never by a poll interval. The
//! clock still travels on the [`ServiceContext`](crate::ServiceContext) and
//! every timestamp the queue causes to be written comes from it — but there is
//! no backoff and no run window here yet, so there is nothing to sleep through.
//! ADR-0011's waits arrive with task 014, and they inject the clock then.
//!
//! # The scheduler is not a second writer of anything
//!
//! Every `run_state` transition goes through
//! [`set_run_state`](crate::tasks::set_run_state), every `runs` row through
//! [`crate::runner::outcome`], every board move through
//! [`move_task`](crate::tasks::move_task). There is no `UPDATE tasks` and no
//! `INSERT INTO runs` anywhere in this module, deliberately: the same invariant
//! enforced in two places eventually enforces two different invariants
//! (ADR-0006), and a scheduler is exactly the second place it would happen.

pub mod claim;
pub mod queue;
pub mod reconcile;
pub mod selection;
pub mod state;

pub use claim::{claim, release, ClaimOutcome};
pub use queue::{build, QueueHandle, QueueStatus, QueueTask};
pub use reconcile::reconcile_interrupted;
pub use selection::{next_to_start, plan, skip_reason, QueueEntry, SkipReason};
pub use state::{queue_state, set_queue_state, QueueState, QUEUE_STATE};
