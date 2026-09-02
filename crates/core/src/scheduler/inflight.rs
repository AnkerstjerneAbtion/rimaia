//! Who holds a live `claude` process for which task, right now.
//!
//! # The question this answers, and who was answering it before
//!
//! ADR-0021 names the gap it closes, and names it as this task's:
//!
//! > "The real question underneath is **who owns 'is this task already
//! > running' outside the Tauri shell**, and that is a core design decision,
//! > not a wiring task — it is also what tasks 012 and 014 will need when
//! > concurrency and retries arrive."
//!
//! Before this module there were two answers and neither could see the other.
//! `scheduler::queue::Shared` held a single `Option<InFlight>` for the run the
//! queue was supervising, and `src-tauri`'s `RunRegistry` held a `HashMap` for
//! the runs a button had started — with the shell's map deferring to the
//! queue's `Option` through an `attach_queue` back-reference, because the two
//! had to agree and only one of them was reachable from the other.
//!
//! That arrangement had three costs. It put a business rule ("one process per
//! task") in `src-tauri`, which ADR-0006 says is a bug wherever it happens. It
//! made `plan_task_strategy` unreachable over MCP, because the MCP server has
//! no access to a `src-tauri` type — ADR-0021's known gap, in as many words.
//! And it could not grow: a slot *map* with per-repository caps cannot be an
//! `Option`, and a second door onto it cannot be a back-reference.
//!
//! # Counting and inserting happen under one lock
//!
//! That is the whole reason this is a type rather than two fields. A caller
//! that asks "how many are running in this repository?" and *then* inserts has
//! written the double-start bug with extra steps; [`InFlight::acquire`] does
//! both inside one critical section and returns a [`Lease`] or a reason.
//!
//! # A lease is RAII, and that is load-bearing
//!
//! Dropping a [`Lease`] frees the slot. `Drop` runs on unwind as well as on a
//! normal return, so a panic anywhere inside the future supervising a run
//! releases the task rather than leaving it claimed until the app restarts —
//! the property `src-tauri`'s hand-written `ReleaseOnDrop` guard existed for,
//! now owned by the thing that hands out the claim rather than by each caller
//! who remembers to.
//!
//! Dropping also bumps a [`releases`](InFlight::releases) generation, which is
//! how the queue learns a slot opened. It cannot learn that from
//! `ChangeEvent`: `finish_run` publishes its events from *inside* `run_task`,
//! while the lease is still held, so a loop woken only by that channel would
//! count the finishing run, find no capacity and go back to sleep with nothing
//! left to wake it. A queue asleep at 2am with a free slot is the failure this
//! watch exists to prevent, and it also covers a *manual* run freeing capacity,
//! which nothing else would.
//!
//! # What this is not
//!
//! Not a claim. The transactional claim is [`super::claim`], on the database
//! row, and it is what stops two *processes* from disagreeing about a task
//! across restarts. This is an in-memory fact about this process: which tasks
//! it personally has a child for. Both exist because they answer different
//! questions, and the queue takes the lease first — before the claim — so that
//! a Pause pressed mid-claim has something to act on (see `queue`'s header).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use crate::runner::CancelSignal;

/// The most runs this process will supervise at once, whatever any setting
/// says.
///
/// Task 012's "a configurable ceiling regardless of mode, so a mis-set value
/// cannot spawn ten agents" — and it is a constant rather than a setting on
/// purpose, because a ceiling a user can raise is not one. `capacity::resolve`
/// clamps to it, and [`InFlight::acquire_unbounded`] still enforces it, so
/// there is no door around it.
pub const CONCURRENCY_CEILING: usize = 8;

/// Who asked for a run.
///
/// The distinction exists for exactly one behaviour: [`QueueHandle::stop`]
/// cancels the queue's own runs and must leave alone a run the operator
/// started by hand in front of them. Before this module the two could not be
/// confused because they lived in separate maps; now that they share one, the
/// difference has to be recorded rather than implied.
///
/// [`QueueHandle::stop`]: crate::scheduler::QueueHandle::stop
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LeaseOwner {
    /// The scheduler picked it off the board.
    Queue,
    /// A human pressed a button — "Run now", or "Plan now".
    Manual,
}

/// Why a lease was refused.
///
/// Every variant is a *reason a user can be told*, which is the test for
/// whether something belongs here: a refusal nobody can act on would be better
/// as a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseRefused {
    /// This process already has a child for that task.
    AlreadyInFlight,
    AtGlobalLimit {
        limit: usize,
    },
    AtRepositoryLimit {
        repository_id: String,
        limit: usize,
    },
}

impl LeaseRefused {
    /// The sentence a card or a tool response shows.
    ///
    /// Rendered here rather than at each call site so the button, the MCP tool
    /// and a future queue-control surface cannot describe the same refusal
    /// three ways — the same argument ADR-0006 makes for the rules themselves.
    pub fn message(&self) -> String {
        match self {
            Self::AlreadyInFlight => "a run is already in progress for this task; wait for it to \
                                      finish or cancel it first"
                .to_string(),
            Self::AtGlobalLimit { limit } => format!(
                "{limit} run{} already in flight, which is as many as this queue may supervise at \
                 once; raise the concurrency limit in Settings or wait for one to finish",
                if *limit == 1 { " is" } else { "s are" },
            ),
            Self::AtRepositoryLimit { limit, .. } => format!(
                "this repository already has {limit} run{} in flight; two agents in one repository \
                 fight over ports, test databases and lockfiles, so raise that repository's own \
                 limit deliberately or wait",
                if *limit == 1 { "" } else { "s" },
            ),
        }
    }
}

/// The two numbers an [`acquire`](InFlight::acquire) is checked against.
///
/// Resolved by the caller before it takes the lock, so this module reads no
/// settings and touches no database — which is what keeps every test in it a
/// pure function of its arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    pub global: usize,
    pub per_repository: usize,
}

impl Capacity {
    /// One run at a time, anywhere. What `sequential` mode resolves to, and
    /// what this crate does until task 012 makes it configurable.
    pub const SEQUENTIAL: Self = Self {
        global: 1,
        per_repository: 1,
    };
}

/// A snapshot of what is in flight, so selection stays a pure function over
/// values rather than something that has to hold a lock while it thinks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    pub total: usize,
    pub per_repository: HashMap<String, usize>,
    pub task_ids: HashSet<String>,
}

impl Counts {
    pub fn in_repository(&self, repository_id: &str) -> usize {
        self.per_repository.get(repository_id).copied().unwrap_or(0)
    }
}

/// One occupied slot. Dropping it frees the slot and wakes the queue.
///
/// Deliberately not `Clone`: a slot with two owners is a slot that is freed
/// twice or never, and the whole point of the type is that there is exactly one
/// thing whose lifetime the claim tracks.
pub struct Lease {
    inner: Arc<Inner>,
    task_id: String,
    cancel: CancelSignal,
}

impl Lease {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// The signal that stops this run. Minted at acquire, so it exists before
    /// any process does and a cancel that lands during worktree preparation is
    /// still heard.
    pub fn cancel_signal(&self) -> CancelSignal {
        self.cancel.clone()
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.inner.release(&self.task_id);
    }
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("task_id", &self.task_id)
            .finish()
    }
}

/// The registry itself. Cheap to clone; every clone is the same map, the shape
/// [`ServiceContext`](crate::ServiceContext) already uses and for the same
/// reason.
#[derive(Clone, Default)]
pub struct InFlight {
    inner: Arc<Inner>,
}

struct Inner {
    /// `std::sync::Mutex` rather than tokio's, on the same terms
    /// `scheduler::queue::Shared` states for its own: the guard never spans
    /// caller code and is never held across an `await`, and a lock that cannot
    /// be held across a yield point is one fewer thing to reason about.
    entries: Mutex<HashMap<String, Entry>>,
    /// Bumped on every release. The queue selects on this — see the module
    /// header on why `ChangeEvent` cannot do this job.
    releases: watch::Sender<u64>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            releases: watch::channel(0).0,
        }
    }
}

struct Entry {
    repository_id: String,
    cancel: CancelSignal,
    owner: LeaseOwner,
}

impl InFlight {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes a slot for `task_id`, or says why it cannot.
    ///
    /// The counting and the insert are one critical section. A caller that
    /// asked for [`counts`](Self::counts) and then acquired would be checking a
    /// number that another thread can invalidate between the two statements,
    /// which is the double-start this type exists to make impossible rather
    /// than unlikely.
    pub fn acquire(
        &self,
        task_id: &str,
        repository_id: &str,
        owner: LeaseOwner,
        capacity: Capacity,
    ) -> std::result::Result<Lease, LeaseRefused> {
        self.insert(task_id, repository_id, owner, Some(capacity))
    }

    /// Takes a slot for a run a human asked for, subject only to the per-task
    /// exclusion and [`CONCURRENCY_CEILING`].
    ///
    /// The caps bound what the *scheduler* starts. A person clicking "Run now"
    /// with the app in front of them is not the mis-set-configuration failure
    /// `max_concurrency` and the per-repository cap exist for, and refusing
    /// them would make those settings mean something they were never described
    /// as meaning (ADR-0010 calls them properties of the *run configuration*).
    /// The ceiling still applies, because it is the backstop rather than a
    /// preference.
    pub fn acquire_unbounded(
        &self,
        task_id: &str,
        repository_id: &str,
        owner: LeaseOwner,
    ) -> std::result::Result<Lease, LeaseRefused> {
        self.insert(task_id, repository_id, owner, None)
    }

    fn insert(
        &self,
        task_id: &str,
        repository_id: &str,
        owner: LeaseOwner,
        capacity: Option<Capacity>,
    ) -> std::result::Result<Lease, LeaseRefused> {
        let mut entries = self.inner.lock();

        if entries.contains_key(task_id) {
            return Err(LeaseRefused::AlreadyInFlight);
        }

        let global = capacity.map_or(CONCURRENCY_CEILING, |c| c.global.min(CONCURRENCY_CEILING));
        if entries.len() >= global {
            return Err(LeaseRefused::AtGlobalLimit { limit: global });
        }

        if let Some(capacity) = capacity {
            let in_repository = entries
                .values()
                .filter(|entry| entry.repository_id == repository_id)
                .count();
            if in_repository >= capacity.per_repository {
                return Err(LeaseRefused::AtRepositoryLimit {
                    repository_id: repository_id.to_string(),
                    limit: capacity.per_repository,
                });
            }
        }

        let cancel = CancelSignal::new();
        entries.insert(
            task_id.to_string(),
            Entry {
                repository_id: repository_id.to_string(),
                cancel: cancel.clone(),
                owner,
            },
        );

        Ok(Lease {
            inner: Arc::clone(&self.inner),
            task_id: task_id.to_string(),
            cancel,
        })
    }

    /// Whether this process has a child for `task_id`.
    pub fn holds(&self, task_id: &str) -> bool {
        self.inner.lock().contains_key(task_id)
    }

    /// Every task with a run in flight, sorted so a status read is stable
    /// between calls that changed nothing.
    pub fn task_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.inner.lock().keys().cloned().collect();
        ids.sort();
        ids
    }

    pub fn counts(&self) -> Counts {
        let entries = self.inner.lock();
        let mut per_repository: HashMap<String, usize> = HashMap::new();
        for entry in entries.values() {
            *per_repository
                .entry(entry.repository_id.clone())
                .or_insert(0) += 1;
        }
        Counts {
            total: entries.len(),
            per_repository,
            task_ids: entries.keys().cloned().collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// Asks `task_id`'s run to stop. `false` when nothing is running for it —
    /// not an error: `CancelSignal::cancel` is idempotent, and a Cancel pressed
    /// after a run already finished is not a mistake worth reporting.
    pub fn cancel(&self, task_id: &str) -> bool {
        match self.inner.lock().get(task_id) {
            Some(entry) => {
                entry.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Asks every run started by `owner` to stop, and reports whether there was
    /// anything to ask.
    pub fn cancel_owned_by(&self, owner: LeaseOwner) -> bool {
        let entries = self.inner.lock();
        let mut asked = false;
        for entry in entries.values().filter(|entry| entry.owner == owner) {
            entry.cancel.cancel();
            asked = true;
        }
        asked
    }

    /// Asks every run to stop, whoever started it. The exit path.
    pub fn cancel_all(&self) -> bool {
        let entries = self.inner.lock();
        for entry in entries.values() {
            entry.cancel.cancel();
        }
        !entries.is_empty()
    }

    /// A receiver that fires whenever a slot is freed. See the module header on
    /// why the queue cannot use `ChangeEvent` for this.
    pub fn releases(&self) -> watch::Receiver<u64> {
        self.inner.releases.subscribe()
    }
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        // The guard never spans caller code, so the only way to poison this is
        // a panic inside one of the short critical sections above.
        self.entries.lock().expect("in-flight registry poisoned")
    }

    fn release(&self, task_id: &str) {
        // Dropped before the notify, so a loop woken by it reads a map that
        // already has the slot free. Waking first would let the queue observe
        // the release and still count the run that produced it.
        self.lock().remove(task_id);
        self.releases.send_modify(|generation| *generation += 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO_A: &str = "repo-a";
    const REPO_B: &str = "repo-b";

    fn queue_lease(
        registry: &InFlight,
        task: &str,
        repository: &str,
        capacity: Capacity,
    ) -> std::result::Result<Lease, LeaseRefused> {
        registry.acquire(task, repository, LeaseOwner::Queue, capacity)
    }

    #[test]
    fn a_task_already_in_flight_cannot_be_leased_twice() {
        let registry = InFlight::new();
        let capacity = Capacity {
            global: 4,
            per_repository: 4,
        };
        let _first = queue_lease(&registry, "task-1", REPO_A, capacity).expect("the first lease");

        assert_eq!(
            queue_lease(&registry, "task-1", REPO_A, capacity).unwrap_err(),
            LeaseRefused::AlreadyInFlight,
        );
    }

    #[test]
    fn a_repository_at_its_cap_refuses_while_another_repository_is_still_free() {
        // ADR-0010's rule, as an assertion: parallelism *across* repositories
        // is the safe default and within one repository it is opt-in.
        let registry = InFlight::new();
        let capacity = Capacity {
            global: 4,
            per_repository: 1,
        };
        let _first = queue_lease(&registry, "task-1", REPO_A, capacity).expect("the first lease");

        assert_eq!(
            queue_lease(&registry, "task-2", REPO_A, capacity).unwrap_err(),
            LeaseRefused::AtRepositoryLimit {
                repository_id: REPO_A.to_string(),
                limit: 1,
            },
        );
        queue_lease(&registry, "task-3", REPO_B, capacity)
            .expect("a different repository still has room");
    }

    #[test]
    fn the_ceiling_refuses_a_lease_no_setting_can_widen() {
        // A `global` above the ceiling is clamped rather than honoured, which
        // is what makes the ceiling a ceiling and not a default.
        let registry = InFlight::new();
        let capacity = Capacity {
            global: 100,
            per_repository: 100,
        };

        let mut held = Vec::new();
        for index in 0..CONCURRENCY_CEILING {
            held.push(
                queue_lease(&registry, &format!("task-{index}"), REPO_A, capacity)
                    .expect("a slot under the ceiling"),
            );
        }

        assert_eq!(
            queue_lease(&registry, "one-too-many", REPO_A, capacity).unwrap_err(),
            LeaseRefused::AtGlobalLimit {
                limit: CONCURRENCY_CEILING,
            },
        );
    }

    #[test]
    fn the_ceiling_binds_a_human_too_even_though_the_caps_do_not() {
        let registry = InFlight::new();
        let mut held = Vec::new();
        for index in 0..CONCURRENCY_CEILING {
            held.push(
                registry
                    .acquire_unbounded(&format!("task-{index}"), REPO_A, LeaseOwner::Manual)
                    .expect("a slot under the ceiling"),
            );
        }

        assert_eq!(
            registry
                .acquire_unbounded("one-too-many", REPO_A, LeaseOwner::Manual)
                .unwrap_err(),
            LeaseRefused::AtGlobalLimit {
                limit: CONCURRENCY_CEILING,
            },
        );
    }

    #[test]
    fn a_human_is_not_refused_by_the_caps_the_scheduler_obeys() {
        let registry = InFlight::new();
        let _queued = queue_lease(&registry, "task-1", REPO_A, Capacity::SEQUENTIAL)
            .expect("the queue's own run");

        registry
            .acquire_unbounded("task-2", REPO_A, LeaseOwner::Manual)
            .expect("a person clicking Run now is not a mis-set setting");
    }

    #[test]
    fn dropping_a_lease_frees_the_slot_and_bumps_the_release_generation() {
        let registry = InFlight::new();
        let mut releases = registry.releases();
        let before = *releases.borrow_and_update();

        let lease =
            queue_lease(&registry, "task-1", REPO_A, Capacity::SEQUENTIAL).expect("the only slot");
        assert!(registry.holds("task-1"));
        assert!(!releases.has_changed().expect("the sender outlives this"));

        drop(lease);

        assert!(!registry.holds("task-1"));
        assert!(registry.is_empty());
        assert!(*releases.borrow_and_update() > before);
    }

    #[test]
    fn a_lease_dropped_by_a_panicking_supervisor_still_frees_the_slot() {
        // The property `src-tauri`'s hand-written `ReleaseOnDrop` guard existed
        // for. A trailing release statement only runs on a normal return; a
        // panic unwinds past it and leaves the task claimed until the app
        // restarts, so every later "Run now" on that card is refused forever.
        let registry = InFlight::new();
        let held = registry.clone();

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lease = held
                .acquire("task-1", REPO_A, LeaseOwner::Manual, Capacity::SEQUENTIAL)
                .expect("the only slot");
            panic!("the supervising future blew up");
        }));

        assert!(panicked.is_err(), "the panic must actually have happened");
        assert!(registry.is_empty(), "unwinding released the slot");
    }

    #[test]
    fn stopping_the_queue_leaves_a_manual_lease_alone() {
        // Before this module the two could not be confused because they lived
        // in separate maps. Sharing one map is what makes the owner load-bearing.
        let registry = InFlight::new();
        let queued = queue_lease(&registry, "queued", REPO_A, Capacity::SEQUENTIAL)
            .expect("the queue's run");
        let manual = registry
            .acquire_unbounded("manual", REPO_B, LeaseOwner::Manual)
            .expect("a run someone started deliberately");

        assert!(registry.cancel_owned_by(LeaseOwner::Queue));

        assert!(queued.cancel_signal().is_cancelled());
        assert!(
            !manual.cancel_signal().is_cancelled(),
            "a Stop must not kill a run the operator started in front of them"
        );
    }

    #[test]
    fn cancelling_everything_reaches_both_owners() {
        let registry = InFlight::new();
        let queued = queue_lease(&registry, "queued", REPO_A, Capacity::SEQUENTIAL)
            .expect("the queue's run");
        let manual = registry
            .acquire_unbounded("manual", REPO_B, LeaseOwner::Manual)
            .expect("a manual run");

        assert!(registry.cancel_all());

        assert!(queued.cancel_signal().is_cancelled());
        assert!(manual.cancel_signal().is_cancelled());
    }

    #[test]
    fn cancelling_a_task_nothing_is_running_is_not_an_error() {
        assert!(!InFlight::new().cancel("never-started"));
    }

    #[test]
    fn counting_and_inserting_happen_under_one_lock() {
        // Two threads racing the last free slot: exactly one wins. A
        // count-then-insert would let both read "zero in flight" before either
        // wrote, which is the double-start this type exists to prevent rather
        // than to make unlikely.
        let registry = InFlight::new();
        let barrier = Arc::new(std::sync::Barrier::new(2));

        let handles: Vec<_> = (0..2)
            .map(|index| {
                let registry = registry.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    // The lease is returned, not tested and dropped: dropping
                    // it inside the thread would free the slot before the other
                    // thread ever asked for it, and the test would pass without
                    // the two ever having raced.
                    registry.acquire(
                        &format!("task-{index}"),
                        REPO_A,
                        LeaseOwner::Queue,
                        Capacity::SEQUENTIAL,
                    )
                })
            })
            .collect();

        let outcomes: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("neither thread panics"))
            .collect();

        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
            1,
            "exactly one thread may take the last slot",
        );
        assert_eq!(registry.counts().total, 1);
    }

    #[test]
    fn counts_are_a_snapshot_selection_can_reason_over_without_a_lock() {
        let registry = InFlight::new();
        let capacity = Capacity {
            global: 4,
            per_repository: 4,
        };
        let _a1 = queue_lease(&registry, "a1", REPO_A, capacity).expect("a slot");
        let _a2 = queue_lease(&registry, "a2", REPO_A, capacity).expect("a slot");
        let _b1 = queue_lease(&registry, "b1", REPO_B, capacity).expect("a slot");

        let counts = registry.counts();

        assert_eq!(counts.total, 3);
        assert_eq!(counts.in_repository(REPO_A), 2);
        assert_eq!(counts.in_repository(REPO_B), 1);
        assert_eq!(counts.in_repository("repo-nobody-is-using"), 0);
        assert!(counts.task_ids.contains("a1"));
    }

    #[test]
    fn task_ids_come_back_in_a_stable_order() {
        // `QueueStatus` is serialized to the Runs view on every read; an order
        // that shuffled between two reads that changed nothing would redraw the
        // list for no reason.
        let registry = InFlight::new();
        let capacity = Capacity {
            global: 4,
            per_repository: 4,
        };
        let _c = queue_lease(&registry, "c", REPO_A, capacity).expect("a slot");
        let _a = queue_lease(&registry, "a", REPO_A, capacity).expect("a slot");
        let _b = queue_lease(&registry, "b", REPO_A, capacity).expect("a slot");

        assert_eq!(registry.task_ids(), vec!["a", "b", "c"]);
    }
}
