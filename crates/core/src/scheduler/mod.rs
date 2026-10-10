//! The scheduler's rules: what the board runs next, what claims it, what a
//! crash left behind, and what one runner may start (ADR-0010, ADR-0007,
//! ADR-0011, ADR-0012, ADR-0031).
//!
//! Task 042 cut this module along the line ADR-0031 draws: **the board decides
//! what runs next; the runner decides whether it has room, and when to ask.**
//! The loop that asks is not here any more. It is `rimaia_runner::queue`, and
//! nothing in this crate re-exports it: a second path to a loop is how two
//! loops happen. What is left is three groups, and none of them needs a
//! runtime of its own, which is what keeps every one testable as functions.
//!
//! # The board's half
//!
//! [`selection`] is pure ordering and eligibility over a board read;
//! [`claim`] keeps the operator's `give_up`, since the conditional write that
//! decides who owns a task *across* processes is `board::lease`'s one
//! transaction (task 043); [`attempts`] derives a retry budget from the `runs`
//! rows, [`retry`] is the policy that spends it and produces deadlines, and
//! [`reconcile`] repairs what a crash left behind, one runner's leases at a
//! time. The one place that runs selection for a claim is `board::service`'s
//! body of `ClaimTarget::Next`, over [`selection::plan`] and
//! [`selection::first_startable`].
//!
//! # The runner's half, over `MachineContext`
//!
//! [`state`] is the go signal, [`pause`] is ADR-0011's hold on new starts while
//! a usage window is closed, and [`capacity`] is how many runs may be in
//! flight at once and in each repository, with a run window's own mode and
//! concurrency overriding the defaults (D24). All three read and write this
//! machine's store through [`MachineContext`](crate::machine::MachineContext)
//! (task 041), so they are per runner already: two runners have two switches,
//! two holds and two sets of caps (ADR-0031 point 6). They stay in this crate
//! because core's own local MCP handlers call them, and core cannot depend on
//! the runner crate. [`view`] combines them with the slots into what a runner
//! tells the board it can take.
//!
//! [`capacity`] and [`selection`] are two modules rather than one because they
//! answer different questions on different sides of the port: "which tasks may
//! ever start" is the board's, "how many may start now" is the runner's.
//! Keeping the second out of `selection` is also what keeps `skip_reason` from
//! acquiring a transient reason — see `next_batch`'s own note on why capacity
//! is not a `SkipReason`.
//!
//! # The slot registry
//!
//! [`inflight`] is the in-memory fact of which tasks *this* process has a
//! child for. It is not a claim and not the board's lease: the lease survives a
//! restart and stops two writers disagreeing about a row, and fences a holder
//! that lost it (`board::LeaseRef`); the slot knows whether the process on the
//! end of that row is ours. A
//! manual start takes its slot before its claim; the runner loop takes it after,
//! because the board chooses the task (D19's 2026-10-10 amendment).
//!
//! # Waiting is on the injected clock
//!
//! A `waiting_retry` task becomes due at a wall-clock instant, and no mutation
//! publishes a [`ChangeEvent`](crate::ChangeEvent) when one passes, so whatever
//! waits for it waits on [`Clock::sleep_until`](crate::Clock::sleep_until) —
//! never `tokio::time::sleep`, because the deadline was computed against
//! [`Clock::now`](crate::Clock::now) and a wait measured any other way would be
//! a second clock (seam-contract D22, D23). That holds for the loop and for a
//! waiting `Next` claim alike.
//!
//! # The scheduler is not a second writer of anything
//!
//! Every `run_state` transition goes through
//! [`set_run_state`](crate::tasks::set_run_state) or the conditional
//! [`transition`](crate::tasks::run_state::transition) beneath it, every
//! `runs` row through
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
pub mod reconcile;
pub mod retry;
pub mod selection;
pub mod state;
pub mod view;

pub use attempts::{history as attempt_history, resume_point, Ending, ResumePoint};
pub use capacity::{
    configured as configured_capacity, max_concurrency, resolve as resolve_capacity, schedule_mode,
    set_max_concurrency, set_schedule_mode, Resolved, RunCapacity, DEFAULT_MAX_CONCURRENCY,
    DEFAULT_PER_REPOSITORY, MAX_CONCURRENCY, SCHEDULE_MODE,
};
pub use claim::give_up;
pub use inflight::{
    Capacity, Counts, InFlight, LocalSlot, SlotOwner, SlotRefused, CONCURRENCY_CEILING,
};
pub use pause::{
    active_until as usage_limit_pause_until, note_usage_limit, USAGE_LIMIT_PAUSE_UNTIL,
};
pub use reconcile::{reconcile_held, reconcile_unrecorded};
pub use retry::{
    decide as decide_retry, AttemptHistory, GiveUpReason, RetryDecision, RetryKind,
    MAX_TRANSIENT_ATTEMPTS, USAGE_LIMIT_FALLBACK_POLL,
};
pub use selection::{
    first_startable, next_batch, next_deadline, next_to_start, plan, skip_reason, QueueEntry,
    RunnerView, SkipReason,
};
pub use state::{queue_state, set_queue_state, QueueState, QUEUE_STATE};
pub use view::{for_runner, QueueStatus};
