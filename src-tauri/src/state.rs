use std::sync::{Arc, Mutex};

use rimaia_core::api::{BoardHost, Caller};
use rimaia_core::board::BoardPort;
use rimaia_core::doctor;
use rimaia_core::identity::SoloIdentity;
use rimaia_core::machine::MachineContext;
use rimaia_core::mcp::{LocalTools, McpHandle, RunHandles};
use rimaia_core::runner::strategy::PlannerAccess;
use rimaia_core::runner::{CancelSignal, RunnerConfig};
use rimaia_core::scheduler::InFlight;
use rimaia_core::{AppPaths, ServiceContext};
use rimaia_runner::queue::QueueHandle;
use rimaia_runner::RunnerStore;

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
/// of the same value `rimaia_runner::queue::build` was given. It used to be two maps that
/// could not see each other: a `cancels: HashMap` here for what a button
/// started, and a private `Option` inside the queue for what the scheduler
/// started, wired together by an `attach_queue` back-reference. That put the
/// rule "one process per task" in the shell, which ADR-0006 calls a bug
/// wherever it happens, and it is the reason ADR-0021 could not put
/// `plan_task_strategy` on the MCP surface — the MCP server cannot reach a
/// `src-tauri` type. Now every door reads one map, and this struct holds no
/// business rule at all.
///
/// The latest live-tail snapshot per run is the board host's tail relay
/// (`rimaia_core::api::TailRelay`, task 048), on [`board`](Self::board): a
/// server relays tails too, and `get_run_tail` is a board command answered
/// from it.
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
    /// The board's own context, unscoped to any one request.
    ///
    /// **A local handler never reads it** (seam-contract D32 point 8): when
    /// one needs board state it dispatches a board command or goes through a
    /// named `rimaia-core` read, so task 059, which sets [`board`](Self::board)
    /// to `None` on a connected desktop, has a list to convert rather than a
    /// search. The reads already here are those D32's 2026-10-04 amendment
    /// lists; add no new one.
    pub context: ServiceContext,
    /// The solo command host board commands are dispatched to (D32 point 3):
    /// `Some` in solo, the only mode before task 059, which sets `None` when a
    /// desktop is connected and its board commands go to the server.
    pub board: Option<SoloBoard>,
    /// The installation's solo team, user and runner (seam-contract D28 part
    /// 3), as `identity::ensure_solo` returned them at startup. `context` is
    /// already scoped to the team and acting for the user; this keeps the
    /// record itself, which task 046's `Caller::solo` reads (D32 point 7).
    pub solo: SoloIdentity,
    /// This machine's own store, `runner.db` (ADR-0028 point 3), opened and
    /// adopted into in `setup()`: for a runner-crate reader such as task 049's
    /// `runner_identity` read. Core code never sees this type; it reaches the
    /// same store through [`machine`](Self::machine).
    pub runner_store: RunnerStore,
    /// The machine context over [`runner_store`](Self::runner_store), with
    /// its own local channel (tasks 041 and 048): what every command and
    /// subsystem reads and writes this machine's own state through. The same
    /// value the queue and the MCP server were built with.
    pub machine: MachineContext,
    pub paths: AppPaths,
    pub in_flight: InFlight,
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
    /// The board port the commands start runs and planners through
    /// (seam-contract D31 point 8): a clone of the one `setup()` built over
    /// `runner`'s provider and handed to the queue and the MCP server too.
    ///
    /// `board_port` and not `board`, because D32 point 3 gives that name to
    /// the solo command host.
    pub board_port: Arc<dyn BoardPort>,
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
    /// rather than in `rimaia-core` because it is a property of *this
    /// window's* button, not of the planning rules, and
    /// `plan_all` takes the signal as an argument so the MCP surface — which
    /// has no Cancel button — hands in one nothing holds.
    ///
    /// A `Mutex` held only for the swap, never across an `await`.
    pub plan_pass: Mutex<Option<CancelSignal>>,
}

/// What `commands::board::route` dispatches a board command with in solo.
///
/// `machine` is the same [`MachineContext`] as [`AppState::machine`]: what lets
/// archiving, dragging to `done`, approving, rejecting and removing a
/// repository keep reacting on this machine in the same call (task 046 point
/// 3a). A server's host has none.
#[derive(Clone)]
pub struct SoloBoard {
    pub host: BoardHost,
    pub caller: Caller,
    pub machine: MachineContext,
}

/// This machine's MCP tools: the machine, a doctor over the shell's own paths
/// and runner, and the planner's access (task 041).
///
/// One function for the first bind in `setup()` and the rebind in
/// `commands::mcp::set_mcp_port`, so a rebind cannot quietly narrow what
/// `run_doctor` sees, leave the server unable to plan what it could plan a
/// moment ago, or drop this machine's tools.
pub fn local_tools(
    machine: &MachineContext,
    paths: &AppPaths,
    runner: &RunnerConfig,
    in_flight: &InFlight,
    board_port: &Arc<dyn BoardPort>,
    runner_id: &str,
) -> LocalTools {
    LocalTools {
        machine: machine.clone(),
        // The shell's own paths and runner, not `Programs::default`, so
        // `run_doctor` over MCP reports on the same `claude` binary and the
        // same data directory the window does (ADR-0021's parity).
        doctor: doctor::Environment::for_runner(paths.clone(), runner),
        // Task 023: everything a planner needs (the data directory, the
        // `claude` the runner would spawn and the one in-flight registry every
        // other door takes slots from) is reachable from `rimaia-core`.
        planner: PlannerAccess {
            paths: paths.clone(),
            runner: runner.clone(),
            in_flight: in_flight.clone(),
            board: Arc::clone(board_port),
            runner_id: runner_id.to_string(),
        },
    }
}

impl AppState {
    /// [`local_tools`] over this state's own values.
    pub fn local_tools(&self) -> LocalTools {
        local_tools(
            &self.machine,
            &self.paths,
            &self.runner,
            &self.in_flight,
            &self.board_port,
            &self.solo.runner_id,
        )
    }

    /// Asks every in-flight run to stop and reports whether there was anything
    /// to ask, so a caller with nothing running can skip the wait entirely.
    ///
    /// Two calls rather than one, and both are needed. `queue.stop()` persists
    /// `queue_state = paused` — a policy decided on *any* quit, not only on a
    /// quit that happened to catch a run mid-flight (seam-contract D15: "a run
    /// the app just killed by quitting should not silently restart itself on
    /// the next launch without the user asking again"). `in_flight.cancel_all`
    /// then reaches every run whoever started it, which `stop` deliberately
    /// does not: `stop` is scoped to the queue's own slots, because stopping
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
