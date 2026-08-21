use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use rimaia_core::runner::events::RunTail;
use rimaia_core::runner::CancelSignal;
use rimaia_core::{AppPaths, Error, Result, ServiceContext};

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
/// `runs` is the shell's own bookkeeping for task 008 (seam-contract D14): which
/// task has a process in flight right now, and the most recent live-tail
/// snapshot for it. Neither lives in `rimaia-core` — a run's `EventStream` and
/// `RunProgress` live entirely on the stack of the task supervising it
/// (`runner::process::execute`), and there is deliberately no shared registry of
/// them there. Publishing a [`RunTail`] on the D14 channel *is* core's whole
/// answer to "what is happening right now"; remembering the latest one so a
/// client that opens the Runs view mid-run has something to read is the
/// shell's to build.
pub struct AppState {
    pub context: ServiceContext,
    pub paths: AppPaths,
    pub runs: RunRegistry,
}

/// How many concurrently-tracked runs' tail snapshots [`RunRegistry`] keeps.
///
/// Task 008 starts runs one manual click at a time, so in practice this is
/// almost always zero or one — the cap exists so an evening of "Run now"
/// clicks across many tasks does not grow the cache without bound in a
/// process that runs all night, the same reasoning behind every other bounded
/// buffer in this codebase (`TAIL_CHANNEL_CAPACITY`, `RECENT_ACTIVITY_CAPACITY`
/// in `rimaia_core::runner::events`).
const MAX_TRACKED_RUN_TAILS: usize = 32;

/// The shell's half of task 008: which tasks have a run in flight, and the
/// latest live-tail snapshot for each (seam-contract D14).
///
/// Cheap to clone — every clone shares the same maps through one `Arc` — so a
/// command can hand a clone into a `tauri::async_runtime::spawn`'d future
/// without reaching for a lifetime, the same shape `ServiceContext` already
/// uses for the same reason.
#[derive(Clone, Default)]
pub struct RunRegistry {
    inner: Arc<RunRegistryInner>,
}

#[derive(Default)]
struct RunRegistryInner {
    /// One entry per task with a process currently supervised by
    /// `runner::run_task`. Presence, not the signal's own state, is what
    /// [`RunRegistry::start`] checks — a task is "running" from this
    /// registry's point of view for exactly as long as its entry exists.
    cancels: Mutex<HashMap<String, CancelSignal>>,
    tails: Mutex<TailCache>,
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

impl RunRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claims `task_id` for a new run, refusing a second concurrent one.
    ///
    /// Task 009 owns real per-repository concurrency (ADR-0010); this is a
    /// narrower guard against the one thing a manual "Run now" button could
    /// otherwise do — start a second `claude` process against the same
    /// worktree because a click landed before the first process's own
    /// `run_state = running` claim was visible anywhere the button reads.
    pub fn start(&self, task_id: &str) -> Result<CancelSignal> {
        let mut cancels = self.inner.cancels.lock().expect("run registry poisoned");
        if cancels.contains_key(task_id) {
            return Err(Error::invalid(
                "a run is already in progress for this task; wait for it to finish or cancel it first",
            ));
        }
        let cancel = CancelSignal::new();
        cancels.insert(task_id.to_string(), cancel.clone());
        Ok(cancel)
    }

    /// Releases the claim once `run_task` has returned — whatever it
    /// returned, success or failure, so a task never stays marked "running"
    /// here after its process is gone.
    pub fn finish(&self, task_id: &str) {
        self.inner
            .cancels
            .lock()
            .expect("run registry poisoned")
            .remove(task_id);
    }

    /// Signals cancellation for `task_id`'s in-flight run. A no-op — not an
    /// error — when nothing is running for it: `CancelSignal::cancel` is
    /// itself idempotent, and a Cancel button pressed after a run already
    /// finished is not a mistake worth reporting.
    pub fn cancel(&self, task_id: &str) {
        if let Some(cancel) = self
            .inner
            .cancels
            .lock()
            .expect("run registry poisoned")
            .get(task_id)
        {
            cancel.cancel();
        }
    }

    /// Signals every run currently tracked. Returns whether there was
    /// anything to signal, so a caller with nothing in flight can skip
    /// waiting entirely — see `lib.rs`'s exit handling.
    pub fn cancel_all(&self) -> bool {
        let cancels = self.inner.cancels.lock().expect("run registry poisoned");
        for cancel in cancels.values() {
            cancel.cancel();
        }
        !cancels.is_empty()
    }

    /// Whether any task is still claimed here. Polled, bounded, by `lib.rs`'s
    /// shutdown handling while it waits for cancelled runs to actually exit.
    pub fn has_in_flight_runs(&self) -> bool {
        !self
            .inner
            .cancels
            .lock()
            .expect("run registry poisoned")
            .is_empty()
    }

    /// Records the latest snapshot for its run. Called only from the D14
    /// forwarding loop.
    pub fn record_tail(&self, tail: RunTail) {
        self.inner
            .tails
            .lock()
            .expect("run registry poisoned")
            .record(tail);
    }

    /// The latest snapshot for `run_id`, or `None` when the shell has not
    /// seen one — either nothing has run yet, or it aged out of
    /// [`MAX_TRACKED_RUN_TAILS`].
    pub fn tail(&self, run_id: &str) -> Option<RunTail> {
        self.inner
            .tails
            .lock()
            .expect("run registry poisoned")
            .get(run_id)
    }
}
