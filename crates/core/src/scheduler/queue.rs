//! The one long-lived task, and the handle the shell holds (ADR-0010).
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
//! # It never polls and never sleeps
//!
//! The loop does work until there is none, then waits on two things: its own
//! control signals, and [`ChangeEvent`](crate::ChangeEvent) — ADR-0018's
//! channel, whose Consequences already anticipated this subscriber ("adding the
//! scheduler's own view later is another `subscribe()` and no coordination with
//! anyone"). A card dragged to the top of `ready` publishes `Tasks`, the loop
//! wakes, re-reads the board and claims it. There is no interval to tune and
//! nothing that costs a query while the board is idle.
//!
//! Change events are drained *before* the board is read, never after. An event
//! that arrives between the read and the wait is then still buffered and wakes
//! it immediately; draining afterwards would throw away exactly the
//! notification that says the plan just went stale.
//!
//! # Shutdown does not fight the exit path
//!
//! [`QueueHandle::shutdown`] stops the loop from starting anything new and lets
//! it end after the current run. It deliberately does **not** cancel that run:
//! the app's exit path already asks every in-flight run to stop and waits for
//! it (SIGTERM, a grace period, then SIGKILL — ADR-0004), and a queue that
//! raced it would either kill a run twice or, worse, abandon the future
//! supervising it and lose the `finish_run` that turns the attempt into a
//! reviewable row. A run this queue abandoned would be indistinguishable from a
//! crash, and would come back as `interrupted` on the next launch for no
//! reason. So the loop waits for the run it started, and simply does not start
//! another.
//!
//! # Nothing here decides anything twice
//!
//! Eligibility is [`selection`]'s, the claim is [`claim`]'s, the switch is
//! [`state`]'s, and supervising a process is
//! [`runner::run_task`](crate::runner::run_task)'s. What is left — and all that
//! is left — is the order those happen in.
//!
//! # A Pause, a Stop or a shutdown pressed mid-claim is not lost
//!
//! `try_step` registers `Shared`'s in-flight entry — the one thing
//! [`QueueHandle::stop`] has to signal, and the one thing that makes
//! [`QueueHandle::in_flight_task_id`] true — **before** the `claude`
//! prerequisite check and the claim itself, not after either succeeds. Both
//! await, so without this a Pause, a Stop or [`QueueHandle::shutdown`]
//! pressed in that window found nothing cancellable and nothing that
//! disagreed with a switch still reading `running`, and the loop claimed and
//! ran the task anyway. `try_step` re-checks the switch and the signal at the
//! two points that window used to hide from — right after the probe, and
//! right after the claim succeeds — releasing a claim it already won rather
//! than spawn a process nobody asked for any more.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::{broadcast, watch};

use crate::context::ServiceContext;
use crate::db::MutationSource;
use crate::error::Result;
use crate::events::ChangeEvent;
use crate::paths::AppPaths;
use crate::runner::{probe_cli, run_task, CancelSignal, RunRequest, RunTrigger, RunnerConfig};
use crate::scheduler::claim::{self, ClaimOutcome};
use crate::scheduler::selection::{self, QueueEntry};
use crate::scheduler::state::{self, QueueState};

/// Everything the Runs view asks the queue about, in one read.
///
/// Assembled from the database and from the one thing that genuinely is not in
/// it — which process this queue is supervising *right now*. Everything else
/// survives a restart because it was never held here in the first place.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueStatus {
    pub state: QueueState,
    /// The task whose process the queue is supervising. `None` when it is
    /// between runs, paused, or has nothing to do.
    pub running_task_id: Option<String>,
    /// Every `ready` task in board order, with the reason the queue will pass
    /// over each one it cannot start, and its queue position when it will.
    pub plan: Vec<QueueEntry>,
    /// Why the loop's last pass could not be completed, if it couldn't.
    /// `None` once a later pass gets all the way through.
    ///
    /// The one failure `selection`'s own `SkipReason` cannot name: a missing
    /// `claude` fails `probe_cli` before any task is even chosen, so nothing
    /// on the board explains it. Without this, that failure was invisible —
    /// `state` still read `running` and `plan` still listed a full night's
    /// work, with nothing to say why none of it was happening.
    pub last_step_error: Option<String>,
}

/// The queue's control surface. Cheap to clone; every clone drives the same
/// loop, the same way [`ServiceContext`] behaves and for the same reason.
#[derive(Clone)]
pub struct QueueHandle {
    shared: Arc<Shared>,
}

/// The queue itself. Spawn [`run`](QueueTask::run) once, and only once.
pub struct QueueTask {
    shared: Arc<Shared>,
    paths: AppPaths,
    runner: RunnerConfig,
}

/// Wires a queue: the handle to keep, and the task to spawn.
pub fn build(
    ctx: ServiceContext,
    paths: AppPaths,
    runner: RunnerConfig,
) -> (QueueHandle, QueueTask) {
    // Every write this queue makes is the machine's, not the user's
    // (ADR-0019). Re-sourced here, once, rather than at each service call
    // inside the loop: the shell hands one `Ui` context to the scheduler and
    // to the MCP server alike, and each subsystem is what decides what its own
    // writes are attributed to. The clone keeps the original's channels — see
    // `ServiceContext::with_source`.
    let ctx = ctx.with_source(MutationSource::System);

    // The receiver is dropped immediately; the loop mints its own with
    // `subscribe`, which is what lets `build` be called before anything is
    // spawned.
    let (signals, _) = watch::channel(Signal::default());
    let shared = Arc::new(Shared {
        ctx,
        signals,
        in_flight: Mutex::new(None),
        last_step_error: Mutex::new(None),
    });

    (
        QueueHandle {
            shared: Arc::clone(&shared),
        },
        QueueTask {
            shared,
            paths,
            runner,
        },
    )
}

impl QueueHandle {
    /// Starts working the board. Idempotent — starting a running queue writes
    /// the same row and wakes a loop that was not asleep.
    pub async fn start(&self) -> Result<()> {
        self.set(QueueState::Running).await
    }

    /// The same thing as [`start`](Self::start), under the name the user
    /// pressed.
    ///
    /// Two verbs for one write because ADR-0010's Control section names both
    /// and the difference is entirely in the button: "start" is the first one
    /// of the evening, "resume" is the one after a pause. A queue whose state
    /// is derived from the database has no way to tell those apart, and no
    /// reason to.
    pub async fn resume(&self) -> Result<()> {
        self.start().await
    }

    /// Starts nothing new; lets the current run finish.
    pub async fn pause(&self) -> Result<()> {
        self.set(QueueState::Paused).await
    }

    /// Pause, plus cancel whatever is running.
    ///
    /// The switch is written **before** the cancellation, so the loop can never
    /// observe the run ending while the queue still reads `running` and pick up
    /// the next task. The cancelled run lands `failed` (ADR-0010: cancel-one on
    /// a running task "goes to `failed` with `cancelled` reason"), which is not
    /// a state the queue re-selects — so a stopped task stays stopped.
    pub async fn stop(&self) -> Result<()> {
        self.pause().await?;
        if let Some(in_flight) = self.shared.in_flight() {
            tracing::info!(task_id = %in_flight.task_id, "stopping the queue's current run");
            in_flight.cancel.cancel();
        }
        Ok(())
    }

    /// The whole picture, for the Runs view.
    pub async fn status(&self) -> Result<QueueStatus> {
        Ok(QueueStatus {
            state: state::queue_state(&self.shared.ctx.pool).await?,
            running_task_id: self.in_flight_task_id(),
            plan: selection::plan(&self.shared.ctx).await?,
            last_step_error: self.shared.step_error(),
        })
    }

    /// The task this queue has a process for right now.
    ///
    /// The one piece of queue state that is *not* in the database, because it
    /// is not a fact about stored state: the row says `run_state = running`
    /// either way, and what this answers is "is the process on the end of it
    /// mine?".
    pub fn in_flight_task_id(&self) -> Option<String> {
        self.shared
            .in_flight()
            .map(|in_flight| in_flight.task_id.clone())
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

    async fn set(&self, to: QueueState) -> Result<()> {
        state::set_queue_state(&self.shared.ctx, to).await?;
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
    /// that owns the change channel is gone.
    pub async fn run(self) {
        let mut signals = self.shared.signals.subscribe();
        let mut changes = self.shared.ctx.subscribe();

        tracing::info!("the run queue is watching the board");

        loop {
            if self.shared.is_shutting_down() {
                break;
            }

            // Before the board read, never after — see this module's header.
            drain(&mut changes);

            if self.step().await == Step::Worked {
                continue;
            }

            tokio::select! {
                // Both are cancel-safe, which is what lets this loop drop
                // whichever future did not win: `watch::Receiver::changed`
                // marks a value seen only once it returns, and
                // `broadcast::Receiver::recv` holds its position in the
                // channel rather than in the future.
                changed = signals.changed() => if changed.is_err() { break },
                event = changes.recv() => if matches!(event, Err(RecvError::Closed)) { break },
            }
        }

        tracing::info!("the run queue has stopped");
    }

    /// One pass: claim the next task and see it through, or find nothing to do.
    ///
    /// A failure here is logged and treated as "nothing to do" rather than
    /// ending the loop. An overnight queue that stopped because one board read
    /// hit a locked database is a queue that did nothing all night for a reason
    /// nobody was awake to see; the next change event tries again.
    ///
    /// Also the one place [`Shared`]'s `last_step_error` is written: recorded
    /// on a failure, cleared on the next pass that gets all the way through —
    /// a `claude` that cannot be found used to fail exactly this way with
    /// nothing on [`QueueStatus`] to show for it, so the Runs view read
    /// "Running" over a full plan while nothing happened all night.
    async fn step(&self) -> Step {
        match self.try_step().await {
            Ok(step) => {
                self.shared.clear_step_error();
                step
            }
            Err(error) => {
                tracing::error!(%error, "the run queue could not take its next step");
                self.shared.record_step_error(error.to_string());
                Step::Idle
            }
        }
    }

    async fn try_step(&self) -> Result<Step> {
        let ctx = &self.shared.ctx;

        if state::queue_state(&ctx.pool).await? != QueueState::Running {
            return Ok(Step::Idle);
        }

        let plan = selection::plan(ctx).await?;
        let Some(entry) = selection::next_to_start(&plan) else {
            return Ok(Step::Idle);
        };
        let task_id = entry.task_id.clone();

        // Registered *before* the prerequisite check and the claim below, not
        // after either succeeds — see this module's header amendment on the
        // window that left open otherwise. A Pause, a Stop or a shutdown
        // pressed anywhere from here on now has a `CancelSignal` to act on
        // (`QueueHandle::stop`) and a switch this function re-checks itself,
        // rather than landing in the gap where `stop` found nothing to
        // cancel and nothing here noticed the switch had changed underneath
        // it. `finish()` on every path back out below is what keeps this
        // from leaving `in_flight` — and so `running_task_id` — pointing at a
        // task nothing is actually doing anything about.
        let cancel = CancelSignal::new();
        self.shared.begin(&task_id, cancel.clone());

        // The prerequisite, before the claim rather than after it — task 008's
        // acceptance criterion ("a missing binary is refused before any run
        // state is written") is worth as much to a queue as to a button. A
        // queue that claimed first would spend a night walking the board
        // marking every task failed because `claude` is not installed.
        if let Err(error) = probe_cli(&self.runner.program).await {
            self.shared.finish();
            return Err(error);
        }

        if self.interrupted_since(&cancel).await? {
            // Nothing has been claimed yet, so there is nothing to release —
            // just stop before spending a claim on a task nobody wants
            // started any more.
            self.shared.finish();
            return Ok(Step::Idle);
        }

        let claimed = match claim::claim(ctx, &task_id).await {
            Ok(outcome) => outcome,
            Err(error) => {
                self.shared.finish();
                return Err(error);
            }
        };
        if claimed == ClaimOutcome::Lost {
            // Somebody else has it. The board says something different from
            // what it said a moment ago, so look again rather than waiting.
            self.shared.finish();
            return Ok(Step::Worked);
        }

        if self.interrupted_since(&cancel).await? {
            // Won the claim, but a Pause, a Stop or a shutdown landed while
            // it was in flight: release what was just claimed rather than
            // spawn a process for a queue that was told to stop before this
            // one started.
            claim::release(ctx, &task_id).await;
            self.shared.finish();
            return Ok(Step::Idle);
        }

        tracing::info!(%task_id, title = %entry.title, "the run queue started a task");

        // Awaited, not raced against shutdown: see this module's header.
        let outcome = run_task(
            ctx,
            &self.paths,
            &self.runner,
            RunRequest {
                task_id: task_id.clone(),
                // ADR-0012: the unattended path, behind the per-repository
                // opt-in `selection` already checked.
                trigger: RunTrigger::Queued,
                cancel,
            },
        )
        .await;
        self.shared.finish();

        match outcome {
            Ok(run) => tracing::info!(
                %task_id,
                run_id = %run.id,
                exit_class = ?run.exit_class,
                "the run queue finished a task",
            ),
            Err(error) => {
                tracing::error!(%task_id, %error, "a queued run could not be completed");
                claim::release(ctx, &task_id).await;
            }
        }

        Ok(Step::Worked)
    }

    /// Whether a Pause, a Stop or a shutdown landed since `cancel` was
    /// registered — looked at again at the two points in [`try_step`] that
    /// used to assume nothing could, because nothing had a way to say so yet.
    ///
    /// `queue_state` is re-read rather than inferred from `cancel`, because a
    /// plain Pause (unlike Stop) never touches a [`CancelSignal`] at all —
    /// only `QueueHandle::stop` does. Checking the switch here is what makes
    /// Pause, not only Stop, effective inside this window.
    async fn interrupted_since(&self, cancel: &CancelSignal) -> Result<bool> {
        if cancel.is_cancelled() || self.shared.is_shutting_down() {
            return Ok(true);
        }
        Ok(state::queue_state(&self.shared.ctx.pool).await? != QueueState::Running)
    }
}

/// What one pass of the loop came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Something happened. Look at the board again immediately — this is what
    /// makes the queue sequential *and* continuous: the next task starts as
    /// soon as the previous one reached a terminal state, and the board it is
    /// chosen from is re-read rather than remembered.
    Worked,
    /// Nothing to do. Wait to be woken.
    Idle,
}

/// What the control surface and the loop share.
struct Shared {
    ctx: ServiceContext,
    signals: watch::Sender<Signal>,
    /// `std::sync::Mutex` rather than tokio's: it is only ever held across two
    /// field accesses, never across an `await`, and a lock that cannot be held
    /// across a yield point is one fewer thing to reason about.
    in_flight: Mutex<Option<InFlight>>,
    /// The reason [`QueueTask::step`]'s last pass could not be completed, if
    /// it couldn't. Its own lock, separate from `in_flight`'s: the two are
    /// written at different points of a pass (this one only in `step`, never
    /// inside `try_step` itself) and reading one has no business waiting on
    /// the other.
    last_step_error: Mutex<Option<String>>,
}

impl Shared {
    fn wake(&self) {
        self.signals.send_modify(|signal| signal.generation += 1);
    }

    fn is_shutting_down(&self) -> bool {
        self.signals.borrow().shutdown
    }

    fn in_flight(&self) -> Option<InFlight> {
        self.lock().clone()
    }

    fn begin(&self, task_id: &str, cancel: CancelSignal) {
        *self.lock() = Some(InFlight {
            task_id: task_id.to_string(),
            cancel,
        });
    }

    fn finish(&self) {
        *self.lock() = None;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<InFlight>> {
        // The guard never spans caller code, so the only way to poison this is
        // a panic inside one of the four statements above.
        self.in_flight
            .lock()
            .expect("queue in-flight lock poisoned")
    }

    fn record_step_error(&self, error: String) {
        *self
            .last_step_error
            .lock()
            .expect("queue step-error lock poisoned") = Some(error);
    }

    fn clear_step_error(&self) {
        *self
            .last_step_error
            .lock()
            .expect("queue step-error lock poisoned") = None;
    }

    fn step_error(&self) -> Option<String> {
        self.last_step_error
            .lock()
            .expect("queue step-error lock poisoned")
            .clone()
    }
}

/// The run this queue is supervising, and the button that stops it.
#[derive(Clone)]
struct InFlight {
    task_id: String,
    cancel: CancelSignal,
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

/// Throws away every change event already buffered.
///
/// A run of its own publishes a handful — the claim, the `runs` row, the
/// outcome, the board move — and none of them says anything the board read that
/// follows will not. Draining costs one pass instead of one per event, and
/// `Lagged` is drained too: on this channel it means the same thing every other
/// event does, "re-read", which is what the caller is about to do anyway.
fn drain(changes: &mut broadcast::Receiver<ChangeEvent>) {
    loop {
        match changes.try_recv() {
            Ok(_) | Err(TryRecvError::Lagged(_)) => continue,
            Err(TryRecvError::Empty | TryRecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestContext;
    use pretty_assertions::assert_eq;

    fn queue(harness: &TestContext) -> (QueueHandle, QueueTask) {
        build(
            harness.context.clone(),
            AppPaths::new("/tmp/rimaia-queue-unit-test"),
            RunnerConfig::default(),
        )
    }

    #[tokio::test]
    async fn the_control_verbs_write_the_switch_the_next_launch_reads() {
        let harness = TestContext::new().await;
        let (handle, _task) = queue(&harness);

        handle.start().await.expect("start the queue");
        assert_eq!(
            state::queue_state(&harness.context.pool)
                .await
                .expect("read"),
            QueueState::Running
        );

        handle.pause().await.expect("pause the queue");
        assert_eq!(
            state::queue_state(&harness.context.pool)
                .await
                .expect("read"),
            QueueState::Paused
        );

        handle.resume().await.expect("resume the queue");
        assert_eq!(
            state::queue_state(&harness.context.pool)
                .await
                .expect("read"),
            QueueState::Running
        );

        handle.stop().await.expect("stop the queue");
        assert_eq!(
            state::queue_state(&harness.context.pool)
                .await
                .expect("read"),
            QueueState::Paused,
            "stop is pause plus a cancellation, not a third state"
        );
    }

    #[tokio::test]
    async fn stopping_a_queue_with_nothing_in_flight_is_not_an_error() {
        // The Stop button is pressed by a human who cannot see whether the
        // current run finished half a second ago.
        let harness = TestContext::new().await;
        let (handle, _task) = queue(&harness);

        handle.stop().await.expect("stop an idle queue");

        assert_eq!(handle.in_flight_task_id(), None);
    }

    #[tokio::test]
    async fn a_control_call_wakes_a_loop_that_is_already_awake() {
        // `send_modify` always marks the value changed, which is what makes a
        // wake that lands between two polls survive to the next one.
        let harness = TestContext::new().await;
        let (handle, task) = queue(&harness);
        let signals = task.shared.signals.subscribe();

        handle.shared.wake();

        assert!(signals.has_changed().expect("the sender is alive"));
    }

    #[tokio::test]
    async fn shutdown_is_visible_to_the_loop_without_awaiting_anything() {
        let harness = TestContext::new().await;
        let (handle, task) = queue(&harness);
        assert!(!task.shared.is_shutting_down());

        handle.shutdown();

        assert!(task.shared.is_shutting_down());
    }

    #[tokio::test]
    async fn draining_leaves_the_channel_empty_without_blocking_on_it() {
        let harness = TestContext::new().await;
        let mut changes = harness.context.subscribe();

        for id in 0..3 {
            harness
                .context
                .publish(ChangeEvent::tasks([id.to_string()]));
        }
        drain(&mut changes);

        assert_eq!(changes.try_recv().unwrap_err(), TryRecvError::Empty);
    }
}
