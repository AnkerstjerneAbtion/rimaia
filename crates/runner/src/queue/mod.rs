//! The runner loop: the one long-lived task, and the handle the shell holds
//! (ADR-0010, ADR-0031 points 1 and 6, task 042).
//!
//! # The board decides what runs next; the runner decides whether it has room
//!
//! Before task 042 this loop read the board, planned with `selection`, picked
//! a task and then claimed the id it picked. A remote runner cannot do that: it
//! has no board to read, and ADR-0031 point 1 has the board choose, applying
//! eligibility a runner must not be trusted to apply. So the loop now says what
//! it has free, through `scheduler::view::for_runner`, and asks the board for
//! the next task with `BoardPort::claim(ClaimTarget::Next { .. })`. Selection
//! lives in `rimaia_core::board::service`, the claim's one body.
//!
//! What stays here is everything that is this machine's (ADR-0031 point 6):
//! the go signal, the usage-limit pause, capacity, run windows and schedules.
//! The rules for each stay in `rimaia-core` over the
//! [`MachineContext`] (task 041), because core's own local MCP handlers call
//! them too; this crate is the loop that obeys them. They are per runner by
//! construction: two loops over two machine stores have two switches, two
//! pauses and two sets of caps.
//!
//! The loop reaches the board through the port, apart from four reads in
//! [`solo`], which is the one file here that holds a board context.
//!
//! # One task, for the process lifetime
//!
//! [`build`] hands back a [`QueueHandle`] and the [`QueueTask`] that *is* the
//! queue. The caller spawns it — `tauri::async_runtime::spawn` in the shell,
//! `tokio::spawn` in a test — because a library function has no business
//! assuming which runtime it is inside, and because the handle has to exist
//! before the task does so the shell can wire a command to it in the same
//! `setup()` hook.
//!
//! # It never polls, and it sleeps for exactly one reason
//!
//! The loop does work until there is none, then waits on five things: its own
//! control signals; `ChangeEvent` — ADR-0018's channel, handed to [`build`] as
//! a receiver the shell subscribed; [`InFlight::releases`]; its own
//! [`JoinSet`]; and a **deadline**. A card dragged to the top of `ready`
//! publishes `Tasks`, the loop wakes, asks the board again and claims it.
//! There is still no interval to tune and nothing that costs a query while the
//! board is idle.
//!
//! The fifth arm exists because ADR-0011 introduced the one fact nothing
//! publishes: a `waiting_retry` task becomes due when a wall-clock instant
//! passes, and no mutation happens at that moment. Without it a task scheduled
//! to resume at 06:00 would wait for the next unrelated board change, which at
//! 06:00 is nobody. A `Next` claim that found nothing carries no deadline, so a
//! pass that ends idle asks [`SoloBoard::next_deadline`] for it (Scope point 4
//! of task 042, D31's 2026-10-10 amendment).
//!
//! **It waits on `Clock::sleep_until`, not on `tokio::time::sleep`.** The
//! deadline was computed against `Clock::now`, so the wait has to be, or a test
//! that advances a `TestClock` by fifteen minutes would sit through fifteen real
//! ones and CLAUDE.md's "no sleep in tests, ever" would quietly hold only for
//! the policy function.
//!
//! And the deadline it actually sleeps on is **capped at [`DEADLINE_CAP`]** —
//! which is not a poll interval, though it will look like one to anyone who
//! skims. A `tokio` timer measures elapsed *monotonic* time; a laptop suspended
//! at 23:10 and reopened at 06:30 has elapsed almost none of it, so a single
//! seven-hour timer would fire hours after the window it was waiting for
//! reopened. The cap makes the loop re-derive the answer from
//! `machine.clock.now()` shortly after each wake, which is the only reading of
//! the clock that survives a system sleep. A wake with nothing to do costs one
//! board read a minute at most, and only while something is genuinely waiting.
//!
//! Change events are drained *before* the board is asked, never after. An
//! event that arrives between the claim and the wait is then still buffered and
//! wakes it immediately; draining afterwards would throw away exactly the
//! notification that says the answer just went stale. The `releases` watch
//! needs no equivalent: `changed()` marks a generation seen only when it
//! *returns*, so a slot freed while the loop was busy is still there to be
//! found.
//!
//! # Every claim it makes is a non-blocking try
//!
//! The solo loop always sends `wait: ZERO`, and keeps the wake sources above
//! rather than letting the board wait. A waiting `Next` claim was issued with
//! the free capacity of the moment it started: with one free slot in A and none
//! in B, a run in B that finishes frees B's slot, but the waiting claim still
//! says B has none and sleeps through the ready task there. Dropping the
//! waiting claim and asking again would fix that. Since task 043 the claim is
//! one transaction, so a claim dropped before its commit writes nothing, but
//! one dropped after it committed still leaves a task `running` under a lease
//! nothing supervises, and a solo lease never expires. The long poll is task
//! 053's, once a lost reply is recovered by lease expiry.
//!
//! **No `select!` arm is ever a claim's future**, for the same reason: an arm
//! that lost the race would drop it.
//!
//! # The schedule timer is a third arm, not a second task (task 013)
//!
//! [`tick_schedules`](QueueTask::tick_schedules) runs inside this loop, and the
//! wake it needs is folded into the same deadline the retry arm already
//! computes. It is emphatically **not** a `tokio::spawn`ed timer calling
//! [`QueueHandle::start`], and there are three reasons, in order of weight:
//!
//! 1. **ADR-0010 makes the scheduler the only component allowed to move a task
//!    into `running`.** A separate timer task calling `start()` would be a
//!    second decider racing `try_step`'s own switch re-checks — the exact window
//!    the "a Pause, a Stop or a shutdown pressed mid-claim" section below was
//!    written to close, reopened from the other side.
//! 2. **ADR-0018's "another `subscribe()` and no coordination with anyone" is
//!    about *subscribers*.** A timer is not a subscriber. This is the same loop
//!    learning to wake on time as well as on events, which is what it already
//!    learned to do for ADR-0011's deadlines.
//! 3. **It costs one future** in a `select!` whose arms are already cancel-safe,
//!    and no new channel, no new task, and no new ordering constraint in the
//!    shell's `setup()`.
//!
//! The order inside the loop is shutdown check → [`drain`] → `tick_schedules` →
//! [`step`](QueueTask::step), and `tick_schedules` running **first** is
//! load-bearing rather than arbitrary: it **closes a window before anything
//! is claimed**, so a task cannot be claimed one millisecond after the window
//! it would have run in shut. Doing it the other way round would let every
//! pass that happened to land on the stop time start one more task.
//!
//! `tick_schedules` does three things, in this order. **Close first** — an open
//! window whose stop time has arrived is paused (ADR-0010: reaching the stop
//! time stops *starting*, and lets the in-flight run finish, so it is `pause`
//! and never `stop`). **Then fire** — the first due schedule in a stable order
//! runs task 018's doctor, writes the window, and flips the switch. **Otherwise
//! idle**, reporting the earliest instant anything is waiting for.
//!
//! A failure anywhere in it is logged and recorded on `last_step_error` exactly
//! as [`step`](QueueTask::step)'s is, and **never ends the loop**: a queue that
//! stopped for the night because one cron expression was hand-edited into
//! nonsense is the failure mode this whole product exists to prevent.
//!
//! # The `releases` arm is not optional
//!
//! `finish_run` publishes its `ChangeEvent`s from *inside* `run_task`, while
//! the slot is still held. A loop woken only by that channel therefore computes
//! its free capacity including the run that is finishing, finds none, and goes
//! back to sleep — with nothing left to wake it when the slot actually drops.
//! That is a queue that stalls at 2am with free slots and a full board. The
//! [`JoinSet`] arm does not cover it either, for the case that matters most: a
//! *manual* run freeing a slot is not a task this queue spawned, so nothing
//! joins.
//!
//! # One pass, in the order that spends nothing it does not have to
//!
//! 1. **Look without spending anything.** The switch, the pause and
//!    [`view::for_runner`]. Switched off, held, or with no free capacity, the
//!    pass ends: no probe and no claim.
//! 2. **The probe, memoised.** `probe_cli`'s last answer, success or failure,
//!    is reused until [`DEADLINE_CAP`] has passed on the injected clock, and
//!    [`QueueHandle::start`] and [`resume`](QueueHandle::resume) forget it,
//!    because the doctor they run has just asked the same question. A probe per
//!    pass would be the per-change spawn seam-contract D22 point 2 argues
//!    against, and the loop can no longer know whether anything is startable
//!    before it claims, which is what used to gate the probe.
//! 3. **Re-check the switch and shutdown**, because the probe awaited.
//! 4. **Then one claim at a time**: re-derive the view so the slots this pass
//!    already took are counted, claim `Next`, re-check the switch and shutdown,
//!    take the slot, spawn [`supervise`]. Claiming several before taking any
//!    slot would derive the capacity from slots not yet taken, over-claim, and
//!    release the surplus into `failed`.
//!
//! Sequential mode is not a separate path — it resolves to `global = 1` and
//! walks the same code.
//!
//! # A Pause, a Stop or a shutdown pressed mid-claim is not lost
//!
//! The slot is taken **after** the claim now, because the board picks the task
//! and the loop does not know which one it is getting until the claim returns.
//! What closes the window was never the slot: it is the re-check of the switch
//! and the shutdown signal after every await. A Stop that lands during the
//! probe writes `paused` and cancels nothing, because nothing is held yet; the
//! re-check after the probe then ends the pass before any claim, and the task
//! stays `idle`. One that lands during the claim is found by the re-check after
//! it, and the claim is released rather than a process spawned for a queue that
//! was told to stop (seam-contract D19's 2026-10-10 amendment).
//!
//! A slot refused after a won claim has two causes. **A capacity refusal**
//! means a Run now on this runner took the slot after the view was built: the
//! board decided on the capacity the runner reported, so the loop takes the
//! slot with `acquire_unbounded`, and the overshoot is one run a person started
//! on purpose, still under `CONCURRENCY_CEILING`. **`AlreadyInFlight`** means a
//! Run now for this very task sits between its slot and its own claim: the loop
//! releases, and that start then loses its claim and says so. A ceiling refusal
//! from `acquire_unbounded` also releases. `release` lands a claimed task in
//! `failed` (ADR-0007 has no softer edge), so every early exit between a claim
//! and `supervise` releases, except the capacity fallback.
//!
//! Manual starts keep task 036's order (`preview`, slot, `claim(Run)`), because
//! a person named the task.
//!
//! # Shutdown does not fight the exit path
//!
//! [`QueueHandle::shutdown`] stops the loop from starting anything new and lets
//! it end after the runs it started. It deliberately does **not** cancel them:
//! the app's exit path already asks every in-flight run to stop and waits for
//! it (SIGTERM, a grace period, then SIGKILL — ADR-0004), and a queue that
//! raced it would either kill a run twice or, worse, abandon the future
//! supervising it and lose the `finish_run` that turns the attempt into a
//! reviewable row. A run this queue abandoned would be indistinguishable from a
//! crash, and would come back as `interrupted` on the next launch for no
//! reason.
//!
//! **Which is why [`run`](QueueTask::run) ends by draining its [`JoinSet`]
//! rather than dropping it.** Dropping a `JoinSet` aborts every task still in
//! it, which is precisely the abandonment the paragraph above argues against —
//! and with N runs it is N of them. The drain is the last statement of the
//! function and sits behind no `?`, so there is no path out of the loop that
//! skips it.
//!
//! # The preflight is on the handle, not in the loop
//!
//! [`QueueHandle::start`] runs task 018's doctor and refuses, without writing
//! `queue_state`, when anything is failing. It is on the **handle** so that
//! every door — the Tauri command, any future MCP queue tool, task 013's
//! scheduled start — inherits the same refusal without each remembering to
//! ask. ADR-0006's "a rule enforced in only one adapter is a bug", made
//! structural. It is **not** in [`QueueTask::try_step`], deliberately: a doctor
//! per pass would spawn eight subprocesses per card drag, and a transient blip
//! would halt a queue mid-flight. The single check that genuinely must be in
//! the loop is the `claude` probe, and it is memoised. Seam-contract D22.
//!
//! # The queue does not own the in-flight map
//!
//! [`build`] takes an [`InFlight`] the shell also holds, and every door reads
//! the same map: the queue, Run now, Plan now and the MCP server's planner.
//! `Shared` keeps only what is genuinely the *queue's* — its control signals,
//! the probe memo and the last step error.

pub mod solo;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use rimaia_core::board::{BoardPort, Claim, ClaimTarget};
use rimaia_core::db::Schedule;
use rimaia_core::doctor;
use rimaia_core::events::ChangeEvent;
use rimaia_core::machine::{leases, MachineContext};
use rimaia_core::paths::AppPaths;
use rimaia_core::runner::start::record_claim;
use rimaia_core::runner::{probe_cli, run_task, RunRequest, RunnerConfig};
use rimaia_core::schedule::window::{self, RunWindow};
use rimaia_core::schedule::{self, fire, Due};
use rimaia_core::scheduler::inflight::{Capacity, InFlight, LocalSlot, SlotOwner, SlotRefused};
use rimaia_core::scheduler::state::{self, QueueState};
use rimaia_core::scheduler::view::{self, QueueStatus};
use rimaia_core::scheduler::{capacity, pause};
use rimaia_core::{Error, ErrorCode, Result};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinSet;

pub use solo::SoloBoard;

/// The longest the loop will sleep on one deadline before re-deriving it, and
/// the longest it reuses one answer from the `claude` probe.
///
/// **Not a poll interval** — see this module's header. It exists because a
/// `tokio` timer cannot be trusted as a wall-clock alarm across a system
/// suspend, and it costs nothing while nothing is waiting: with no deadline the
/// loop parks on its channels and this constant is never reached.
pub const DEADLINE_CAP: Duration = Duration::seconds(60);

/// The queue's control surface. Cheap to clone; every clone drives the same
/// loop, the same way the board's context behaves and for the same reason.
#[derive(Clone)]
pub struct QueueHandle {
    shared: Arc<Shared>,
}

/// The queue itself. Spawn [`run`](QueueTask::run) once, and only once.
pub struct QueueTask {
    shared: Arc<Shared>,
    /// The board's change channel, subscribed by whoever built this queue
    /// before anything could publish. Owned by the loop, because only the loop
    /// reads it.
    changes: broadcast::Receiver<ChangeEvent>,
}

/// Wires a queue: the handle to keep, and the task to spawn.
///
/// - `machine` is this machine's own state (task 041): the switch, the
///   capacity, the run window, the usage-limit pause and the schedules, all
///   read and written through it.
/// - `board` is where the queue claims, releases and has its runs report (D31
///   point 8). The shell builds one in `setup()` over the same provider as
///   `runner` and hands clones to everything that starts a process.
/// - `changes` is the board's change channel, subscribed by the caller. In
///   solo it is also the machine's: a `MachineContext` carries the board's
///   sender until task 048, so a schedule edited or a switch flipped wakes this
///   loop as a card moved does.
/// - `solo` is the four board reads the loop has no port method for yet.
/// - `in_flight` is passed rather than created here, because the shell needs
///   the same value for its own doors (D19 point 1): one value built in
///   `setup()` and handed to everything that needs it.
pub fn build(
    machine: MachineContext,
    board: Arc<dyn BoardPort>,
    changes: broadcast::Receiver<ChangeEvent>,
    solo: SoloBoard,
    in_flight: InFlight,
    paths: AppPaths,
    runner: RunnerConfig,
) -> (QueueHandle, QueueTask) {
    // The receiver is dropped immediately; the loop mints its own with
    // `subscribe`, which is what lets `build` be called before anything is
    // spawned.
    let (signals, _) = watch::channel(Signal::default());
    let shared = Arc::new(Shared {
        machine,
        board,
        solo,
        paths,
        runner,
        signals,
        in_flight,
        probe: Mutex::new(None),
        passes: AtomicU64::new(0),
        last_step_error: Mutex::new(None),
    });

    (
        QueueHandle {
            shared: Arc::clone(&shared),
        },
        QueueTask { shared, changes },
    )
}

/// Everything the Runs view asks the queue about, in one read: the runner's
/// half from `handle`, and the board's plan over the same repositories the
/// loop sends with every `Next` claim (`scheduler::view::for_runner`).
///
/// What `get_queue_status` answers, in the wire shape it has always had.
pub async fn status_with_plan(handle: &QueueHandle) -> Result<QueueStatus> {
    let shared = &handle.shared;
    let machine = &shared.machine;
    let (repositories, _) = view::for_runner(machine, &shared.in_flight).await?;
    Ok(QueueStatus {
        state: state::queue_state(machine).await?,
        running_task_ids: handle.in_flight_task_ids(),
        plan: shared.solo.plan(&repositories).await?,
        last_step_error: shared.step_error(),
        usage_limit_pause_until: pause::active_until(machine, machine.clock.now()).await?,
        window: window::active(machine).await?,
    })
}

impl QueueHandle {
    /// Starts working the board. Idempotent — starting a running queue writes
    /// the same row and wakes a loop that was not asleep.
    ///
    /// # The preflight refusal (task 018)
    ///
    /// Runs the doctor first and **refuses without writing `queue_state`**
    /// when anything is failing. Nothing is half-done on the refusal path: the
    /// switch is untouched, so a user who fixes the environment and presses
    /// Start again is starting from the same place, and a user who walks away
    /// has not left a queue that thinks it is running.
    ///
    /// It lives here, on the handle, rather than in either command surface, and
    /// that placement is the point: every door calls this method, so the
    /// refusal is identical on all of them *by construction*. ADR-0006's rule
    /// satisfied structurally. It is deliberately **not** in the loop's own
    /// `try_step` — see this module's header and seam-contract D22.
    pub async fn start(&self) -> Result<()> {
        let report = self
            .shared
            .solo
            .doctor(&self.shared.machine, &self.shared.doctor_environment())
            .await?;
        if report.is_blocking() {
            tracing::warn!(
                blocking = report.blocking().count(),
                "refusing to start the run queue: the preflight doctor is reporting failures",
            );
            return Err(Error::invalid(report.blocking_summary()));
        }

        // The doctor has just asked whether `claude` runs, so the loop's
        // remembered answer is older than the one a person is acting on.
        self.shared.forget_probe();
        // A human has acted and the environment has just been proved good, so
        // whatever a *scheduled* start failed with last night no longer
        // describes anything. Cleared here rather than left to the next
        // successful pass, because a pass would not clear it — see
        // `Shared::record_schedule_error` on why that reason is sticky.
        self.shared.clear_schedule_error();
        self.set(QueueState::Running).await
    }

    /// The same thing as [`start`](Self::start), under the name the user
    /// pressed.
    ///
    /// Two verbs for one write because ADR-0010's Control section names both
    /// and the difference is entirely in the button: "start" is the first one
    /// of the evening, "resume" is the one after a pause. A queue whose state
    /// is stored has no way to tell those apart, and no reason to.
    ///
    /// It inherits the preflight for free, and should: resuming after a pause
    /// is the same act of leaving the machine to work unattended, and the
    /// environment has had every opportunity to change during the pause.
    pub async fn resume(&self) -> Result<()> {
        self.start().await
    }

    /// Starts nothing new; lets the current run finish.
    ///
    /// **Closes the run window too** (task 013, seam-contract D15's amendment).
    /// Pause inside a window means pause, not "pause until the timer looks
    /// again" — and the timer would look again within the minute, because
    /// `tick_schedules` reads a window that is still open as a night still in
    /// progress. Leaving the window behind would make the Pause button undo
    /// itself.
    ///
    /// The schedule's *next* occurrence still fires. A window is one night; a
    /// schedule is the standing instruction that produces them.
    pub async fn pause(&self) -> Result<()> {
        self.set(QueueState::Paused).await?;
        window::close(&self.shared.machine).await
    }

    /// Pause, plus cancel whatever *the queue* is running.
    ///
    /// The switch is written **before** the cancellation, so the loop can never
    /// observe the run ending while the queue still reads `running` and pick up
    /// the next task. The cancelled run lands `failed` (ADR-0010: cancel-one on
    /// a running task "goes to `failed` with `cancelled` reason"), which is not
    /// a state the queue re-selects — so a stopped task stays stopped.
    ///
    /// Scoped to [`SlotOwner::Queue`] (D19 point 4). Stopping the queue is a
    /// statement about the queue, and a run the operator started by hand in
    /// front of them is not part of it.
    pub async fn stop(&self) -> Result<()> {
        self.pause().await?;
        if self.shared.in_flight.cancel_owned_by(SlotOwner::Queue) {
            tracing::info!("stopping the run queue's own in-flight runs");
        }
        Ok(())
    }

    /// The whole picture, for the Runs view. See [`status_with_plan`].
    pub async fn status(&self) -> Result<QueueStatus> {
        status_with_plan(self).await
    }

    /// Why the loop's last pass could not be completed, if it couldn't.
    ///
    /// The same value [`status`](Self::status) carries, reachable without the
    /// database reads that go with it. That is the whole reason it exists: the
    /// shell's notifier looks at this on every settings change, and reading the
    /// entire board to find out whether one string moved would be a board read
    /// per keystroke of the base-instructions textarea.
    pub fn last_step_error(&self) -> Option<String> {
        self.shared.step_error()
    }

    /// Every task this process has a `claude` child for right now.
    ///
    /// The one piece of queue state that is *not* stored, because it is not a
    /// fact about stored state: the row says `run_state = running` either way,
    /// and what this answers is "is the process on the end of it ours?".
    pub fn in_flight_task_ids(&self) -> Vec<String> {
        self.shared.in_flight.task_ids()
    }

    /// Whether `task_id` has a run in flight in this process, by either door.
    pub fn holds(&self, task_id: &str) -> bool {
        self.shared.in_flight.holds(task_id)
    }

    /// Ends the loop after the current run, without cancelling it. See this
    /// module's header on why those are separate.
    ///
    /// Synchronous and infallible: it is called from an exit path, where an
    /// `await` would be one more thing that can fail to happen.
    pub fn shutdown(&self) {
        self.shared.signals.send_modify(|signal| {
            signal.shutdown = true;
            signal.generation += 1;
        });
    }

    /// How many passes the loop has finished, whatever each came to.
    ///
    /// For a test that has to know a pass it released has run to its end
    /// before it asserts what that pass did — a negative has no event to wait
    /// for. Not part of the control surface.
    #[cfg(any(test, feature = "testing"))]
    pub fn completed_passes(&self) -> u64 {
        self.shared.passes.load(Ordering::SeqCst)
    }

    async fn set(&self, to: QueueState) -> Result<()> {
        state::set_queue_state(&self.shared.machine, to).await?;
        tracing::info!(
            state = to.as_str(),
            "the run queue was told to change state"
        );
        self.shared.wake();
        Ok(())
    }
}

impl QueueTask {
    /// Works the board until [`QueueHandle::shutdown`], or until the context
    /// that owns the change channel is gone, and then waits for every run it
    /// started.
    pub async fn run(self) {
        let QueueTask {
            shared,
            mut changes,
        } = self;
        let task = Looping { shared };
        task.run(&mut changes).await;
    }
}

/// The loop's own view of the shared state, once [`QueueTask::run`] has taken
/// its change receiver.
struct Looping {
    shared: Arc<Shared>,
}

impl Looping {
    async fn run(&self, changes: &mut broadcast::Receiver<ChangeEvent>) {
        let mut signals = self.shared.signals.subscribe();
        let mut releases = self.shared.in_flight.releases();
        // The runs this queue is supervising. Owned by the loop rather than by
        // `Shared`, because nothing outside the loop may join or abort them —
        // `QueueHandle::stop` speaks to the *slots*, which is a request the
        // run itself honours, not an abort that would lose its `finish_run`.
        let mut runs: JoinSet<()> = JoinSet::new();
        // Held once rather than reached for through the context on every
        // iteration: the timer future below outlives the borrow of `self` that
        // `select!` would otherwise need.
        let clock = Arc::clone(&self.shared.machine.clock);

        tracing::info!("the run queue is watching the board");

        loop {
            if self.shared.is_shutting_down() {
                break;
            }

            // Before the board is asked, never after — see this module's
            // header.
            drain(changes);

            // **Before `step`, never after.** A window whose stop time has
            // arrived is closed here, so the pass below cannot claim one more
            // task a millisecond after the night was supposed to end.
            let tick = self.tick_schedules().await;
            if tick == Step::Worked {
                continue;
            }

            let step = self.step(&mut runs).await;
            if step == Step::Worked {
                continue;
            }

            // The fifth wake source, now serving two questions: when a
            // `waiting_retry` task becomes due, and when a schedule fires or a
            // window closes. The earliest of them, because either changes the
            // answer and waking for one and sleeping through the other would be
            // the same bug twice.
            //
            // Capped before it is slept on — see this module's header on why
            // that is not a poll interval — and built fresh every iteration,
            // because both deadlines are re-derived from state that may have
            // changed while the loop was busy.
            let deadline = earliest(tick.deadline(), step.deadline())
                .map(|at| at.min(clock.now() + DEADLINE_CAP));
            let clock = Arc::clone(&clock);
            let due = async move {
                match deadline {
                    Some(at) => clock.sleep_until(at).await,
                    // A queue with nothing waiting parks on its channels, which
                    // is what keeps an idle night free of timers entirely.
                    None => std::future::pending().await,
                }
            };

            tokio::select! {
                // Every arm is cancel-safe, which is what lets this loop drop
                // whichever future did not win: `watch::Receiver::changed`
                // marks a value seen only once it returns,
                // `broadcast::Receiver::recv` holds its position in the
                // channel rather than in the future, and `JoinSet::join_next`
                // leaves an unfinished task in the set. No arm is a claim.
                changed = signals.changed() => if changed.is_err() { break },
                event = changes.recv() => if matches!(event, Err(RecvError::Closed)) { break },
                // A slot opened. See this module's header on why `ChangeEvent`
                // cannot carry this and why the arm below does not cover it.
                _ = releases.changed() => {},
                // A deadline passed — or the cap did. Either way the answer is
                // the same, and it is the one the next pass computes from
                // `machine.clock.now()`: this arm carries no information
                // beyond "look again".
                _ = due => {},
                // Guarded, because `join_next` on an empty set returns `None`
                // immediately — an unguarded arm would make this `select!` a
                // spin loop for the whole time the queue has nothing running,
                // which is most of the night.
                Some(joined) = runs.join_next(), if !runs.is_empty() => {
                    if let Err(error) = joined {
                        // `supervise` returns `()` and handles its own errors,
                        // so the only thing this can be is a panic inside it
                        // (or an abort, which nothing issues). The slot is
                        // already released — `Drop` runs on unwind — so the
                        // queue is not stuck; what would otherwise be lost is
                        // the fact that it happened at all.
                        tracing::error!(%error, "a run supervisor panicked");
                    }
                },
            }
        }

        // Not `drop(runs)`: dropping a `JoinSet` aborts its tasks, and a run
        // this queue abandoned is indistinguishable from a crash — see this
        // module's header. The last statement of the function, behind no `?`.
        tracing::info!(
            supervising = runs.len(),
            "the run queue has stopped starting tasks and is waiting for the runs it started",
        );
        while runs.join_next().await.is_some() {}
    }

    /// One pass: claim what there is room for and see each through, or find
    /// nothing to do.
    ///
    /// A failure here is logged and treated as "nothing to do" rather than
    /// ending the loop. An overnight queue that stopped because one read hit a
    /// locked database is a queue that did nothing all night for a reason
    /// nobody was awake to see; the next change event tries again.
    ///
    /// Also the one place `Shared`'s `last_step_error` is written: recorded on a
    /// failure, cleared on the next pass that gets all the way through — a
    /// `claude` that cannot be found used to fail exactly this way with nothing
    /// on [`QueueStatus`] to show for it.
    async fn step(&self, runs: &mut JoinSet<()>) -> Step {
        let step = match self.try_step(runs).await {
            Ok(step) => {
                self.shared.clear_step_error();
                step
            }
            Err(error) => {
                tracing::error!(%error, "the run queue could not take its next step");
                self.shared.record_step_error(error.to_string());
                Step::Idle
            }
        };
        self.shared.passes.fetch_add(1, Ordering::SeqCst);
        step
    }

    /// One look at the schedules: close a window that is over, fire one that is
    /// due, or report when to look again (task 013, ADR-0010's Triggering).
    ///
    /// A failure is logged and recorded exactly as [`step`](Self::step)'s is,
    /// and treated as "nothing to do" rather than ending the loop. One
    /// hand-edited cron expression must not be able to end a night — and unlike
    /// a step failure, nobody is awake to notice this one, which is precisely
    /// why it is recorded rather than only logged.
    async fn tick_schedules(&self) -> Step {
        match self.try_tick_schedules().await {
            Ok(step) => step,
            Err(error) => {
                tracing::error!(%error, "the run queue could not check its schedules");
                self.shared.record_step_error(error.to_string());
                Step::Idle
            }
        }
    }

    async fn try_tick_schedules(&self) -> Result<Step> {
        let machine = &self.shared.machine;
        let now = machine.clock.now();
        let open = window::active(machine).await?;

        // 1. Close first, before anything is claimed.
        if let Some(window) = &open {
            if window.has_closed(now) {
                return self.close_window(window).await;
            }
        }

        // 2. Fire. A stable order, because two schedules due in the same minute
        //    produce one window and *which* of them owns the night has to be
        //    the same answer on every pass and after every restart.
        let mut wake: Option<DateTime<Utc>> = open.as_ref().and_then(|window| window.closes_at);
        for schedule in schedule::enabled(machine).await? {
            // Per row, not per pass: one unreadable row must not stop every
            // other schedule being looked at. The row is named, because the
            // operator has to know which one to fix.
            let due = match fire::due(&schedule, now) {
                Ok(due) => due,
                Err(error) => {
                    tracing::error!(
                        schedule = %schedule.name, %error,
                        "a schedule could not be read; it will not fire until it is fixed",
                    );
                    continue;
                }
            };

            match due {
                Due::Fire {
                    occurrence,
                    closes_at,
                } => {
                    if let Some(window) = &open {
                        // Deliberately not a second window. `last_fired_at` is
                        // still written, so this occurrence is honoured and does
                        // not come round again the moment the other window
                        // closes — the schedule fired, it simply found the
                        // machine already working.
                        tracing::info!(
                            schedule = %schedule.name,
                            active = %window.schedule_name,
                            closes_at = ?window.closes_at,
                            "a schedule came due while another schedule's window is still open; \
                             not opening a second one",
                        );
                        schedule::record_fire(machine, &schedule.id, now).await?;
                        return Ok(Step::Worked);
                    }
                    return self
                        .open_window(&schedule, occurrence, closes_at, now)
                        .await;
                }

                // Due, and too late to matter. Nothing is written — see
                // `Due::Expired` on why lying to `last_fired_at` would be worse
                // than recomputing this — so the only thing to do is wait for
                // the next occurrence, which the wake below picks up.
                Due::Expired {
                    occurrence,
                    closed_at,
                } => tracing::info!(
                    schedule = %schedule.name,
                    occurrence = %occurrence.to_rfc3339(),
                    closed_at = %closed_at.to_rfc3339(),
                    "a schedule was due while the app was closed, but its run window has already \
                     ended; waiting for the next one",
                ),

                Due::NotDue => {}
            }

            // **Strictly future instants only.** `next_wake_at`, never
            // `next_fire_at`: the latter reports an overdue occurrence in the
            // past, and a deadline in the past resolves immediately, which would
            // turn this loop into a spin until morning.
            match fire::next_wake_at(&schedule, now) {
                Ok(next) => wake = earliest(wake, next),
                Err(error) => tracing::debug!(
                    schedule = %schedule.name, %error,
                    "a schedule has no next occurrence to wake for",
                ),
            }
        }

        Ok(match wake {
            Some(at) => Step::IdleUntil(at),
            None => Step::Idle,
        })
    }

    /// The stop time arrived: start nothing more, and let what is running
    /// finish.
    ///
    /// **`pause`, never `stop`.** ADR-0010 is explicit — "reaching it stops
    /// *starting* new tasks; in-flight runs are allowed to finish rather than
    /// being killed mid-edit" — and the difference is a run that was three
    /// minutes from a commit at 06:00.
    ///
    /// The switch is written **before** the window is cleared, for the same
    /// reason [`QueueHandle::stop`] writes it before cancelling: a crash between
    /// the two leaves a paused queue with a stale window, which the next tick
    /// tidies, rather than a running queue with no window, which would spend the
    /// morning working the board under the default configuration.
    async fn close_window(&self, window: &RunWindow) -> Result<Step> {
        let machine = &self.shared.machine;
        state::set_queue_state(machine, QueueState::Paused).await?;
        window::close(machine).await?;

        tracing::info!(
            schedule = %window.schedule_name,
            closes_at = ?window.closes_at,
            still_running = self.shared.in_flight.task_ids().len(),
            "the run window closed; starting nothing new and letting the current runs finish",
        );
        Ok(Step::Worked)
    }

    /// A schedule came due: check the environment, record what will happen, and
    /// flip the switch.
    ///
    /// # The doctor runs here, and a blocking report stops the night
    ///
    /// A scheduled start is the one nobody is watching, which is exactly task
    /// 018's case: "a broken environment is reported in the evening rather than
    /// discovered in the morning". A blocking report therefore **does not flip
    /// the switch**, records the blocking summary where the Runs view will show
    /// it, and logs at `error`.
    ///
    /// It **still writes `last_fired_at`**, and that is the part worth stating:
    /// without it the occurrence stays due, the next wake finds it again, and a
    /// missing `claude` becomes eight subprocess spawns a minute until morning.
    /// The schedule fired; what it found was a broken machine.
    async fn open_window(
        &self,
        schedule: &Schedule,
        occurrence: DateTime<Utc>,
        closes_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<Step> {
        let machine = &self.shared.machine;
        let solo = &self.shared.solo;

        let report = solo
            .doctor(machine, &self.shared.doctor_environment())
            .await?;
        if report.is_blocking() {
            schedule::record_fire(machine, &schedule.id, now).await?;
            let summary = report.blocking_summary();
            tracing::error!(
                schedule = %schedule.name,
                blocking = report.blocking().count(),
                summary = %summary,
                "a scheduled start was refused by the preflight doctor; the queue was not started",
            );
            self.shared.record_schedule_error(summary);
            // So the Runs view re-reads and shows it. `queue_state` was not
            // written, so nothing else on this path would have announced
            // anything at all. A machine event on the board's channel, under
            // `event_team`, until task 048 gives machine events `LocalEvents`.
            machine.publish(ChangeEvent::settings);
            return Ok(Step::Worked);
        }

        // The doctor just asked whether `claude` runs.
        self.shared.forget_probe();

        // Computed, never stored, and the same object the evening's Preview
        // button showed — so the log line a morning review reads and the
        // sentence the user read before leaving came from one function, over
        // the repositories the loop is about to claim for.
        let preflight = match view::for_runner(machine, &self.shared.in_flight).await {
            Ok((repositories, _)) => solo.preflight(machine, &schedule.id, &repositories).await,
            Err(error) => Err(error),
        };
        match preflight {
            Ok(summary) => tracing::info!(
                schedule = %schedule.name,
                will_start = summary.startable(),
                blocked = summary.blocked(),
                order = ?summary
                    .plan
                    .iter()
                    .filter(|entry| entry.skip.is_none())
                    .map(|entry| entry.title.as_str())
                    .collect::<Vec<_>>(),
                "a schedule fired",
            ),
            // A summary is a log line, not a precondition. Refusing to start the
            // night because the *description* of it could not be built would be
            // the preflight preventing the thing it exists to protect.
            Err(error) => tracing::warn!(
                schedule = %schedule.name, %error,
                "a schedule fired, but its preflight summary could not be built",
            ),
        }

        let window = RunWindow::opened_by(schedule, now, closes_at);
        window::open(machine, &window).await?;
        schedule::record_fire(machine, &schedule.id, now).await?;
        self.shared.clear_schedule_error();
        state::set_queue_state(machine, QueueState::Running).await?;

        tracing::info!(
            schedule = %window.schedule_name,
            occurrence = %occurrence.to_rfc3339(),
            closes_at = ?window.closes_at,
            mode = window.mode.as_str(),
            max_concurrency = window.max_concurrency,
            late_by_seconds = (now - occurrence).num_seconds(),
            "the run window is open",
        );
        Ok(Step::Worked)
    }

    async fn try_step(&self, runs: &mut JoinSet<()>) -> Result<Step> {
        let machine = &self.shared.machine;

        // 1. Look without spending anything.
        if state::queue_state(machine).await? != QueueState::Running {
            return Ok(Step::Idle);
        }

        // ADR-0011's hold on *this* runner, checked before anything is asked
        // of the board. Both modes therefore honour it by construction rather
        // than by each having a branch for it. In-flight runs are deliberately
        // untouched: a run mid-edit when *another* task hit a wall has done
        // nothing wrong, and this is a rule about starting.
        if let Some(until) = pause::active_until(machine, machine.clock.now()).await? {
            tracing::debug!(
                until = %until.to_rfc3339(),
                "the run queue is holding new starts until the usage window reopens",
            );
            return Ok(Step::IdleUntil(until));
        }

        // Read fresh every pass, never held: the operator may flip the mode or
        // raise a repository's cap at 23:00 with runs already in flight, and
        // the pass after that write is the one that has to notice.
        let mut asked_at = machine.clock.now();
        let (mut repositories, mut free) =
            view::for_runner(machine, &self.shared.in_flight).await?;
        if free.total == 0 {
            // Full. A full queue that also has a task due at 06:00 loses
            // nothing by waking then, and nothing else would wake it.
            return self.idle_until_due(&repositories, asked_at).await;
        }

        // 2. The probe, memoised. A missing `claude` is a property of this
        //    installation, not of a task, and claiming first would spend a
        //    night walking the board marking every task failed because it is
        //    not installed (task 008's "refused before any run state is
        //    written", worth as much to a queue as to a button).
        self.shared.probe().await?;

        // 3. The probe awaited, so look at the switch and shutdown again.
        //    Nothing is held yet, so there is nothing to release: a Stop that
        //    landed during the probe cancelled nothing and wrote `paused`.
        if self.interrupted().await? {
            return Ok(Step::Idle);
        }

        // 4. One claim at a time.
        let mut worked = false;
        while free.total > 0 {
            // The caps the slot is taken under, read before the claim so that
            // nothing between a won claim and its slot can fail on a read.
            let caps = capacity::resolve(machine).await?;
            let target = ClaimTarget::Next {
                capacity: free,
                repositories: repositories.clone(),
                // Always zero from the solo loop: see this module's header.
                wait: std::time::Duration::ZERO,
            };
            asked_at = machine.clock.now();
            let Some(claim) = self.shared.board.claim(target).await? else {
                break;
            };
            // Recorded before anything else can happen to the claim, so a
            // crash from here on is this runner's to reconcile (task 043).
            record_claim(self.shared.board.as_ref(), machine, &claim).await?;

            // Won, but a Pause, a Stop or a shutdown may have landed while the
            // claim was in flight: release what was just claimed rather than
            // spawn a process for a queue that was told to stop.
            match self.interrupted().await {
                Ok(false) => {}
                Ok(true) => {
                    release(self.shared.board.as_ref(), machine, &claim).await;
                    return Ok(if worked { Step::Worked } else { Step::Idle });
                }
                Err(error) => {
                    release(self.shared.board.as_ref(), machine, &claim).await;
                    return Err(error);
                }
            }

            let Some(slot) = self.take_slot(&claim, &caps).await else {
                // Released already; the board has moved on, so look again.
                worked = true;
                (repositories, free) = view::for_runner(machine, &self.shared.in_flight).await?;
                continue;
            };

            tracing::info!(
                task_id = %claim.lease.task_id,
                title = %claim.context.task.task.title,
                resuming = claim.resume.is_some(),
                "the run queue started a task",
            );
            let cancel = slot.cancel_signal();
            runs.spawn(supervise(
                Arc::clone(&self.shared.board),
                machine.clone(),
                self.shared.solo.clone(),
                self.shared.paths.clone(),
                self.shared.runner.clone(),
                slot,
                claim,
                RunRequest {
                    cancel,
                    // So two runs in one repository take turns creating their
                    // worktrees rather than racing `git worktree add` against
                    // one `.git` — see `InFlight::preparation_lock`.
                    in_flight: Some(self.shared.in_flight.clone()),
                },
            ));
            worked = true;

            // Re-derived, so the slot just taken is counted.
            (repositories, free) = view::for_runner(machine, &self.shared.in_flight).await?;
        }

        if worked {
            return Ok(Step::Worked);
        }
        self.idle_until_due(&repositories, asked_at).await
    }

    /// The slot for a claim the board granted, or `None` once the claim has
    /// been released because no slot can be had. See this module's header for
    /// the two causes of a refusal after a won claim.
    async fn take_slot(&self, claim: &Claim, caps: &capacity::Resolved) -> Option<LocalSlot> {
        let task_id = &claim.lease.task_id;
        let repository_id = &claim.context.repository.id;
        let in_flight = &self.shared.in_flight;

        let refused = match in_flight.acquire(
            task_id,
            repository_id,
            SlotOwner::Queue,
            Capacity {
                global: caps.global,
                per_repository: caps.for_repository(repository_id),
            },
        ) {
            Ok(slot) => return Some(slot),
            Err(refused) => refused,
        };

        let refused = match refused {
            // The board decided on the capacity this runner reported, and a Run
            // now took the slot since: one run over, started by a person on
            // purpose, still under the ceiling.
            SlotRefused::AtGlobalLimit { .. } | SlotRefused::AtRepositoryLimit { .. } => {
                match in_flight.acquire_unbounded(task_id, repository_id, SlotOwner::Queue) {
                    Ok(slot) => {
                        tracing::info!(
                            %task_id,
                            "a run started by hand took this slot after the board answered; \
                             starting the claimed task anyway",
                        );
                        return Some(slot);
                    }
                    Err(refused) => refused,
                }
            }
            // A Run now for this very task sits between its slot and its own
            // claim, which will now lose and say so.
            refused @ SlotRefused::AlreadyInFlight => refused,
        };

        tracing::warn!(
            %task_id,
            reason = %refused.message(),
            "releasing a claim the run queue could not take a slot for",
        );
        release(self.shared.board.as_ref(), &self.shared.machine, claim).await;
        None
    }

    /// The end of a pass that started nothing: wait for the world to change,
    /// or until a `waiting_retry` task becomes due, which nothing announces.
    ///
    /// `asked_at` is when the board was last asked: a deadline that passed
    /// after it is still returned, and wakes the loop at once, because the
    /// claim never saw that task due. One that had already passed is not,
    /// because the claim saw it and something else (a full repository) held it
    /// back, and waking for it again would be a spin until a slot frees — which
    /// the `releases` arm reports anyway.
    async fn idle_until_due(
        &self,
        repositories: &[String],
        asked_at: DateTime<Utc>,
    ) -> Result<Step> {
        Ok(
            match self
                .shared
                .solo
                .next_deadline(repositories, asked_at)
                .await?
            {
                Some(at) => Step::IdleUntil(at),
                None => Step::Idle,
            },
        )
    }

    /// Whether a Pause, a Stop or a shutdown landed since the pass began —
    /// looked at again after every await in [`try_step`](Self::try_step) that
    /// could hide one.
    ///
    /// `queue_state` is re-read rather than inferred from a cancel signal,
    /// because a plain Pause (unlike Stop) never touches one, and because
    /// before its slot is taken a queued run has no cancel signal at all.
    async fn interrupted(&self) -> Result<bool> {
        if self.shared.is_shutting_down() {
            return Ok(true);
        }
        Ok(state::queue_state(&self.shared.machine).await? != QueueState::Running)
    }
}

/// Sees one run through, and gives its slot back.
///
/// A free function taking owned clones rather than a method, and deliberately
/// with no access to `Shared`: it outlives the pass that spawned it and may
/// outlive the loop itself, so anything it could reach through `&self` would be
/// a lifetime the borrow checker has to be argued out of and a shared field two
/// concurrent supervisors could disagree over. Everything it needs is cheap to
/// clone and already designed to be.
///
/// The slot is dropped **last**, after `run_task` has returned and after any
/// release. Dropping it earlier would wake the loop while `finish_run`'s own
/// writes were still landing, and the pass it woke would read a board that had
/// not finished changing.
#[allow(clippy::too_many_arguments)]
async fn supervise(
    board: Arc<dyn BoardPort>,
    machine: MachineContext,
    solo: SoloBoard,
    paths: AppPaths,
    runner: RunnerConfig,
    slot: LocalSlot,
    claim: Claim,
    request: RunRequest,
) {
    let task_id = claim.lease.task_id.clone();
    let lease = claim.lease.clone();

    match run_task(
        board.as_ref(),
        &machine,
        solo.prepare_context(),
        &paths,
        &runner,
        claim,
        request,
    )
    .await
    {
        Ok(run) => tracing::info!(
            %task_id,
            run_id = %run.id,
            exit_class = ?run.exit_class,
            "the run queue finished a task",
        ),
        Err(error) => {
            tracing::error!(%task_id, %error, "a queued run could not be completed");
            // A backstop. `run_task` gives its claim back on every error path,
            // and then this lease is already gone, which the board answers
            // `Conflict`: nothing to release, and nothing to say about it.
            let released = board.release(&lease).await;
            match &released {
                Ok(()) => {}
                Err(error) if error.code() == ErrorCode::Conflict => {
                    tracing::debug!(%task_id, "the failed run's claim was already given back");
                }
                Err(error) => {
                    tracing::error!(%task_id, %error, "could not release a queued run that failed");
                }
            }
            leases::forget_released(&machine, &task_id, &released).await;
        }
    }

    drop(slot);
}

/// Gives back a claim the queue won and then decided not to use, and forgets
/// this runner's record of it. Best effort, as every release is.
async fn release(board: &dyn BoardPort, machine: &MachineContext, claim: &Claim) {
    let released = board.release(&claim.lease).await;
    if let Err(error) = &released {
        tracing::error!(task_id = %claim.lease.task_id, %error, "could not release a claim the queue did not use");
    }
    leases::forget_released(machine, &claim.lease.task_id, &released).await;
}

/// What one pass of the loop came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Something happened — a run was spawned, or a claim was given back. Look
    /// at the board again immediately. This is what makes the queue continuous:
    /// the next task starts as soon as there is room for it.
    Worked,
    /// Nothing to do. Wait to be woken.
    Idle,
    /// Nothing to do *yet*: something becomes startable at this instant and
    /// nothing will publish an event when it does.
    ///
    /// A separate variant rather than `Idle` carrying an `Option` because the
    /// two are genuinely different conclusions — "wait for the world to change"
    /// and "wait for the clock" — and a queue that conflated them would either
    /// arm a timer it never needs or sleep through a deadline.
    IdleUntil(DateTime<Utc>),
}

impl Step {
    fn deadline(self) -> Option<DateTime<Utc>> {
        match self {
            Step::IdleUntil(at) => Some(at),
            Step::Worked | Step::Idle => None,
        }
    }
}

/// What the control surface and the loop share.
struct Shared {
    /// This machine's own state: everything the queue decides with that is
    /// not on the board (task 041).
    machine: MachineContext,
    /// Where every claim, release and run report this queue makes goes.
    board: Arc<dyn BoardPort>,
    /// The four board reads the loop has no port method for yet, and task
    /// 044's temporary context.
    solo: SoloBoard,
    /// Where state lives. Read by the loop when it starts a run, and by
    /// [`QueueHandle::start`]'s preflight, which asks whether it is writable
    /// and how much room is left on it.
    paths: AppPaths,
    /// The one runner configuration every run this queue starts is worked
    /// from. The preflight reads two things off it that must not be guessed:
    /// `program`, so the doctor probes the same `claude` the loop will spawn,
    /// and `run_handles`, for the endpoint the MCP server actually bound.
    runner: RunnerConfig,
    signals: watch::Sender<Signal>,
    /// The shell holds a clone of the same registry, and every door reads it.
    in_flight: InFlight,
    /// The `claude` probe's last answer and when it was taken.
    probe: Mutex<Option<ProbeAnswer>>,
    /// Passes finished, for [`QueueHandle::completed_passes`].
    passes: AtomicU64,
    /// The reason the queue is not doing what the switch says, if there is one.
    /// Written only in [`Looping::step`] and [`Looping::tick_schedules`], never
    /// inside `try_step` itself.
    last_step_error: Mutex<Option<StepError>>,
}

/// One answer from `probe_cli`, success or failure, and the instant on the
/// injected clock it was taken.
#[derive(Debug, Clone)]
struct ProbeAnswer {
    taken_at: DateTime<Utc>,
    /// The refusal's sentence on failure. `probe_cli` only ever refuses with
    /// `Error::Invalid`, so the sentence is the whole error.
    answer: std::result::Result<(), String>,
}

/// A recorded reason, and how long it outlives the pass that recorded it.
///
/// Two lifetimes, because there are genuinely two kinds of reason. An ordinary
/// step failure — a locked database, a `claude` that could not be probed — is
/// true of *one pass*, and the next pass that gets all the way through has
/// disproved it. A **scheduled start refused by the doctor** is not: the queue it
/// refused to start is `paused`, so `try_step` returns immediately at its switch
/// check and every subsequent pass would "succeed" without having proved
/// anything at all. Left non-sticky, the message the user is meant to find in
/// the morning would be cleared microseconds after it was written, by a pass
/// that did nothing.
#[derive(Debug, Clone)]
struct StepError {
    message: String,
    /// Whether an ordinary successful pass may clear it. Cleared instead by the
    /// next successful fire, or by a human pressing Start.
    sticky: bool,
}

impl Shared {
    /// The preflight's view of this installation, built from what the queue was
    /// wired with rather than from defaults — so the doctor answers about the
    /// binary and the endpoint this queue would actually use.
    fn doctor_environment(&self) -> doctor::Environment {
        doctor::Environment::for_runner(self.paths.clone(), &self.runner)
    }

    /// `probe_cli`, at most once per [`DEADLINE_CAP`] of clock time.
    ///
    /// The answer is reused, success or failure, while it is younger than the
    /// cap, so with free capacity a board change costs at most one `--version`
    /// spawn a minute whether or not anything is ready (seam-contract D22's
    /// 2026-10-10 amendment). A `claude` removed while the queue runs is
    /// noticed within the same minute, and one claimed task can fail at spawn
    /// before it is — `run_task` probes again before it spawns.
    async fn probe(&self) -> Result<()> {
        let now = self.machine.clock.now();
        let remembered = self
            .probe
            .lock()
            .expect("queue probe lock poisoned")
            .clone();
        if let Some(ProbeAnswer { taken_at, answer }) = remembered {
            if taken_at <= now && now < taken_at + DEADLINE_CAP {
                return answer.map_err(Error::invalid);
            }
        }

        let answer = probe_cli(self.runner.provider.as_ref(), &self.runner.program)
            .await
            .map(drop)
            .map_err(|error| error.to_string());
        *self.probe.lock().expect("queue probe lock poisoned") = Some(ProbeAnswer {
            taken_at: now,
            answer: answer.clone(),
        });
        answer.map_err(Error::invalid)
    }

    /// Forgets the probe's answer, for the two moments a doctor has just asked
    /// the same question.
    fn forget_probe(&self) {
        *self.probe.lock().expect("queue probe lock poisoned") = None;
    }

    fn wake(&self) {
        self.signals.send_modify(|signal| signal.generation += 1);
    }

    fn is_shutting_down(&self) -> bool {
        self.signals.borrow().shutdown
    }

    fn record_step_error(&self, error: String) {
        self.set_step_error(Some(StepError {
            message: error,
            sticky: false,
        }));
    }

    /// Records why a *scheduled* start did not happen, in a way an ordinary
    /// pass will not erase. See [`StepError`].
    fn record_schedule_error(&self, error: String) {
        self.set_step_error(Some(StepError {
            message: error,
            sticky: true,
        }));
    }

    /// Clears what one pass proved wrong, and only that.
    fn clear_step_error(&self) {
        let mut held = self
            .last_step_error
            .lock()
            .expect("queue step-error lock poisoned");
        if held.as_ref().is_none_or(|error| !error.sticky) {
            *held = None;
        }
    }

    /// Clears everything, including a sticky refusal — for the two things that
    /// genuinely supersede one: a fire that got through, and a human pressing
    /// Start.
    fn clear_schedule_error(&self) {
        self.set_step_error(None);
    }

    fn set_step_error(&self, error: Option<StepError>) {
        *self
            .last_step_error
            .lock()
            .expect("queue step-error lock poisoned") = error;
    }

    fn step_error(&self) -> Option<String> {
        self.last_step_error
            .lock()
            .expect("queue step-error lock poisoned")
            .as_ref()
            .map(|error| error.message.clone())
    }
}

/// Why the control channel is a `watch` and not a `Notify`.
///
/// A watch retains its value, so a Pause that lands while the loop is busy
/// supervising a run is still there to be found when it next looks — the same
/// argument `runner::process::CancelSignal` makes for itself. `generation`
/// exists because "wake up" is not a state anyone can compare: bumping it is
/// what makes `changed()` fire for a control call that did not alter
/// `shutdown`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Signal {
    generation: u64,
    shutdown: bool,
}

/// The earlier of two instants, either of which may be absent.
///
/// The loop has two independent reasons to wake on a clock — a retry deadline
/// and a schedule — and waking for the earlier is the only answer that serves
/// both. `Option::min` would be wrong in the obvious way: `None` sorts below
/// `Some`, so a queue with one deadline and one absent would arm nothing.
fn earliest(left: Option<DateTime<Utc>>, right: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (found, None) | (None, found) => found,
    }
}

/// Throws away every change event already buffered.
///
/// A run of its own publishes a handful — the claim, the `runs` row, the
/// outcome, the board move — and none of them says anything the claim that
/// follows will not see. Draining costs one pass instead of one per event, and
/// `Lagged` is drained too: on this channel it means the same thing every other
/// event does, "look again", which is what the caller is about to do anyway.
fn drain(changes: &mut broadcast::Receiver<ChangeEvent>) {
    loop {
        match changes.try_recv() {
            Ok(_) | Err(TryRecvError::Lagged(_)) => continue,
            Err(TryRecvError::Empty | TryRecvError::Closed) => return,
        }
    }
}

/// **Unix only**, and it is the preflight that makes it so.
///
/// `QueueHandle::start` runs the doctor, which spawns the `claude` binary — so
/// every test below needs a stand-in, and the stand-in is a `/bin/sh` shebang
/// script (`testing::doctor::passing_queue_environment`). Windows has no
/// shebang. Gated whole rather than test by test, because a module that
/// compiled and ran nothing would report a green Windows job that had checked
/// none of this.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rimaia_core::testing::TestContext;
    use tempfile::TempDir;

    /// A queue whose preflight can actually pass.
    ///
    /// The [`TempDir`] is returned rather than dropped on the way out, and every
    /// caller binds it: it owns both the app directory `data_directory` reports
    /// on and the stand-in binary the two `claude` checks spawn, so letting it
    /// drop here would delete the environment the gate is about to inspect.
    fn queue(harness: &TestContext) -> (TempDir, QueueHandle, QueueTask) {
        let (root, paths, runner) = rimaia_core::testing::doctor::passing_queue_environment();
        let (handle, task) = build(
            harness.machine().clone(),
            harness.board(&paths, &runner),
            harness.context.subscribe(),
            SoloBoard::new(
                harness.context.clone(),
                harness.solo.runner_id.clone(),
                rimaia_core::runner::provider::ProviderId::ClaudeCode,
            ),
            InFlight::new(),
            paths,
            runner,
        );
        (root, handle, task)
    }

    #[tokio::test]
    async fn the_control_verbs_write_the_switch_the_next_launch_reads() {
        let harness = TestContext::new().await;
        let (_root, handle, _task) = queue(&harness);

        handle.start().await.expect("start the queue");
        assert_eq!(
            state::queue_state(harness.machine()).await.expect("read"),
            QueueState::Running
        );

        handle.pause().await.expect("pause the queue");
        assert_eq!(
            state::queue_state(harness.machine()).await.expect("read"),
            QueueState::Paused
        );

        handle.resume().await.expect("resume the queue");
        assert_eq!(
            state::queue_state(harness.machine()).await.expect("read"),
            QueueState::Running
        );

        handle.stop().await.expect("stop the queue");
        assert_eq!(
            state::queue_state(harness.machine()).await.expect("read"),
            QueueState::Paused,
            "stop is pause plus a cancellation, not a third state"
        );
    }

    #[tokio::test]
    async fn stopping_a_queue_with_nothing_in_flight_is_not_an_error() {
        // The Stop button is pressed by a human who cannot see whether the
        // current run finished half a second ago.
        let harness = TestContext::new().await;
        let (_root, handle, _task) = queue(&harness);

        handle.stop().await.expect("stop an idle queue");

        assert!(handle.in_flight_task_ids().is_empty());
    }

    #[tokio::test]
    async fn a_control_call_wakes_a_loop_that_is_already_awake() {
        // `send_modify` always marks the value changed, which is what makes a
        // wake that lands between two polls survive to the next one.
        let harness = TestContext::new().await;
        let (_root, handle, task) = queue(&harness);
        let signals = task.shared.signals.subscribe();

        handle.shared.wake();

        assert!(signals.has_changed().expect("the sender is alive"));
    }

    #[tokio::test]
    async fn shutdown_is_visible_to_the_loop_without_awaiting_anything() {
        let harness = TestContext::new().await;
        let (_root, handle, task) = queue(&harness);
        assert!(!task.shared.is_shutting_down());

        handle.shutdown();

        assert!(task.shared.is_shutting_down());
    }

    #[tokio::test]
    async fn draining_leaves_the_channel_empty_without_blocking_on_it() {
        let harness = TestContext::new().await;
        let mut changes = harness.context.subscribe();

        for id in 0..3 {
            harness.context.publish(ChangeEvent::tasks(
                harness.solo.team_id.clone(),
                [id.to_string()],
            ));
        }
        drain(&mut changes);

        assert_eq!(changes.try_recv().unwrap_err(), TryRecvError::Empty);
    }
}
