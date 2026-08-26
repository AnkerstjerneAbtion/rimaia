use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use rimaia_core::mcp::McpHandle;
use rimaia_core::runner::events::RunTail;
use rimaia_core::runner::CancelSignal;
use rimaia_core::scheduler::QueueHandle;
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
/// `lib.rs`'s exit path calls `shutdown` on it before `runs.cancel_all` runs,
/// so the queue stops claiming new work before its current run is asked to
/// end. It is also handed to `runs` itself via [`RunRegistry::attach_queue`]
/// in `setup()`, which is what lets a manual "Run now" and the queue's own
/// claim refuse to double-spawn a process for the same task — see that
/// method's doc.
pub struct AppState {
    pub context: ServiceContext,
    pub paths: AppPaths,
    pub runs: RunRegistry,
    pub queue: QueueHandle,
    /// Task 010's MCP server (ADR-0006).
    ///
    /// A `Mutex` because this is the one field that is not write-once:
    /// `commands::mcp::set_mcp_port` replaces the whole handle in-process when
    /// the user moves the port, since a listener cannot be rebound. Held only
    /// for the swap and never across an `await`.
    pub mcp: Mutex<McpHandle>,
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
    ///
    /// The queue's own in-flight run is deliberately **not** an entry here —
    /// `scheduler::QueueTask` keeps its `CancelSignal` to itself, and the only
    /// door onto it is [`QueueHandle::stop`]. See [`RunRegistry::attach_queue`]
    /// for how this registry still refuses to double-spawn a task the queue
    /// owns, and [`RunRegistry::cancel_all`] for how it still stops that run
    /// on exit.
    cancels: Mutex<HashMap<String, CancelSignal>>,
    tails: Mutex<TailCache>,
    /// The queue this registry defers to, set once in `setup()` — see
    /// [`RunRegistry::attach_queue`]. `OnceLock` rather than
    /// `Mutex<Option<_>>`: it is written exactly once, before any command can
    /// run, so every read after that is a lock-free `Option::as_ref`.
    queue: OnceLock<QueueHandle>,
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

    /// Wires the queue this registry defers to for the two things a
    /// shell-only, in-memory map cannot answer on its own (task 009):
    /// whether the *queue* already owns a task ([`start`](Self::start)), and
    /// how to actually stop the queue's own in-flight run on exit
    /// ([`cancel_all`](Self::cancel_all)). Called exactly once, from
    /// `setup()`, immediately after `scheduler::build` — every command that
    /// reads `AppState` afterwards sees it already attached, so `start` and
    /// `cancel_all` never have to treat "not attached yet" as a state they
    /// handle.
    ///
    /// # Why this has to exist at all
    ///
    /// `runner::process::claim` — the DB-level claim inside `run_task` — has
    /// the arm `RunState::Running => &[]`: once a task is already `running`,
    /// *any* caller's `run_task` treats that as "already claimed" and
    /// proceeds straight to spawning a process, because the arm exists so the
    /// queue's own call to `run_task` (after its own `scheduler::claim`
    /// already moved the row to `running`) does not fight itself. That
    /// shortcut does not distinguish *who* holds the claim, so a manual
    /// "Run now" reaching `run_task` for a task the queue is already running
    /// would sail through the same no-op and spawn a second process. Refusing
    /// it here, before `run_task` is ever called, is the only place left to
    /// stop that: `commands::runs::start_task_run` is not this stage's file
    /// to change, and `scheduler::claim` (the real, transactional check) is
    /// `rimaia-core`'s.
    pub fn attach_queue(&self, queue: QueueHandle) {
        if self.inner.queue.set(queue).is_err() {
            // Every caller in this codebase calls this exactly once, from
            // `setup()`, before `app.manage` hands the registry to any
            // command — a second call is a programming error, not a race
            // worth handling gracefully.
            tracing::error!(
                "RunRegistry::attach_queue called more than once; keeping the first queue"
            );
        }
    }

    /// Claims `task_id` for a new run, refusing a second concurrent one.
    ///
    /// Task 009 owns real per-repository concurrency (ADR-0010); this is a
    /// narrower guard against the one thing a manual "Run now" button could
    /// otherwise do — start a second `claude` process against the same
    /// worktree because a click landed before the first process's own
    /// `run_state = running` claim was visible anywhere the button reads.
    ///
    /// Also refuses `task_id` outright when the attached queue is already
    /// running it — see [`attach_queue`](Self::attach_queue)'s doc for why
    /// this has to be checked here, before `run_task` is ever reached, rather
    /// than inside it.
    pub fn start(&self, task_id: &str) -> Result<CancelSignal> {
        if self.queue_owns(task_id) {
            return Err(Error::invalid(
                "the run queue is already working on this task; pause or stop the queue, \
                 or wait for it to finish, before starting it by hand",
            ));
        }

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

    /// Whether the attached queue's own in-flight run — not this registry's
    /// map — is `task_id`. `false` when no queue has been attached, which
    /// only happens before `setup()` finishes wiring one; no command runs
    /// before then.
    fn queue_owns(&self, task_id: &str) -> bool {
        self.inner
            .queue
            .get()
            .is_some_and(|queue| queue.in_flight_task_id().as_deref() == Some(task_id))
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

    /// Signals every run currently tracked here, manual and — through the
    /// attached queue — its own, and reports whether there was anything *in
    /// flight* to wait for, so a caller with nothing running can skip that
    /// wait entirely (see `lib.rs`'s exit handling).
    ///
    /// The queue's in-flight run is not in `cancels`: `scheduler::QueueTask`
    /// keeps its `CancelSignal` to itself, and the only door its public
    /// surface leaves onto it is [`QueueHandle::stop`] — pause, then cancel
    /// whatever is running. That also persists `queue_state = paused`, which
    /// on a deliberate app exit is the same direction `QueueState`'s own
    /// default already takes (`scheduler::state`'s doc): a run the app just
    /// killed by quitting should not silently restart itself on the next
    /// launch without the user asking again.
    ///
    /// `queue.stop()` below is called whenever a queue is attached at all —
    /// **not** only when something happened to be in flight at this exact
    /// instant. The two are different questions: whether to persist "paused"
    /// is a policy decided once, on any quit; whether to wait for a process to
    /// actually die is a fact about this particular instant, which is what the
    /// returned `bool` still tracks. Gating the call itself on the fact would
    /// leave a queue that finished its last run 200ms before Cmd-Q resume the
    /// whole board unattended on the next launch — the same run that killed
    /// mid-flight `bool` already prevented, decided by nothing but timing.
    /// `QueueHandle::stop`'s own doc: pausing an already-idle queue is a
    /// cheap, ordinary write, not a special case to avoid.
    ///
    /// `lib.rs`'s `shut_down` calls [`QueueHandle::shutdown`] on `AppState`'s
    /// own handle *before* this — the switch that stops the loop from
    /// *starting* another task — so this only ever has to stop the one
    /// already running, never race a fresh claim.
    pub async fn cancel_all(&self) -> bool {
        let manual = {
            let cancels = self.inner.cancels.lock().expect("run registry poisoned");
            for cancel in cancels.values() {
                cancel.cancel();
            }
            !cancels.is_empty()
        };

        let queue_had_a_run_in_flight = match self.inner.queue.get() {
            Some(queue) => {
                let had_run_in_flight = queue.in_flight_task_id().is_some();
                if let Err(error) = queue.stop().await {
                    tracing::error!(%error, "could not stop the run queue while exiting");
                }
                had_run_in_flight
            }
            None => false,
        };

        manual || queue_had_a_run_in_flight
    }

    /// Whether any task is still claimed here, manual or the attached
    /// queue's own. Polled, bounded, by `lib.rs`'s shutdown handling while it
    /// waits for cancelled runs to actually exit.
    pub fn has_in_flight_runs(&self) -> bool {
        let manual = !self
            .inner
            .cancels
            .lock()
            .expect("run registry poisoned")
            .is_empty();
        manual
            || self
                .inner
                .queue
                .get()
                .is_some_and(|queue| queue.in_flight_task_id().is_some())
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
