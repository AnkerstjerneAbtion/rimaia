//! The runner's view of itself: which repositories it may be offered work in,
//! and how much room it has (task 042, ADR-0031 points 1 and 6).
//!
//! [`for_runner`] is the one builder. The runner loop sends what it returns
//! with every `ClaimTarget::Next`, the Runs view's plan (the loop's
//! `status_with_plan`) is drawn over the same repositories, and a schedule's
//! preview is planned over them too. So the plan a card shows, the plan a
//! schedule previews and the plan a claim acts on come from one view, and
//! cannot drift apart.
//!
//! Every input is this machine's, read through [`MachineContext`], plus the
//! in-memory [`InFlight`] registry. It reads nothing from the board.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::board::FreeCapacity;
use crate::error::Result;
use crate::machine::MachineContext;
use crate::schedule::window::RunWindow;
use crate::scheduler::capacity;
use crate::scheduler::inflight::InFlight;
use crate::scheduler::selection::QueueEntry;
use crate::scheduler::state::QueueState;

/// The repositories this runner lists on a `Next` claim, and what it has free.
///
/// - **The repositories** are those with a checkout here and unattended
///   consent on it (task 066's `consented_repositories`). Before task 045 a
///   repository is opted in exactly when it is listed, so listing only
///   consented checkouts keeps the board from offering a task the runner would
///   refuse after the claim — a `release`, which lands the task in `failed`.
/// - **The capacity** is [`capacity::resolve`] (the open window, the mode,
///   `max_concurrency` and each checkout's own cap, D24) minus what
///   [`InFlight::counts`] says this runner is already running. It is net: the
///   board takes it as "this many more", never as a cap to subtract from
///   again. Every listed repository has an entry, zero when it is full.
pub async fn for_runner(
    machine: &MachineContext,
    in_flight: &InFlight,
) -> Result<(Vec<String>, FreeCapacity)> {
    let resolved = capacity::resolve(machine).await?;
    let repositories: Vec<String> = crate::machine::consented_repositories(machine)
        .await?
        .into_iter()
        .collect();
    let running = in_flight.counts();

    let per_repository: BTreeMap<String, usize> = repositories
        .iter()
        .map(|id| {
            let free = resolved
                .for_repository(id)
                .saturating_sub(running.in_repository(id));
            (id.clone(), free)
        })
        .collect();
    let capacity = FreeCapacity {
        // Saturating: a total above the limit is reachable the moment somebody
        // lowers `max_concurrency` with runs already going, and an unsaturated
        // subtraction would underflow into "start everything".
        total: resolved.global.saturating_sub(running.total),
        per_repository,
    };

    Ok((repositories, capacity))
}

/// Everything the Runs view asks the queue about, in one read.
///
/// Assembled by `rimaia_runner::queue::status_with_plan`, which joins the
/// runner's half (the switch, the slots, the pause, the window and the last
/// step error) with the board's plan over [`for_runner`]'s repositories. The
/// type lives here, beside the view it is drawn from, because it is a wire
/// type the shell returns and its shape does not change with the loop's crate.
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
    /// `claude` fails the probe before any task is even chosen, so nothing on
    /// the board explains it. Without this, that failure was invisible —
    /// `state` still read `running` and `plan` still listed a full night's
    /// work, with nothing to say why none of it was happening.
    pub last_step_error: Option<String>,
    /// When ADR-0011's usage-limit hold on this runner lifts, or `None` when
    /// there is none.
    ///
    /// Surfaced for the same reason [`last_step_error`](Self::last_step_error)
    /// is: it is the other way a queue that reads `running` over a full plan can
    /// be starting nothing, and a hold the operator cannot see is one they will
    /// debug as a bug. Read fresh rather than cached, so a hold that has expired
    /// stops being reported without anything having to clear it.
    pub usage_limit_pause_until: Option<DateTime<Utc>>,
    /// The run window a schedule opened, or `None` when the queue is running
    /// because somebody pressed Start (task 013).
    ///
    /// Here rather than on a schedules read of its own, because it answers a
    /// question about *the queue*: the Runs view says "Running until 06:00 —
    /// Nightly" while it is open and, once the stop time has passed and the
    /// switch has gone back to `paused`, this is the one thing that explains a
    /// queue which stopped by itself at 06:00 with work still on the board.
    /// Without it, that reads exactly like the queue having failed.
    ///
    /// It is deliberately **not** on the Settings panel, which keeps showing the
    /// stored default configuration — see [`capacity`]'s header and
    /// seam-contract D24 on why a control that rewrote itself at 22:00 would be
    /// worse than one that does not move.
    pub window: Option<RunWindow>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Clock;
    use crate::db::{OnArchive, ScheduleMode};
    use crate::machine::Checkout;
    use crate::scheduler::inflight::SlotOwner;
    use crate::testing::TestContext;
    use pretty_assertions::assert_eq;

    async fn checkout(harness: &TestContext, id: &str, consent: bool, max_concurrency: i64) {
        harness
            .machine()
            .store
            .insert_checkout(&Checkout {
                repository_id: id.to_string(),
                path: format!("/tmp/rimaia-view-{id}"),
                worktree_root: format!("/tmp/rimaia-view-{id}-worktrees"),
                max_concurrency,
                unattended_consent: consent,
                on_archive: OnArchive::default(),
                on_archive_script: None,
                credential_login: None,
                credential_label: None,
                credential_added_at: None,
                created_at: harness.clock.now(),
            })
            .await
            .expect("a checkout");
    }

    #[tokio::test]
    async fn the_view_lists_consented_checkouts_and_what_each_has_free() {
        let harness = TestContext::new().await;
        checkout(&harness, "repo-a", true, 2).await;
        checkout(&harness, "repo-b", true, 1).await;
        checkout(&harness, "repo-c", false, 4).await;
        capacity::set_schedule_mode(harness.machine(), ScheduleMode::Parallel)
            .await
            .expect("parallel");
        capacity::set_max_concurrency(harness.machine(), 3)
            .await
            .expect("three at once");
        let in_flight = InFlight::new();
        let _running = in_flight
            .acquire_unbounded("task-1", "repo-a", SlotOwner::Manual)
            .expect("a run in repo-a");

        let (repositories, free) = for_runner(harness.machine(), &in_flight)
            .await
            .expect("build the view");

        assert_eq!(
            repositories,
            vec!["repo-a".to_string(), "repo-b".to_string()]
        );
        assert_eq!(
            free,
            FreeCapacity {
                total: 2,
                per_repository: BTreeMap::from([
                    ("repo-a".to_string(), 1),
                    ("repo-b".to_string(), 1),
                ]),
            }
        );
    }

    #[tokio::test]
    async fn a_full_runner_has_nothing_free_and_never_underflows() {
        let harness = TestContext::new().await;
        checkout(&harness, "repo-a", true, 1).await;
        let in_flight = InFlight::new();
        let _first = in_flight
            .acquire_unbounded("task-1", "repo-a", SlotOwner::Manual)
            .expect("a run");
        let _second = in_flight
            .acquire_unbounded("task-2", "repo-a", SlotOwner::Manual)
            .expect("a second, by hand, past the cap");

        let (_, free) = for_runner(harness.machine(), &in_flight)
            .await
            .expect("build the view");

        assert_eq!(
            free,
            FreeCapacity {
                total: 0,
                per_repository: BTreeMap::from([("repo-a".to_string(), 0)]),
            }
        );
    }
}
