use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use rimaia_core::mcp::{McpHandle, RunHandles};
use rimaia_core::runner::events::RunTail;
use rimaia_core::runner::{CancelSignal, RunnerConfig};
use rimaia_core::scheduler::{InFlight, QueueHandle};
use rimaia_core::{AppPaths, ServiceContext};

/// Everything a command needs, built once at startup and managed by Tauri.
///
/// `context` is the one `ServiceContext` for the whole process (ADR-0018): the
/// `SqlitePool` (ADR-0003), the clock, and the change-event sender travel
/// together, so every command calls a `rimaia-core` service the same way the
/// MCP server (task 010) will — `&state.context`, never a bare pool pulled back
/// out of it. `paths` stays a separate field because it is the shell's own
/// concern (`AppPaths::worktrees_dir` and friends resolve a platform directory
/// core cannot look up itself), not something a service needs ambiently.
///
/// `in_flight` is which tasks this process has a `claude` child for, and it is
/// **`rimaia-core`'s** — `scheduler::inflight::InFlight`, held here as a clone
/// of the same value `scheduler::build` was given. It used to be two maps that
/// could not see each other: a `cancels: HashMap` here for what a button
/// started, and a private `Option` inside the queue for what the scheduler
/// started, wired together by an `attach_queue` back-reference. That put the
/// rule "one process per task" in the shell, which ADR-0006 calls a bug
/// wherever it happens, and it is the reason ADR-0021 could not put
/// `plan_task_strategy` on the MCP surface — the MCP server cannot reach a
/// `src-tauri` type. Now every door reads one map, and this struct holds no
/// business rule at all.
///
/// `tails` is what genuinely is the shell's (seam-contract D14): the most
/// recent live-tail snapshot per run. A run's `EventStream` and `RunProgress`
/// live entirely on the stack of the task supervising it
/// (`runner::process::execute`), and there is deliberately no shared registry of
/// them in core. Publishing a [`RunTail`] on the D14 channel *is* core's whole
/// answer to "what is happening right now"; remembering the latest one so a
/// client that opens the Runs view mid-run has something to read is the
/// shell's to build.
///
/// `mcp` is the handle onto that server: `lib.rs` binds it as the last step of
/// `setup()` — nothing outside Rimaia should reach the board until every repair
/// startup was going to make has been made — and the exit path shuts it down
/// before the queue, above `cancel_all`'s early return, so quitting with
/// nothing in flight still closes the listener.
///
/// `queue` is task 009's one long-lived queue for the process's lifetime
/// (ADR-0010). `commands::queue` is a thin wrapper over it — Start, Pause,
/// Resume, Stop and Status are all this handle's own methods — and
/// `lib.rs`'s exit path calls `shutdown` on it before anything is cancelled,
/// so the queue stops claiming new work before its current run is asked to
/// end. It no longer needs to be handed to anything else: the double-spawn
/// refusal a manual "Run now" used to get by asking the queue is now the
/// `in_flight` registry's, which both of them read directly.
///
/// `runner` and `run_handles` are task 020's two shared values, and they are
/// shared for the same kind of reason: a second copy would be a second answer.
/// See each field's own doc.
pub struct AppState {
    pub context: ServiceContext,
    pub paths: AppPaths,
    pub in_flight: InFlight,
    pub tails: RunTails,
    pub queue: QueueHandle,
    /// The one [`RunnerConfig`] every starter spawns from — task 009's queue,
    /// a manual "Run now", and task 020's "Plan now".
    ///
    /// Built once in `setup()` and cloned from there, replacing the two
    /// independent `RunnerConfig::default()` calls task 020 found in `lib.rs`
    /// and `commands::runs`. What it holds — which `claude` binary, the grace
    /// period a cancelled child gets, the turn budget — is a property of *this
    /// installation*, not of whoever pressed which button, so two starters that
    /// disagreed about any of it would spawn measurably different processes for
    /// the same card with nothing on screen to explain the difference.
    pub runner: RunnerConfig,
    /// Task 020's live run-scoped MCP endpoints (seam-contract D17.4).
    ///
    /// Built in `setup()` before either subsystem and handed to both:
    /// [`mcp::build`](rimaia_core::mcp::build) records the address it actually
    /// bound on **every** bind — including the rebind
    /// `commands::mcp::set_mcp_port` performs at runtime — and the runner mints
    /// a per-run token against this same value. That is the whole reason it is
    /// shared mutable state rather than a URL captured once: a captured URL goes
    /// stale the moment the operator moves the port, and the next planner would
    /// be handed an endpoint nothing answers.
    ///
    /// It is also what removes an ordering constraint from `setup()` — neither
    /// the scheduler nor the MCP server has to exist before the other for a run
    /// to have somewhere to mint a token.
    pub run_handles: RunHandles,
    /// Task 010's MCP server (ADR-0006).
    ///
    /// A `Mutex` because this is the one field that is not write-once:
    /// `commands::mcp::set_mcp_port` replaces the whole handle in-process when
    /// the user moves the port, since a listener cannot be rebound. Held only
    /// for the swap and never across an `await`.
    pub mcp: Mutex<McpHandle>,
    /// The cancel signal of the planning pass currently running, if any (task
    /// 023).
    ///
    /// One at a time, because a pass is sequential by design and two of them
    /// would be exactly the fan-out task 023's Notes refuse. It lives here
    /// rather than in `rimaia-core` for the reason `tails` does: it is a
    /// property of *this window's* button, not of the planning rules, and
    /// `plan_all` takes the signal as an argument so the MCP surface — which
    /// has no Cancel button — hands in one nothing holds.
    ///
    /// A `Mutex` held only for the swap, never across an `await`.
    pub plan_pass: Mutex<Option<CancelSignal>>,
}

/// How many concurrently-tracked runs' tail snapshots [`RunTails`] keeps.
///
/// Task 008 starts runs one manual click at a time, so in practice this is
/// almost always zero or one — the cap exists so an evening of "Run now"
/// clicks across many tasks does not grow the cache without bound in a
/// process that runs all night, the same reasoning behind every other bounded
/// buffer in this codebase (`TAIL_CHANNEL_CAPACITY`, `RECENT_ACTIVITY_CAPACITY`
/// in `rimaia_core::runner::events`).
const MAX_TRACKED_RUN_TAILS: usize = 32;

impl AppState {
    /// Asks every in-flight run to stop and reports whether there was anything
    /// to ask, so a caller with nothing running can skip the wait entirely.
    ///
    /// Two calls rather than one, and both are needed. `queue.stop()` persists
    /// `queue_state = paused` — a policy decided on *any* quit, not only on a
    /// quit that happened to catch a run mid-flight (seam-contract D15: "a run
    /// the app just killed by quitting should not silently restart itself on
    /// the next launch without the user asking again"). `in_flight.cancel_all`
    /// then reaches every run whoever started it, which `stop` deliberately
    /// does not: `stop` is scoped to the queue's own leases, because stopping
    /// the queue is a statement about the queue. Quitting is a statement about
    /// everything.
    ///
    /// `lib.rs`'s `shut_down` calls [`QueueHandle::shutdown`] before this — the
    /// switch that stops the loop *starting* another task — so this only ever
    /// has to stop what is already running, never race a fresh claim.
    pub async fn cancel_everything(&self) -> bool {
        let anything_in_flight = self.in_flight.cancel_all();
        if let Err(error) = self.queue.stop().await {
            tracing::error!(%error, "could not stop the run queue while exiting");
        }
        anything_in_flight
    }

    /// Whether any run is still in flight. Polled, bounded, by `lib.rs`'s
    /// shutdown handling while it waits for cancelled runs to actually exit.
    pub fn has_in_flight_runs(&self) -> bool {
        !self.in_flight.is_empty()
    }
}

/// A small bounded cache of the most recent [`RunTail`] per run id.
///
/// Fed by the D14 forwarding loop `lib.rs` spawns in `setup()` — the same one
/// that re-emits every snapshot as `runs:tail` — and read back by
/// `commands::runs::get_run_tail`. This is *not* the deeper ring buffer
/// seam-contract D14 describes (`RunProgress::recent`, up to 64 individual
/// assistant-text and tool-call lines): that buffer lives inside the
/// `EventStream` owned by the task supervising the run, on its own stack,
/// and rimaia-core exposes no registry of them for anything outside that task
/// to reach. What crosses the D14 channel is [`RunTail`] — one rolled-up
/// snapshot of where things stand — so that is the coarser thing a newly
/// watching client can actually catch up on here: not every recent line, just
/// the current elapsed time, turn count, tool call and last message.
#[derive(Default)]
struct TailCache {
    /// Insertion order, oldest first, for [`MAX_TRACKED_RUN_TAILS`] eviction.
    order: VecDeque<String>,
    latest: HashMap<String, RunTail>,
}

impl TailCache {
    fn record(&mut self, tail: RunTail) {
        if !self.latest.contains_key(&tail.run_id) {
            self.order.push_back(tail.run_id.clone());
            if self.order.len() > MAX_TRACKED_RUN_TAILS {
                if let Some(oldest) = self.order.pop_front() {
                    self.latest.remove(&oldest);
                }
            }
        }
        self.latest.insert(tail.run_id.clone(), tail);
    }

    fn get(&self, run_id: &str) -> Option<RunTail> {
        self.latest.get(run_id).cloned()
    }
}

/// The shell's half of seam-contract D14: the latest live-tail snapshot per run.
///
/// Cheap to clone — every clone shares the same map through one `Arc` — so a
/// command can hand a clone into a `tauri::async_runtime::spawn`'d future
/// without reaching for a lifetime, the same shape `ServiceContext` already
/// uses for the same reason.
///
/// This was `RunRegistry`, and it registered two things. The other one — which
/// tasks have a process in flight — moved to `rimaia_core::scheduler::InFlight`,
/// where it belongs and where the MCP server can also reach it. What is left is
/// genuinely the shell's, because D14's catch-up snapshot is a thing the
/// *forwarder* has seen and core deliberately keeps no registry of.
#[derive(Clone, Default)]
pub struct RunTails {
    inner: Arc<Mutex<TailCache>>,
}

impl RunTails {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the latest snapshot for its run. Called only from the D14
    /// forwarding loop.
    pub fn record(&self, tail: RunTail) {
        self.lock().record(tail);
    }

    /// The latest snapshot for `run_id`, or `None` when the shell has not seen
    /// one — either nothing has run yet, or it aged out of
    /// [`MAX_TRACKED_RUN_TAILS`].
    pub fn get(&self, run_id: &str) -> Option<RunTail> {
        self.lock().get(run_id)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TailCache> {
        self.inner.lock().expect("run tail cache poisoned")
    }
}
