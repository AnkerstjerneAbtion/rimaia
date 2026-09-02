//! The run queue: what runs next, what claims it, and what a crash left behind
//! (ADR-0010, ADR-0007, ADR-0011, ADR-0012).
//!
//! One long-lived task ([`QueueTask`]) works the `ready` column top-down for
//! the process lifetime, one run at a time or several at once depending on
//! [`capacity`]. Everything it decides is recomputable from the database, so a
//! crash loses at most the in-flight processes — which is what [`reconcile`]
//! then repairs on the next launch.
//!
//! # The six pieces, and why they are separate
//!
//! [`selection`] is pure ordering and eligibility over a board read;
//! [`capacity`] is how many of that ordering may run at once and where the two
//! numbers are stored; [`claim`] is the conditional write that decides who owns
//! a task *across* processes; [`inflight`] is the in-memory fact of which tasks
//! *this* process has a child for; [`state`] is the on/off switch, stored in
//! `settings` rather than in a struct field; [`queue`] is the loop that ties
//! them together and the handle the shell holds. Only the last of those needs a
//! runtime, which is what makes the other five testable as functions.
//!
//! [`capacity`] and [`selection`] are two modules rather than one because they
//! answer different questions and only one of them touches the database:
//! "which tasks may ever start" is a pure function of a board read, "how many
//! may start now" is a read of two settings keys and a column. Keeping the
//! second out of `selection` is also what keeps `skip_reason` from acquiring a
//! transient reason — see `next_batch`'s own note on why capacity is not a
//! `SkipReason`.
//!
//! [`claim`] and [`inflight`] answer different questions and both are needed.
//! The claim is a conditional write that survives a restart and stops two
//! writers disagreeing about a row; the registry is what knows whether the
//! process on the end of that row is ours. The queue takes the lease *before*
//! the claim, so a Pause pressed mid-claim has something to act on.
//!
//! # The one thing here that waits, waits on the injected clock
//!
//! Task 009's version of this header said "nothing here waits at all", and that
//! was true until ADR-0011's backoff arrived: a `waiting_retry` task becomes due
//! at a wall-clock instant, and no mutation publishes a
//! [`ChangeEvent`](crate::ChangeEvent) when one passes. So the loop has a fourth
//! wake source, and it is [`Clock::sleep_until`](crate::Clock::sleep_until) —
//! not `tokio::time::sleep`, because the deadline was computed against
//! [`Clock::now`](crate::Clock::now) and a wait measured any other way would be
//! a second clock (seam-contract D22). Everything else here is still woken by
//! publications and control calls, never by a poll interval.
//!
//! [`retry`] is the policy that produces those deadlines and is entirely pure;
//! [`attempts`] derives the budget it spends from the `runs` rows; [`pause`] is
//! ADR-0011's global hold on new starts while a usage window is closed.
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

pub mod attempts;
pub mod capacity;
pub mod claim;
pub mod inflight;
pub mod pause;
pub mod queue;
pub mod reconcile;
pub mod retry;
pub mod selection;
pub mod state;

pub use attempts::{history as attempt_history, resumable_session, Ending};
pub use capacity::{
    configured as configured_capacity, max_concurrency, resolve as resolve_capacity, schedule_mode,
    set_max_concurrency, set_schedule_mode, Resolved, RunCapacity, DEFAULT_MAX_CONCURRENCY,
    DEFAULT_PER_REPOSITORY, MAX_CONCURRENCY, SCHEDULE_MODE,
};
pub use claim::{claim, claim_retry, give_up, release, ClaimOutcome};
pub use pause::{
    active_until as usage_limit_pause_until, note_usage_limit, USAGE_LIMIT_PAUSE_UNTIL,
};
pub use retry::{
    decide as decide_retry, AttemptHistory, GiveUpReason, RetryDecision, RetryKind,
    MAX_TRANSIENT_ATTEMPTS, USAGE_LIMIT_FALLBACK_POLL,
};
pub use inflight::{
    Capacity, Counts, InFlight, Lease, LeaseOwner, LeaseRefused, CONCURRENCY_CEILING,
};
pub use queue::{build, QueueHandle, QueueStatus, QueueTask};
pub use reconcile::reconcile_interrupted;
pub use selection::{
    next_batch, next_deadline, next_to_start, plan, skip_reason, QueueEntry, SkipReason,
};
pub use state::{queue_state, set_queue_state, QueueState, QUEUE_STATE};
