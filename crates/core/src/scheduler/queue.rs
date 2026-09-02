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
//! The loop does work until there is none, then waits on four things: its own
//! control signals; [`ChangeEvent`](crate::ChangeEvent) — ADR-0018's channel,
//! whose Consequences already anticipated this subscriber ("adding the
//! scheduler's own view later is another `subscribe()` and no coordination with
//! anyone"); [`InFlight::releases`]; and its own [`JoinSet`]. A card dragged to
//! the top of `ready` publishes `Tasks`, the loop wakes, re-reads the board and
//! claims it. There is no interval to tune and nothing that costs a query while
//! the board is idle.
//!
//! Change events are drained *before* the board is read, never after. An event
//! that arrives between the read and the wait is then still buffered and wakes
//! it immediately; draining afterwards would throw away exactly the
//! notification that says the plan just went stale. The `releases` watch needs
//! no equivalent: `changed()` marks a generation seen only when it *returns*,
//! so a slot freed while the loop was busy is still there to be found.
//!
//! # The `releases` arm is not optional
//!
//! `finish_run` publishes its `ChangeEvent`s from *inside* `run_task`, while
//! the lease is still held. A loop woken only by that channel therefore
//! computes [`counts`](InFlight::counts) including the run that is finishing,
//! finds no capacity, and goes back to sleep — with nothing left to wake it
//! when the lease actually drops. That is a queue that stalls at 2am with free
//! slots and a full board. The [`JoinSet`] arm does not cover it either, for
//! the case that matters most: a *manual* run freeing a slot is not a task this
//! queue spawned, so nothing joins.
//!
//! # Several runs at once, and one place that decides how many
//!
//! [`capacity::resolve`] answers "how many, and how many per repository";
//! [`selection::next_batch`] turns that plus a board read into a list;
//! `try_step` walks that list in board order and hands each entry to
//! [`supervise`] on the [`JoinSet`]. Sequential mode is not a separate path —
//! it resolves to `global = 1` and walks the same code.
//!
//! One pass is three phases, and the order of them is the part worth reading:
//!
//! 1. **Every lease, for the whole batch.** Before anything that awaits, for
//!    the reason the mid-claim section below gives — and for the whole batch
//!    rather than one at a time, so a Stop pressed during phase 2 reaches the
//!    second and third entries and not only the first.
//! 2. **`probe_cli`, once.** A missing `claude` is a property of the
//!    installation and not of an entry; probing per entry would spawn N
//!    processes to learn one fact.
//! 3. **Claim and spawn, per entry.**
//!
//! A stop found mid-batch applies to the *rest of the batch*, not only to the
//! entry that found it. `break`, not `continue`: a Pause that lands while the
//! queue is starting the second of three is a Pause, and starting the third
//! anyway would be the same "the switch changed underneath it" failure the
//! mid-claim window below is about, one entry later. The leases of the entries
//! never reached are freed by dropping the rest of the vector, which is the
//! same RAII that covers the `?` on the probe.
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
//! [`QueueHandle::start`] runs task 018's [`doctor`] and refuses, without
//! writing `queue_state`, when anything is failing. Two consequences worth
//! stating where the loop can see them.
//!
//! It is on the **handle** so that every door — the Tauri command, any future
//! MCP queue tool, task 013's scheduled start — inherits the same refusal
//! without each remembering to ask. ADR-0006's "a rule enforced in only one
//! adapter is a bug", made structural.
//!
//! It is **not** in [`QueueTask::try_step`], deliberately, and that is not an
//! oversight to correct later: `try_step` runs on every change event, so a
//! doctor there would spawn eight subprocesses per card drag. The queue is a
//! preflight case, not a watchdog case. The single check that genuinely must be
//! per-step is `probe_cli`, and it is already there. Seam-contract D22.
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
//! `try_step` takes its [`Lease`] on the shared [`InFlight`] registry — the one
//! thing [`QueueHandle::stop`] has to signal, and the one thing that makes
//! [`QueueHandle::in_flight_task_ids`] non-empty — **before** the `claude`
//! prerequisite check and the claim itself, not after either succeeds. Both
//! await, so without this a Pause, a Stop or [`QueueHandle::shutdown`]
//! pressed in that window found nothing cancellable and nothing that
//! disagreed with a switch still reading `running`, and the loop claimed and
//! ran the task anyway. `try_step` re-checks the switch and the signal at the
//! two points that window used to hide from — right after the probe, and
//! right after the claim succeeds — releasing a claim it already won rather
//! than spawn a process nobody asked for any more.
//!
//! # The queue does not own the in-flight map any more
//!
//! It used to: a private `Option<InFlight>` on `Shared`, with `src-tauri`'s
//! `RunRegistry` holding a back-reference to this handle so the two maps could
//! agree. That put a business rule — one process per task — in the shell, which
//! ADR-0006 calls a bug, and it is the reason ADR-0021 could not put
//! `plan_task_strategy` on the MCP surface.
//!
//! Now [`build`] takes an [`InFlight`] the shell also holds, and both doors
//! read the same map. `Shared` keeps only what is genuinely the *queue's* —
//! its control signals and the last step error. The `Option` is gone rather
//! than widened, which is what makes task 012's slot map an ordinary change to
//! one type instead of a second rewrite of this one.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinSet;

use crate::context::ServiceContext;
use crate::db::MutationSource;
use crate::doctor;
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::paths::AppPaths;
use crate::runner::{probe_cli, run_task, CancelSignal, RunRequest, RunTrigger, RunnerConfig};
use crate::scheduler::capacity;
use crate::scheduler::claim::{self, ClaimOutcome};
use crate::scheduler::inflight::{Capacity, InFlight, Lease, LeaseOwner};
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
    /// Every task this process has a `claude` child for right now, in a stable
    /// order — the queue's own runs and any a button started, because they
    /// share one registry and the Runs view renders them the same way.
    ///
    /// A `Vec` rather than the `Option` this was: task 012 fills more than one
    /// slot, and a wire field that changes shape once a mode setting is flipped
    /// would be worse than one that is always a list and usually has one entry.
    pub running_task_ids: Vec<String>,
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
}

/// Wires a queue: the handle to keep, and the task to spawn.
///
/// `in_flight` is passed rather than created here, and rather than living on
/// [`ServiceContext`]. Not on the context because ADR-0019 fixes that struct's
/// shape and a sixth field would be a new record — and because it would be
/// wrong on the merits: store, clock, channels and attribution are things *any*
/// service may use, while an in-flight map is only meaningful to something that
/// can spawn, which is three call sites rather than every function. Passed
/// rather than created because the shell needs the same value for its own
/// doors, which is the precedent `RunnerConfig::run_handles` already sets: one
/// value built in `setup()` and handed to everything that needs it, with no
/// ordering constraint between the subsystems that take it.
pub fn build(
    ctx: ServiceContext,
    paths: AppPaths,
    runner: RunnerConfig,
    in_flight: InFlight,
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
    // `paths` and `runner` live on `Shared` rather than on `QueueTask` because
    // task 018's preflight needs both, and the preflight belongs to the
    // *handle* — see [`QueueHandle::start`]. The loop reads them through the
    // same `Arc` it already holds, so nothing about its own use changed.
    let shared = Arc::new(Shared {
        ctx,
        paths,
        runner,
        signals,
        in_flight,
        last_step_error: Mutex::new(None),
    });

    (
        QueueHandle {
            shared: Arc::clone(&shared),
        },
        QueueTask { shared },
    )
}

impl QueueHandle {
    /// Starts working the board. Idempotent — starting a running queue writes
    /// the same row and wakes a loop that was not asleep.
    ///
    /// # The preflight refusal (task 018)
    ///
    /// Runs [`doctor::run`] first and **refuses without writing `queue_state`**
    /// when anything is failing. Nothing is half-done on the refusal path: the
    /// switch is untouched, so a user who fixes the environment and presses
    /// Start again is starting from the same place, and a user who walks away
    /// has not left a queue that thinks it is running.
    ///
    /// It lives here, on the handle, rather than in either command surface, and
    /// that placement is the point: the Tauri command and any future MCP queue
    /// tool both call this method, so the refusal is identical on both doors
    /// *by construction* rather than by two adapters remembering to agree.
    /// ADR-0006's rule satisfied structurally.
    ///
    /// It is deliberately **not** in the loop's own `try_step` — see this
    /// module's header and `doctor`'s, and seam-contract D22.
    pub async fn start(&self) -> Result<()> {
        let report = doctor::run(&self.shared.ctx, &self.shared.doctor_environment()).await?;
        if report.is_blocking() {
            tracing::warn!(
                blocking = report.blocking().count(),
                "refusing to start the run queue: the preflight doctor is reporting failures",
            );
            return Err(Error::invalid(report.blocking_summary()));
        }

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
    ///
    /// It inherits the preflight for free, and should: resuming after a pause
    /// is the same act of leaving the machine to work unattended, and the
    /// environment has had every opportunity to change during the pause.
    pub async fn resume(&self) -> Result<()> {
        self.start().await
    }

    /// Starts nothing new; lets the current run finish.
    pub async fn pause(&self) -> Result<()> {
        self.set(QueueState::Paused).await
    }

    /// Pause, plus cancel whatever *the queue* is running.
    ///
    /// The switch is written **before** the cancellation, so the loop can never
    /// observe the run ending while the queue still reads `running` and pick up
    /// the next task. The cancelled run lands `failed` (ADR-0010: cancel-one on
    /// a running task "goes to `failed` with `cancelled` reason"), which is not
    /// a state the queue re-selects — so a stopped task stays stopped.
    ///
    /// Scoped to [`LeaseOwner::Queue`]. Before the registries were merged this
    /// was true by accident, because a manual run lived in a different map that
    /// this handle could not reach; now that they share one it has to be said.
    /// Stopping the queue is a statement about the queue, and a run the
    /// operator started by hand in front of them is not part of it.
    pub async fn stop(&self) -> Result<()> {
        self.pause().await?;
        if self.shared.in_flight.cancel_owned_by(LeaseOwner::Queue) {
            tracing::info!("stopping the run queue's own in-flight runs");
        }
        Ok(())
    }

    /// The whole picture, for the Runs view.
    pub async fn status(&self) -> Result<QueueStatus> {
        Ok(QueueStatus {
            state: state::queue_state(&self.shared.ctx.pool).await?,
            running_task_ids: self.in_flight_task_ids(),
            plan: selection::plan(&self.shared.ctx).await?,
            last_step_error: self.shared.step_error(),
        })
    }

    /// Every task this process has a `claude` child for right now.
    ///
    /// The one piece of queue state that is *not* in the database, because it
    /// is not a fact about stored state: the row says `run_state = running`
    /// either way, and what this answers is "is the process on the end of it
    /// ours?".
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
    /// that owns the change channel is gone, and then waits for every run it
    /// started.
    pub async fn run(self) {
        let mut signals = self.shared.signals.subscribe();
        let mut changes = self.shared.ctx.subscribe();
        let mut releases = self.shared.in_flight.releases();
        // The runs this queue is supervising. Owned by the loop rather than by
        // `Shared`, because nothing outside the loop may join or abort them —
        // `QueueHandle::stop` speaks to the *leases*, which is a request the
        // run itself honours, not an abort that would lose its `finish_run`.
        let mut runs: JoinSet<()> = JoinSet::new();

        tracing::info!("the run queue is watching the board");

        loop {
            if self.shared.is_shutting_down() {
                break;
            }

            // Before the board read, never after — see this module's header.
            drain(&mut changes);

            if self.step(&mut runs).await == Step::Worked {
                continue;
            }

            tokio::select! {
                // Every arm is cancel-safe, which is what lets this loop drop
                // whichever future did not win: `watch::Receiver::changed`
                // marks a value seen only once it returns,
                // `broadcast::Receiver::recv` holds its position in the
                // channel rather than in the future, and `JoinSet::join_next`
                // leaves an unfinished task in the set.
                changed = signals.changed() => if changed.is_err() { break },
                event = changes.recv() => if matches!(event, Err(RecvError::Closed)) { break },
                // A slot opened. See this module's header on why `ChangeEvent`
                // cannot carry this and why the arm below does not cover it.
                _ = releases.changed() => {},
                // Guarded, because `join_next` on an empty set returns `None`
                // immediately — an unguarded arm would make this `select!` a
                // spin loop for the whole time the queue has nothing running,
                // which is most of the night.
                Some(joined) = runs.join_next(), if !runs.is_empty() => {
                    if let Err(error) = joined {
                        // `supervise` returns `()` and handles its own errors,
                        // so the only thing this can be is a panic inside it
                        // (or an abort, which nothing issues). The lease is
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
    async fn step(&self, runs: &mut JoinSet<()>) -> Step {
        match self.try_step(runs).await {
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

    async fn try_step(&self, runs: &mut JoinSet<()>) -> Result<Step> {
        let ctx = &self.shared.ctx;

        if state::queue_state(&ctx.pool).await? != QueueState::Running {
            return Ok(Step::Idle);
        }

        // Read fresh every pass, never held: the operator may flip the mode or
        // raise a repository's cap at 23:00 with runs already in flight, and
        // the pass after that write is the one that has to notice.
        let capacity = capacity::resolve(ctx).await?;
        let plan = selection::plan(ctx).await?;
        let batch = selection::next_batch(
            &plan,
            &self.shared.in_flight.counts(),
            capacity.global,
            &capacity.per_repository,
        );
        if batch.is_empty() {
            return Ok(Step::Idle);
        }

        // Every lease first, for the whole batch, and **before** the
        // prerequisite probe below. Both halves of that ordering are
        // load-bearing.
        //
        // Before the probe and before the claim, because both await — see this
        // module's header on the window that left open otherwise. A Pause, a
        // Stop or a shutdown pressed anywhere from here on now has a
        // `CancelSignal` to act on (`QueueHandle::stop`) and a switch this
        // function re-checks itself, rather than landing in the gap where
        // `stop` found nothing to cancel and nothing here noticed the switch
        // had changed underneath it. Taking them for the *whole* batch rather
        // than one at a time is what extends that guarantee to the second and
        // third entries too: a Stop during the probe now reaches all of them.
        //
        // The lease is also what replaces the explicit `finish()` that used to
        // be needed on every path back out of here: `Drop` frees the slot on
        // all of them — including the early return below, including the `?` on
        // the probe, and including a panic, which no trailing statement could.
        //
        // A refusal is not an error. The registry is shared with the button, so
        // "a human already started this one" is an ordinary race the queue
        // loses gracefully — it moves to the next entry rather than recording a
        // step error nobody needs to read.
        let mut leased: Vec<(&QueueEntry, Lease)> = Vec::with_capacity(batch.len());
        for entry in batch {
            match self.shared.in_flight.acquire(
                &entry.task_id,
                &entry.repository_id,
                LeaseOwner::Queue,
                Capacity {
                    global: capacity.global,
                    per_repository: capacity.for_repository(&entry.repository_id),
                },
            ) {
                Ok(lease) => leased.push((entry, lease)),
                Err(refused) => tracing::info!(
                    task_id = %entry.task_id,
                    reason = %refused.message(),
                    "passing over a task",
                ),
            }
        }
        if leased.is_empty() {
            return Ok(Step::Idle);
        }

        // Once for the whole batch. A missing `claude` is a property of this
        // installation, not of an entry, and probing per entry would spawn N
        // processes to learn one fact. Before any claim, rather than after —
        // task 008's acceptance criterion ("a missing binary is refused before
        // any run state is written") is worth as much to a queue as to a
        // button, and a queue that claimed first would spend a night walking
        // the board marking every task failed because `claude` is not
        // installed.
        //
        // **This one probe, and never the whole doctor.** Task 018's preflight
        // is on `QueueHandle::start`, which the user presses once; this loop
        // wakes on *every* change event, so a doctor here would be eight
        // subprocess spawns per card drag — and a transient blip would halt a
        // queue mid-flight, which is a new failure mode invented to prevent an
        // old one. What that costs is bounded and stated: an environment that
        // breaks after the queue started is caught for `claude` by this line,
        // and for everything else by the run itself. Seam-contract D22 records
        // it so it is not later "improved".
        probe_cli(&self.shared.runner.program).await?;

        let mut worked = false;
        for (entry, lease) in leased {
            let task_id = entry.task_id.clone();
            let cancel = lease.cancel_signal();

            if self.interrupted_since(&cancel).await? {
                // Nothing has been claimed yet, so there is nothing to release
                // — just stop before spending a claim on a task nobody wants
                // started any more. `break`, not `continue`: a stop applies to
                // the rest of the batch and not only to this entry.
                drop(lease);
                break;
            }

            if claim::claim(ctx, &task_id).await? == ClaimOutcome::Lost {
                // Somebody else has it. The board says something different
                // from what it said a moment ago, so the pass counts as work
                // and the loop will look again rather than waiting.
                drop(lease);
                worked = true;
                continue;
            }

            if self.interrupted_since(&cancel).await? {
                // Won the claim, but a Pause, a Stop or a shutdown landed
                // while it was in flight: release what was just claimed rather
                // than spawn a process for a queue that was told to stop
                // before this one started.
                claim::release(ctx, &task_id).await;
                drop(lease);
                break;
            }

            tracing::info!(%task_id, title = %entry.title, "the run queue started a task");

            runs.spawn(supervise(
                ctx.clone(),
                self.shared.paths.clone(),
                self.shared.runner.clone(),
                lease,
                RunRequest {
                    task_id,
                    // ADR-0012: the unattended path, behind the per-repository
                    // opt-in `selection` already checked.
                    trigger: RunTrigger::Queued,
                    cancel,
                    // So two runs in one repository take turns creating their
                    // worktrees rather than racing `git worktree add` against
                    // one `.git` — see `InFlight::preparation_lock`.
                    in_flight: Some(self.shared.in_flight.clone()),
                },
            ));
            worked = true;
        }

        Ok(if worked { Step::Worked } else { Step::Idle })
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

/// Sees one run through, and gives its slot back.
///
/// A free function taking owned clones rather than a method, and deliberately
/// with no access to [`Shared`]: it outlives the pass that spawned it and may
/// outlive the loop itself, so anything it could reach through `&self` would be
/// a lifetime the borrow checker has to be argued out of and a shared field two
/// concurrent supervisors could disagree over. Everything it needs is cheap to
/// clone and already designed to be — [`ServiceContext`], [`AppPaths`] and
/// [`RunnerConfig`] are all handles.
///
/// The lease is dropped **last**, after `run_task` has returned and after any
/// release. Dropping it earlier would wake the loop while `finish_run`'s own
/// writes were still landing, and the pass it woke would read a board that had
/// not finished changing.
async fn supervise(
    ctx: ServiceContext,
    paths: AppPaths,
    runner: RunnerConfig,
    lease: Lease,
    request: RunRequest,
) {
    let task_id = request.task_id.clone();

    match run_task(&ctx, &paths, &runner, request).await {
        Ok(run) => tracing::info!(
            %task_id,
            run_id = %run.id,
            exit_class = ?run.exit_class,
            "the run queue finished a task",
        ),
        Err(error) => {
            tracing::error!(%task_id, %error, "a queued run could not be completed");
            claim::release(&ctx, &task_id).await;
        }
    }

    drop(lease);
}

/// What one pass of the loop came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Something happened — a run was spawned, or a claim was lost under the
    /// queue. Look at the board again immediately. This is what makes the queue
    /// continuous: the next task starts as soon as there is room for it, and
    /// the board it is chosen from is re-read rather than remembered.
    Worked,
    /// Nothing to do. Wait to be woken.
    Idle,
}

/// What the control surface and the loop share.
struct Shared {
    ctx: ServiceContext,
    /// Where state lives. Read by the loop when it starts a run, and by
    /// [`QueueHandle::start`]'s preflight, which asks whether it is writable
    /// and how much room is left on it.
    paths: AppPaths,
    /// The one runner configuration every run this queue starts is spawned
    /// from. The preflight reads two things off it that must not be guessed:
    /// `program`, so the doctor probes the same `claude` the loop will spawn,
    /// and `run_handles`, for the endpoint the MCP server actually bound.
    runner: RunnerConfig,
    signals: watch::Sender<Signal>,
    /// Not the queue's own map any more — the shell holds a clone of the same
    /// registry, and both doors read it. See this module's header.
    in_flight: InFlight,
    /// The reason [`QueueTask::step`]'s last pass could not be completed, if
    /// it couldn't. Written only in `step`, never inside `try_step` itself.
    last_step_error: Mutex<Option<String>>,
}

impl Shared {
    /// The preflight's view of this installation, built from what the queue was
    /// wired with rather than from defaults — so the doctor answers about the
    /// binary and the endpoint this queue would actually use.
    fn doctor_environment(&self) -> doctor::Environment {
        doctor::Environment::for_runner(self.paths.clone(), &self.runner)
    }

    fn wake(&self) {
        self.signals.send_modify(|signal| signal.generation += 1);
    }

    fn is_shutting_down(&self) -> bool {
        self.signals.borrow().shutdown
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
    use tempfile::TempDir;

    /// A queue whose preflight can actually pass.
    ///
    /// The [`TempDir`] is returned rather than dropped on the way out, and every
    /// caller binds it: it owns both the app directory `data_directory` reports
    /// on and the stand-in binary the two `claude` checks spawn, so letting it
    /// drop here would delete the environment the gate is about to inspect.
    ///
    /// This used to be `AppPaths::new("/tmp/…")` and `RunnerConfig::default()`,
    /// which was fine until task 018 put the doctor on `start()`. A bare
    /// `RunnerConfig::default()` resolves `claude` on `PATH`, and `claude` is a
    /// prerequisite the project deliberately never bundles (ADR-0004) — so that
    /// version of this helper passed on a developer's machine and failed on CI,
    /// where no CLI is installed. See `testing::doctor::passing_queue_environment`.
    fn queue(harness: &TestContext) -> (TempDir, QueueHandle, QueueTask) {
        let (root, paths, runner) = crate::testing::doctor::passing_queue_environment();
        let (handle, task) = build(harness.context.clone(), paths, runner, InFlight::new());
        (root, handle, task)
    }

    #[tokio::test]
    async fn the_control_verbs_write_the_switch_the_next_launch_reads() {
        let harness = TestContext::new().await;
        let (_root, handle, _task) = queue(&harness);

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
            harness
                .context
                .publish(ChangeEvent::tasks([id.to_string()]));
        }
        drain(&mut changes);

        assert_eq!(changes.try_recv().unwrap_err(), TryRecvError::Empty);
    }
}
