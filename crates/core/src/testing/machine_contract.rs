//! The machine store's contract suite (task 041, seam-contract D31's
//! 2026-10-10 amendment).
//!
//! One set of cases, run against both implementations: `MemoryMachine` from
//! `crates/core/tests/machine_store_memory.rs`, and the runner's store on a
//! real `runner.db` from `crates/runner/tests/machine_store_sqlite.rs` (D31
//! point 13's pattern). The suite is the reason the in-memory store can be
//! trusted, so it covers every constraint the runner schema enforces: the
//! `worktrees → checkouts` foreign key in both directions, the primary keys,
//! absent against empty settings, and every nullable column round-tripping.
//!
//! A case reaches the store only through [`Harness::store`], so neither
//! implementation can pass by a route the other does not have.

use std::future::Future;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};

use crate::board::LeasePurpose;
use crate::db::{OnArchive, Schedule, ScheduleMode};
use crate::error::ErrorCode;
use crate::machine::{Checkout, CheckoutPatch, HeldLease, MachineStore, WorktreeRecord};
use crate::tasks::Patch;
use crate::testing::test_epoch;

/// What an implementation's test crate implements to run the suite.
pub trait Harness: Sized {
    /// A fresh, empty store.
    fn start() -> impl Future<Output = Self>;
    /// The store under test.
    fn store(&self) -> Arc<dyn MachineStore>;
}

/// Expands to one `#[tokio::test]` per case, so a failure names its case.
#[macro_export]
macro_rules! machine_store_contract {
    ($harness:ty) => {
        $crate::machine_store_contract!(@cases $harness;
            an_absent_setting_reads_as_none,
            a_setting_written_twice_holds_the_second_value,
            clearing_a_setting_makes_it_absent_and_clearing_twice_is_not_an_error,
            a_checkout_round_trips_every_column,
            a_second_checkout_of_one_repository_is_refused,
            patching_a_checkout_changes_only_what_the_patch_sets,
            patching_or_removing_an_unknown_checkout_answers_false,
            a_worktree_for_an_unknown_checkout_is_refused,
            a_checkout_with_worktrees_cannot_be_removed,
            recording_a_tasks_worktree_again_replaces_it,
            forgetting_a_worktree_answers_whether_there_was_one,
            checkouts_and_worktrees_list_in_key_order,
            every_schedule_column_round_trips,
            a_second_schedule_with_one_id_is_refused,
            updating_a_schedule_keeps_its_last_fire,
            enabling_and_firing_write_only_their_columns,
            a_write_to_an_unknown_schedule_answers_false,
            schedules_list_by_name_then_id,
            a_held_lease_round_trips_every_column,
            recording_a_held_lease_on_a_task_again_replaces_it,
            noting_a_held_leases_run_writes_the_run_and_the_purpose,
            forgetting_a_held_lease_answers_whether_there_was_one,
        );
    };
    (@cases $harness:ty; $($case:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $case() {
                $crate::testing::machine_contract::cases::$case::<$harness>().await;
            }
        )*
    };
}

fn at(minutes: i64) -> DateTime<Utc> {
    test_epoch() + Duration::minutes(minutes)
}

/// A checkout with every column set, so a column that does not survive the
/// trip shows.
fn full_checkout(repository_id: &str) -> Checkout {
    Checkout {
        repository_id: repository_id.to_string(),
        path: format!("/Users/someone/code/{repository_id} with spaces"),
        worktree_root: format!("/Users/someone/worktrees/{repository_id}"),
        max_concurrency: 3,
        unattended_consent: true,
        on_archive: OnArchive::Script,
        on_archive_script: Some("/usr/local/bin/tidy up.sh".to_string()),
        credential_login: Some("octocat".to_string()),
        credential_label: Some("fine-grained, rimaia only — expires March".to_string()),
        credential_added_at: Some(at(5)),
        created_at: at(1),
    }
}

/// The same repository's checkout with every nullable column `NULL`.
fn bare_checkout(repository_id: &str) -> Checkout {
    Checkout {
        max_concurrency: 1,
        unattended_consent: false,
        on_archive: OnArchive::None,
        on_archive_script: None,
        credential_login: None,
        credential_label: None,
        credential_added_at: None,
        ..full_checkout(repository_id)
    }
}

fn worktree(task_id: &str, repository_id: &str) -> WorktreeRecord {
    WorktreeRecord {
        task_id: task_id.to_string(),
        repository_id: repository_id.to_string(),
        path: format!("/Users/someone/worktrees/{repository_id}/{task_id}"),
        fenced_at: None,
    }
}

fn schedule(id: &str, name: &str) -> Schedule {
    Schedule {
        id: id.to_string(),
        name: name.to_string(),
        mode: ScheduleMode::Parallel,
        cron: Some("0 22 * * *".to_string()),
        start_at: None,
        max_concurrency: 3,
        enabled: true,
        timezone: Some("Europe/Copenhagen".to_string()),
        stop_at: Some("06:00".to_string()),
        last_fired_at: Some(at(30)),
        armed_at: Some(at(10)),
    }
}

/// A held lease with every column set, its run included.
fn held_lease(task_id: &str, generation: i64) -> HeldLease {
    HeldLease {
        task_id: task_id.to_string(),
        team_id: "3f2b1c00-0000-4000-8000-0000000000a1".to_string(),
        purpose: LeasePurpose::Review,
        run_id: Some(format!("run-of-{task_id}")),
        generation,
        acquired_at: at(7),
    }
}

fn assert_refused(error: crate::Error, message: &str) {
    assert_eq!(error.code(), ErrorCode::Invalid, "{error}");
    assert_eq!(error.to_string(), message);
}

pub mod cases {
    use super::*;

    pub async fn an_absent_setting_reads_as_none<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();

        assert_eq!(store.get_setting("queue_state").await.expect("read"), None);

        // Absent and empty are different answers: an accessor decides what
        // each means, and the store must not decide for it.
        store.set_setting("queue_state", "").await.expect("write");
        assert_eq!(
            store.get_setting("queue_state").await.expect("read"),
            Some(String::new())
        );
    }

    pub async fn a_setting_written_twice_holds_the_second_value<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        let awkward = "{\"note\":\"it's — ünïcode\"}\n";

        store
            .set_setting("doctor_dismissals", "[]")
            .await
            .expect("write");
        store
            .set_setting("doctor_dismissals", awkward)
            .await
            .expect("write again");

        assert_eq!(
            store.get_setting("doctor_dismissals").await.expect("read"),
            Some(awkward.to_string())
        );
    }

    pub async fn clearing_a_setting_makes_it_absent_and_clearing_twice_is_not_an_error<
        H: Harness,
    >() {
        let harness = H::start().await;
        let store = harness.store();
        store
            .set_setting("active_run_window", "{}")
            .await
            .expect("write");

        store
            .clear_setting("active_run_window")
            .await
            .expect("clear");
        store
            .clear_setting("active_run_window")
            .await
            .expect("clear an absent key");

        assert_eq!(
            store.get_setting("active_run_window").await.expect("read"),
            None
        );
    }

    pub async fn a_checkout_round_trips_every_column<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        let full = full_checkout("repo-full");
        let bare = bare_checkout("repo-bare");

        store.insert_checkout(&full).await.expect("insert");
        store.insert_checkout(&bare).await.expect("insert");

        assert_eq!(
            store.get_checkout("repo-full").await.expect("read"),
            Some(full)
        );
        assert_eq!(
            store.get_checkout("repo-bare").await.expect("read"),
            Some(bare)
        );
        assert_eq!(store.get_checkout("repo-none").await.expect("read"), None);
    }

    pub async fn a_second_checkout_of_one_repository_is_refused<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        store
            .insert_checkout(&full_checkout("repo-1"))
            .await
            .expect("insert");

        let error = store
            .insert_checkout(&bare_checkout("repo-1"))
            .await
            .expect_err("one checkout per repository");

        assert_refused(
            error,
            "this machine already has a checkout of repository repo-1",
        );
        assert_eq!(
            store.get_checkout("repo-1").await.expect("read"),
            Some(full_checkout("repo-1")),
            "the first checkout is untouched"
        );
    }

    pub async fn patching_a_checkout_changes_only_what_the_patch_sets<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        store
            .insert_checkout(&full_checkout("repo-1"))
            .await
            .expect("insert");

        let patch = CheckoutPatch {
            worktree_root: Some("/elsewhere".to_string()),
            unattended_consent: Some(false),
            on_archive: Some(OnArchive::RemoveWorktree),
            on_archive_script: Patch::Clear,
            credential_label: Patch::Set("renamed".to_string()),
            ..CheckoutPatch::default()
        };
        assert!(store.patch_checkout("repo-1", &patch).await.expect("patch"));

        assert_eq!(
            store.get_checkout("repo-1").await.expect("read"),
            Some(Checkout {
                worktree_root: "/elsewhere".to_string(),
                unattended_consent: false,
                on_archive: OnArchive::RemoveWorktree,
                on_archive_script: None,
                credential_label: Some("renamed".to_string()),
                ..full_checkout("repo-1")
            })
        );
    }

    pub async fn patching_or_removing_an_unknown_checkout_answers_false<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        let patch = CheckoutPatch {
            max_concurrency: Some(2),
            ..CheckoutPatch::default()
        };

        assert!(!store
            .patch_checkout("repo-none", &patch)
            .await
            .expect("patch"));
        assert!(!store.remove_checkout("repo-none").await.expect("remove"));
        assert_eq!(store.list_checkouts().await.expect("list"), Vec::new());
    }

    pub async fn a_worktree_for_an_unknown_checkout_is_refused<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();

        let error = store
            .record_worktree(&worktree("task-1", "repo-none"))
            .await
            .expect_err("a worktree needs a checkout on this machine");

        assert_refused(
            error,
            "this machine has no checkout of repository repo-none",
        );
        assert_eq!(store.list_worktrees().await.expect("list"), Vec::new());
    }

    pub async fn a_checkout_with_worktrees_cannot_be_removed<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        store
            .insert_checkout(&full_checkout("repo-1"))
            .await
            .expect("insert");
        store
            .record_worktree(&worktree("task-1", "repo-1"))
            .await
            .expect("record");

        let error = store
            .remove_checkout("repo-1")
            .await
            .expect_err("a checkout in use stays");

        assert_refused(
            error,
            "repository repo-1 still has worktrees on this machine, so its checkout cannot be \
             removed",
        );
        assert!(store.get_checkout("repo-1").await.expect("read").is_some());

        // Once the worktree is forgotten, nothing holds it.
        assert!(store.forget_worktree("task-1").await.expect("forget"));
        assert!(store.remove_checkout("repo-1").await.expect("remove"));
        assert_eq!(store.get_checkout("repo-1").await.expect("read"), None);
    }

    pub async fn recording_a_tasks_worktree_again_replaces_it<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        store
            .insert_checkout(&full_checkout("repo-1"))
            .await
            .expect("insert");
        store
            .insert_checkout(&bare_checkout("repo-2"))
            .await
            .expect("insert");
        store
            .record_worktree(&worktree("task-1", "repo-1"))
            .await
            .expect("record");

        let fenced = WorktreeRecord {
            path: "/somewhere/else".to_string(),
            fenced_at: Some(at(45)),
            ..worktree("task-1", "repo-2")
        };
        store.record_worktree(&fenced).await.expect("record again");

        assert_eq!(
            store.list_worktrees().await.expect("list"),
            vec![fenced.clone()]
        );
        assert_eq!(
            store.get_worktree("task-1").await.expect("read"),
            Some(fenced)
        );
    }

    pub async fn forgetting_a_worktree_answers_whether_there_was_one<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        store
            .insert_checkout(&full_checkout("repo-1"))
            .await
            .expect("insert");
        store
            .record_worktree(&worktree("task-1", "repo-1"))
            .await
            .expect("record");

        assert!(store.forget_worktree("task-1").await.expect("forget"));
        assert!(!store.forget_worktree("task-1").await.expect("forget again"));
        assert_eq!(store.get_worktree("task-1").await.expect("read"), None);
    }

    pub async fn checkouts_and_worktrees_list_in_key_order<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        for id in ["repo-c", "repo-a", "repo-b"] {
            store
                .insert_checkout(&bare_checkout(id))
                .await
                .expect("insert");
        }
        for (task, repository) in [
            ("task-2", "repo-a"),
            ("task-3", "repo-c"),
            ("task-1", "repo-a"),
        ] {
            store
                .record_worktree(&worktree(task, repository))
                .await
                .expect("record");
        }

        let checkouts: Vec<String> = store
            .list_checkouts()
            .await
            .expect("list")
            .into_iter()
            .map(|checkout| checkout.repository_id)
            .collect();
        let worktrees: Vec<String> = store
            .list_worktrees()
            .await
            .expect("list")
            .into_iter()
            .map(|worktree| worktree.task_id)
            .collect();
        assert_eq!(checkouts, ["repo-a", "repo-b", "repo-c"]);
        assert_eq!(worktrees, ["task-1", "task-2", "task-3"]);
    }

    pub async fn every_schedule_column_round_trips<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        // Every nullable column set, on a repeating schedule.
        let set = schedule("schedule-set", "Every column set");
        // Every nullable column `NULL`: no cron, no start, no zone, no stop,
        // never fired and never armed. Not a row the service writes, which is
        // the point: the store holds what it is given.
        let null = Schedule {
            id: "schedule-null".to_string(),
            name: "Every column null".to_string(),
            mode: ScheduleMode::Sequential,
            cron: None,
            start_at: None,
            max_concurrency: 1,
            enabled: false,
            timezone: None,
            stop_at: None,
            last_fired_at: None,
            armed_at: None,
        };
        // And `start_at`, which a repeating schedule leaves `NULL`, set.
        let one_off = Schedule {
            id: "schedule-one-off".to_string(),
            cron: None,
            start_at: Some(at(600)),
            ..schedule("schedule-one-off", "A one-off")
        };

        for schedule in [&set, &null, &one_off] {
            store.insert_schedule(schedule).await.expect("insert");
        }

        for schedule in [set, null, one_off] {
            assert_eq!(
                store.get_schedule(&schedule.id).await.expect("read"),
                Some(schedule)
            );
        }
        assert_eq!(
            store.get_schedule("schedule-none").await.expect("read"),
            None
        );
    }

    pub async fn a_second_schedule_with_one_id_is_refused<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        store
            .insert_schedule(&schedule("schedule-1", "Nightly"))
            .await
            .expect("insert");

        let error = store
            .insert_schedule(&schedule("schedule-1", "Another"))
            .await
            .expect_err("one schedule per id");

        assert_refused(error, "a schedule with id schedule-1 already exists");
        assert_eq!(
            store
                .get_schedule("schedule-1")
                .await
                .expect("read")
                .map(|schedule| schedule.name),
            Some("Nightly".to_string())
        );
    }

    pub async fn updating_a_schedule_keeps_its_last_fire<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        let original = schedule("schedule-1", "Nightly");
        store.insert_schedule(&original).await.expect("insert");

        let edited = Schedule {
            name: "Weeknights".to_string(),
            mode: ScheduleMode::Sequential,
            cron: Some("0 23 * * 1-5".to_string()),
            max_concurrency: 1,
            enabled: false,
            timezone: Some("Europe/London".to_string()),
            stop_at: None,
            last_fired_at: Some(at(999)),
            armed_at: None,
            ..original.clone()
        };
        assert!(store.update_schedule(&edited).await.expect("update"));

        assert_eq!(
            store.get_schedule("schedule-1").await.expect("read"),
            Some(Schedule {
                last_fired_at: original.last_fired_at,
                ..edited
            })
        );
    }

    pub async fn enabling_and_firing_write_only_their_columns<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        let original = schedule("schedule-1", "Nightly");
        store.insert_schedule(&original).await.expect("insert");

        assert!(store
            .set_schedule_enabled("schedule-1", false, None)
            .await
            .expect("disable"));
        assert!(store
            .record_schedule_fire("schedule-1", at(120))
            .await
            .expect("fire"));

        assert_eq!(
            store.get_schedule("schedule-1").await.expect("read"),
            Some(Schedule {
                enabled: false,
                armed_at: None,
                last_fired_at: Some(at(120)),
                ..original
            })
        );
    }

    pub async fn a_write_to_an_unknown_schedule_answers_false<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();

        assert!(!store
            .update_schedule(&schedule("schedule-none", "Nobody"))
            .await
            .expect("update"));
        assert!(!store
            .set_schedule_enabled("schedule-none", true, Some(at(1)))
            .await
            .expect("enable"));
        assert!(!store
            .record_schedule_fire("schedule-none", at(1))
            .await
            .expect("fire"));
        assert!(!store
            .delete_schedule("schedule-none")
            .await
            .expect("delete"));
        assert_eq!(store.list_schedules().await.expect("list"), Vec::new());
    }

    pub async fn schedules_list_by_name_then_id<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        for (id, name) in [
            ("schedule-3", "Nightly"),
            ("schedule-1", "Weekend"),
            ("schedule-2", "Nightly"),
            ("schedule-0", "Afternoon"),
        ] {
            store
                .insert_schedule(&schedule(id, name))
                .await
                .expect("insert");
        }
        assert!(store.delete_schedule("schedule-1").await.expect("delete"));

        let listed: Vec<(String, String)> = store
            .list_schedules()
            .await
            .expect("list")
            .into_iter()
            .map(|schedule| (schedule.name, schedule.id))
            .collect();
        assert_eq!(
            listed,
            [
                ("Afternoon".to_string(), "schedule-0".to_string()),
                ("Nightly".to_string(), "schedule-2".to_string()),
                ("Nightly".to_string(), "schedule-3".to_string()),
            ]
        );
    }

    pub async fn a_held_lease_round_trips_every_column<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        let full = held_lease("task-1", 7);
        // `run_id` NULL, as a lease is until `start_run`, and every purpose's
        // spelling through the column.
        let bare = HeldLease {
            purpose: LeasePurpose::Strategy,
            run_id: None,
            ..held_lease("task-2", 1)
        };

        store.record_held_lease(&full).await.expect("record");
        store.record_held_lease(&bare).await.expect("record");
        for (task, purpose) in [
            ("task-3", LeasePurpose::Implementation),
            ("task-4", LeasePurpose::Fix),
        ] {
            store
                .record_held_lease(&HeldLease {
                    purpose,
                    ..held_lease(task, 1)
                })
                .await
                .expect("record");
        }

        let listed = store.list_held_leases().await.expect("list");
        assert_eq!(
            listed
                .iter()
                .map(|lease| (lease.task_id.as_str(), lease.purpose))
                .collect::<Vec<_>>(),
            [
                ("task-1", LeasePurpose::Review),
                ("task-2", LeasePurpose::Strategy),
                ("task-3", LeasePurpose::Implementation),
                ("task-4", LeasePurpose::Fix),
            ],
            "listed in task order, every purpose intact"
        );
        assert_eq!(listed[0], full);
        assert_eq!(listed[1], bare);
    }

    pub async fn recording_a_held_lease_on_a_task_again_replaces_it<H: Harness>() {
        // The primary key on `task_id`: one row per task, and a second record
        // replaces the first rather than refusing, because the board holds at
        // most one lease per task and a row a missed forget left behind must
        // not refuse the next claim's record.
        let harness = H::start().await;
        let store = harness.store();
        store
            .record_held_lease(&held_lease("task-1", 1))
            .await
            .expect("record");

        let reclaimed = HeldLease {
            purpose: LeasePurpose::Implementation,
            run_id: None,
            acquired_at: at(90),
            ..held_lease("task-1", 2)
        };
        store
            .record_held_lease(&reclaimed)
            .await
            .expect("record again");

        assert_eq!(
            store.list_held_leases().await.expect("list"),
            vec![reclaimed]
        );
    }

    pub async fn noting_a_held_leases_run_writes_the_run_and_the_purpose<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        let recorded = HeldLease {
            purpose: LeasePurpose::Strategy,
            run_id: None,
            ..held_lease("task-1", 3)
        };
        store.record_held_lease(&recorded).await.expect("record");

        assert!(store
            .set_held_lease_run("task-1", Some("run-9"), LeasePurpose::Implementation)
            .await
            .expect("note the run"));
        assert_eq!(
            store.list_held_leases().await.expect("list"),
            vec![HeldLease {
                purpose: LeasePurpose::Implementation,
                run_id: Some("run-9".to_string()),
                ..recorded.clone()
            }],
            "the run and the purpose, and nothing else"
        );

        assert!(store
            .set_held_lease_run("task-1", None, LeasePurpose::Review)
            .await
            .expect("clear the run"));
        assert_eq!(
            store.list_held_leases().await.expect("list"),
            vec![HeldLease {
                purpose: LeasePurpose::Review,
                run_id: None,
                ..recorded
            }]
        );
        assert!(!store
            .set_held_lease_run("task-2", Some("run-9"), LeasePurpose::Fix)
            .await
            .expect("an unknown task"));
    }

    pub async fn forgetting_a_held_lease_answers_whether_there_was_one<H: Harness>() {
        let harness = H::start().await;
        let store = harness.store();
        store
            .record_held_lease(&held_lease("task-1", 1))
            .await
            .expect("record");
        store
            .record_held_lease(&held_lease("task-2", 1))
            .await
            .expect("record");

        assert!(store.forget_held_lease("task-1").await.expect("forget"));
        assert!(!store.forget_held_lease("task-1").await.expect("again"));
        assert_eq!(
            store
                .list_held_leases()
                .await
                .expect("list")
                .into_iter()
                .map(|lease| lease.task_id)
                .collect::<Vec<_>>(),
            ["task-2"]
        );
    }
}
