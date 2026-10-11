//! The one way a board command runs (task 046, seam-contract D32 point 2).
//!
//! `api::dispatch` looks a name up, defaults its arguments, re-scopes the host
//! context from the caller and runs the handler. The solo shell's
//! `dispatch_with_machine` is the same path with this machine on the request,
//! which is what keeps archive, move-to-done, approve, reject and repository
//! removal reacting on disk in solo (task 046 point 3a).
//!
//! Git and the filesystem are real: worktrees are `git worktree add`ed in a
//! `TempDir`, as everywhere else in this crate.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use rimaia_core::api::{self, BoardHost, Caller, Door, TeamGrant};
use rimaia_core::archive::{self, OnArchiveOutcome};
use rimaia_core::db::{BoardColumn, OnArchive, Repository, RunState, Task};
use rimaia_core::identity::rate_limit::RateLimiter;
use rimaia_core::identity::Role;
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::provider::{ClaudeProvider, ProviderProfile};
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::api as testing_api;
use rimaia_core::testing::teams::TwoTeams;
use rimaia_core::testing::{TempRepo, TestContext};
use rimaia_core::worktree::{self, AutoCleanup};
use rimaia_core::{AppPaths, Audience, ErrorCode};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// What dispatch does before any handler runs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatch_refuses_a_local_or_unknown_name_as_not_found() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;

    for name in ["get_app_info", "start_task_run", "no_such_command"] {
        let refused = api::dispatch(&host, team_a(&teams), name, json!({}))
            .await
            .expect_err(name);
        assert_eq!(refused.code(), ErrorCode::NotFound, "{name}");
        assert_eq!(
            refused.to_string(),
            format!("there is no board command `{name}`")
        );
    }
}

#[tokio::test]
async fn dispatch_reads_null_arguments_as_an_empty_object() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;

    let from_null = api::dispatch(&host, team_a(&teams), "list_repositories", Value::Null)
        .await
        .expect("null reads as {}");
    let from_object = api::dispatch(&host, team_a(&teams), "list_repositories", json!({}))
        .await
        .expect("{}");

    assert_eq!(from_null, from_object);
}

#[tokio::test]
async fn a_malformed_argument_is_invalid_and_names_the_command() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;

    for args in [json!({}), json!({ "id": 7 }), json!([1, 2]), json!("an id")] {
        let refused = api::dispatch(&host, team_a(&teams), "get_task", args.clone())
            .await
            .expect_err("get_task needs a string id");
        assert_eq!(refused.code(), ErrorCode::Invalid, "{args}");
        assert!(
            refused
                .to_string()
                .starts_with("`get_task` could not read its arguments: "),
            "{refused}"
        );
    }
}

#[tokio::test]
async fn dispatch_rescopes_from_the_caller_and_never_hands_over_the_host_context() {
    let teams = TwoTeams::new().await;
    // A base context that names team A, and a caller whose only team is B.
    let host = testing_api::host_over(&teams, &teams.a).await;
    let only_b = testing_api::owner(&teams.team_b, testing_api::desktop("b"));

    let repositories = api::dispatch(&host, only_b.clone(), "list_repositories", json!({}))
        .await
        .expect("B lists its own repositories");
    let ids: Vec<&str> = repositories
        .as_array()
        .expect("a list")
        .iter()
        .map(|repository| repository["id"].as_str().expect("an id"))
        .collect();
    assert_eq!(ids, vec![teams.team_b.repository.id.as_str()]);

    let a_task = api::dispatch(
        &host,
        only_b,
        "get_task",
        json!({ "id": teams.team_a.ready }),
    )
    .await
    .expect_err("team A's task does not exist for B");
    assert_eq!(a_task.code(), ErrorCode::NotFound);
}

#[tokio::test]
async fn dispatch_writes_as_the_callers_user_through_the_callers_door() {
    let mut teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let runner = Caller {
        door: Door::Runner {
            runner_id: teams.team_a.runner_id.clone(),
        },
        ..team_a(&teams)
    };
    teams.drain_changes();

    let created = api::dispatch(
        &host,
        runner,
        "create_task",
        json!({ "input": { "repositoryId": teams.team_a.repository.id, "title": "From a runner" } }),
    )
    .await
    .expect("create");

    let row: (String, String) =
        sqlx::query_as("SELECT source, created_by FROM tasks WHERE id = ?1")
            .bind(created["id"].as_str().expect("an id"))
            .fetch_one(&teams.a.pool)
            .await
            .expect("read the row back");
    assert_eq!(row, ("system".to_string(), teams.team_a.owner_id.clone()));
    let events = teams.drain_changes();
    assert!(
        events
            .iter()
            .all(|event| event.audience == Audience::Team(teams.team_a.team_id.clone())),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_caller_with_no_team_is_refused_before_any_handler_runs() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let nobody = Caller {
        teams: Vec::new(),
        ..team_a(&teams)
    };

    let refused = api::dispatch(&host, nobody, "list_repositories", json!({}))
        .await
        .expect_err("no team, no scope");

    assert_eq!(refused.code(), ErrorCode::Invalid);
}

// ---------------------------------------------------------------------------
// Invoke payloads are unchanged
// ---------------------------------------------------------------------------

/// The argument objects `src/**/*.test.ts(x)` assert `invoke` is called with,
/// dispatched as they are. Every one succeeds, so the frontend sends nothing
/// that the move from Tauri's derived parameters to argument structs broke.
#[tokio::test]
async fn invoke_payloads_dispatch_unchanged() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let a = &teams.team_a;

    let payloads = [
        // Board.test.tsx: a new card from "n".
        (
            "create_task",
            json!({ "input": { "repositoryId": a.repository.id, "title": "Untitled task" } }),
        ),
        // TitleField, PlanEditor, ExtraInstructionsEditor, StrategySection,
        // RepositorySelector.
        (
            "update_task",
            json!({ "id": a.ready, "patch": { "title": "Wire the panel" } }),
        ),
        (
            "update_task",
            json!({ "id": a.ready, "patch": { "plan": "new plan text" } }),
        ),
        (
            "update_task",
            json!({ "id": a.ready, "patch": { "extraInstructions": null } }),
        ),
        (
            "update_task",
            json!({ "id": a.ready, "patch": { "strategyMode": "planned" } }),
        ),
        (
            "update_task",
            json!({ "id": a.ready, "patch": { "model": null } }),
        ),
        (
            "update_task",
            json!({ "id": a.ready, "patch": { "repositoryId": a.repository.id } }),
        ),
        // Board.test.tsx's drag: `{ id, column, beforeId, afterId }`.
        (
            "move_task",
            json!({ "id": a.not_ready, "column": "ready", "beforeId": a.ready, "afterId": null }),
        ),
        // RunsView.test.tsx's filters.
        ("list_runs", json!({ "filter": {} })),
        (
            "list_runs",
            json!({ "filter": { "repositoryId": a.repository.id } }),
        ),
        ("list_runs", json!({ "filter": { "kind": "review" } })),
        // commands.test.ts and RepositoriesSection.test.tsx.
        (
            "set_strategy_defaults",
            json!({ "repositoryId": null, "value": { "mode": "default" } }),
        ),
        (
            "set_strategy_defaults",
            json!({ "repositoryId": a.repository.id, "value": { "mode": "default", "model": "team-a-model" } }),
        ),
    ];
    for (name, args) in payloads {
        api::dispatch(&host, team_a(&teams), name, args.clone())
            .await
            .unwrap_or_else(|error| panic!("{name} {args}: {error}"));
    }

    // commands.test.ts: `give_up_on_task` with `{ taskId }`, on a task that is
    // waiting out a retry, which is the only one giving up applies to.
    for run_state in [RunState::Queued, RunState::Running, RunState::WaitingRetry] {
        tasks::set_run_state(&teams.a, &a.ready, run_state)
            .await
            .expect("a legal run-state transition");
    }
    api::dispatch(
        &host,
        team_a(&teams),
        "give_up_on_task",
        json!({ "taskId": a.ready }),
    )
    .await
    .expect("give up on a task waiting to retry");
}

// ---------------------------------------------------------------------------
// The case table's own arguments
// ---------------------------------------------------------------------------

/// Each case's `own` arguments, against a fresh copy of the fixture, succeed
/// for team A's owner, except the few this fixture cannot make succeed, which
/// are refused and say why in `REFUSED_FOR_TEAM_A`.
#[tokio::test]
async fn every_case_succeeds_for_team_a_with_its_own_arguments() {
    let teams = TwoTeams::new().await;

    let mut failures = Vec::new();
    for case in testing_api::BOARD_CASES {
        let (copy, _dir) = testing_api::twin(&teams).await;
        let host = testing_api::host_over(&teams, &copy).await;
        let answer = api::dispatch(&host, team_a(&teams), case.name, (case.own)(&teams)).await;
        let refused = testing_api::REFUSED_FOR_TEAM_A
            .iter()
            .any(|(name, _)| *name == case.name);
        match (answer, refused) {
            (Ok(_), false) | (Err(_), true) => {}
            (Err(error), false) => failures.push(format!("{}: refused: {error}", case.name)),
            (Ok(answer), true) => failures.push(format!(
                "{}: listed as refused, but succeeded: {answer}",
                case.name
            )),
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ---------------------------------------------------------------------------
// Solo keeps its machine reactions (task 046 point 3a)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_solo_archive_through_dispatch_runs_the_on_archive_policy() {
    let mut solo = Solo::new().await;
    let repository = solo.repository().await;
    solo.on_archive(&repository, OnArchive::RemoveWorktree)
        .await;
    let task = solo
        .task(&repository, "Finished with this", BoardColumn::Ready)
        .await;
    let checkout = solo.worktree(&task.id).await;

    let archived = solo
        .solo("archive_task", json!({ "id": task.id }))
        .await
        .expect("archive");

    assert_eq!(archived["cleanup"]["kind"], "worktreeRemoved");
    assert!(
        archived["cleanup"]["bytes_freed"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0),
        "{archived}"
    );
    assert!(!checkout.exists());
    assert_eq!(solo.harness.worktree_path(&task.id).await, None);

    // The bulk archive reacts the same way.
    let other = solo
        .task(&repository, "Finished too", BoardColumn::Ready)
        .await;
    let other_checkout = solo.worktree(&other.id).await;
    let report = solo
        .solo("archive_tasks", json!({ "ids": [other.id] }))
        .await
        .expect("archive many");
    assert_eq!(report["archived"][0]["cleanup"]["kind"], "worktreeRemoved");
    assert!(!other_checkout.exists());
}

#[tokio::test]
async fn a_solo_move_to_done_through_dispatch_runs_the_auto_removal() {
    let mut solo = Solo::new().await;
    let repository = solo.repository().await;
    solo.auto_cleanup().await;
    let task = solo.task(&repository, "Ship it", BoardColumn::Ready).await;
    let checkout = solo.worktree(&task.id).await;

    let moved = solo
        .solo("move_task", json!({ "id": task.id, "column": "done" }))
        .await
        .expect("move to done");

    assert_eq!(moved["column"], "done");
    assert!(!checkout.exists());
    assert_eq!(solo.harness.worktree_path(&task.id).await, None);
}

#[tokio::test]
async fn a_solo_approve_and_reject_through_dispatch_reach_the_worktree() {
    let mut solo = Solo::new().await;
    let repository = solo.repository().await;
    solo.auto_cleanup().await;

    // Approve, with auto-cleanup on, removes the worktree.
    let approved = solo.in_review(&repository, "Approve me").await;
    let approved_checkout = solo.worktree(&approved.id).await;
    solo.solo("approve_task", json!({ "taskId": approved.id }))
        .await
        .expect("approve");
    assert!(!approved_checkout.exists());
    assert_eq!(solo.harness.worktree_path(&approved.id).await, None);

    // Reject removes it and keeps the branch.
    let rejected = solo.in_review(&repository, "Reject me").await;
    let rejected_checkout = solo.worktree(&rejected.id).await;
    let branch = solo.reload(&rejected.id).await.branch.expect("a branch");
    let outcome = solo
        .solo(
            "reject_task",
            json!({ "taskId": rejected.id, "note": "Not this." }),
        )
        .await
        .expect("reject");
    assert_eq!(outcome["setAsideBranch"], json!(branch));
    assert!(!rejected_checkout.exists());
    assert_eq!(solo.harness.worktree_path(&rejected.id).await, None);
    assert!(
        solo.branch_exists(&repository, &branch),
        "set aside, never deleted"
    );

    // Reject of a dirty worktree is still refused, with nothing written.
    let dirty = solo.in_review(&repository, "Dirty").await;
    let dirty_checkout = solo.worktree(&dirty.id).await;
    std::fs::write(dirty_checkout.join("stray.txt"), "x\n").expect("dirty the worktree");
    let before = solo.reload(&dirty.id).await;
    let refused = solo
        .solo(
            "reject_task",
            json!({ "taskId": dirty.id, "note": "Not this." }),
        )
        .await
        .expect_err("a dirty worktree refuses the verdict");
    assert_eq!(refused.code(), ErrorCode::Invalid);
    assert!(
        refused.to_string().contains("uncommitted change"),
        "{refused}"
    );
    assert_eq!(solo.reload(&dirty.id).await, before);
    assert!(dirty_checkout.join("stray.txt").exists());
}

#[tokio::test]
async fn a_solo_remove_repository_through_dispatch_removes_the_checkout() {
    let mut solo = Solo::new().await;
    let repository = solo.repository().await;
    let task = solo
        .task(&repository, "Deleted later", BoardColumn::Ready)
        .await;
    solo.worktree(&task.id).await;
    tasks::delete_task(&solo.harness.context, &task.id)
        .await
        .expect("delete the task, which leaves its worktree record");

    solo.solo("remove_repository", json!({ "id": repository.id }))
        .await
        .expect("remove");

    let store = &solo.harness.machine().store;
    assert_eq!(
        store.get_checkout(&repository.id).await.expect("read"),
        None
    );
    assert!(store
        .list_worktrees()
        .await
        .expect("read")
        .iter()
        .all(|record| record.repository_id != repository.id));
}

#[tokio::test]
async fn the_server_dispatch_passes_no_machine() {
    let mut solo = Solo::new().await;
    let repository = solo.repository().await;
    solo.on_archive(&repository, OnArchive::RemoveWorktree)
        .await;
    solo.auto_cleanup().await;

    // archive_task and archive_tasks leave the worktree, and report nothing.
    let archived = solo.task(&repository, "Archived", BoardColumn::Ready).await;
    let archived_checkout = solo.worktree(&archived.id).await;
    let answer = solo
        .server("archive_task", json!({ "id": archived.id }))
        .await
        .expect("archive");
    // Task 054 changes this to `Pending` for a task with a `runs` row naming a
    // runner: the board records the cleanup for the holding runner's heartbeat.
    assert_eq!(
        answer["cleanup"],
        serde_json::to_value(OnArchiveOutcome::Nothing).expect("serializes")
    );
    assert!(archived_checkout.exists());

    let bulk = solo
        .task(&repository, "Archived in bulk", BoardColumn::Ready)
        .await;
    let bulk_checkout = solo.worktree(&bulk.id).await;
    let report = solo
        .server("archive_tasks", json!({ "ids": [bulk.id] }))
        .await
        .expect("archive many");
    assert_eq!(report["archived"][0]["cleanup"]["kind"], "nothing");
    assert!(bulk_checkout.exists());

    // move_task to done and approve_task leave it, with auto-cleanup on.
    let moved = solo.task(&repository, "Moved", BoardColumn::Ready).await;
    let moved_checkout = solo.worktree(&moved.id).await;
    solo.server("move_task", json!({ "id": moved.id, "column": "done" }))
        .await
        .expect("move to done");
    assert!(moved_checkout.exists());

    let approved = solo.in_review(&repository, "Approved").await;
    let approved_checkout = solo.worktree(&approved.id).await;
    solo.server("approve_task", json!({ "taskId": approved.id }))
        .await
        .expect("approve");
    assert!(approved_checkout.exists());

    // reject_task writes its board half and leaves the worktree, dirty or not.
    for dirty in [false, true] {
        let rejected = solo.in_review(&repository, "Rejected").await;
        let checkout = solo.worktree(&rejected.id).await;
        if dirty {
            std::fs::write(checkout.join("stray.txt"), "x\n").expect("dirty the worktree");
        }
        let outcome = solo
            .server(
                "reject_task",
                json!({ "taskId": rejected.id, "note": "Not this." }),
            )
            .await
            .expect("no machine, so nothing on disk to refuse over");
        assert_eq!(outcome["task"]["column"], "ready");
        assert_eq!(outcome["task"]["branch"], Value::Null);
        assert!(checkout.exists(), "dirty: {dirty}");
        assert!(solo.harness.worktree_path(&rejected.id).await.is_some());
    }

    // remove_repository removes the board row and leaves the checkout and
    // its worktree records.
    let doomed = solo.repository().await;
    let task = solo
        .task(&doomed, "Deleted later", BoardColumn::Ready)
        .await;
    solo.worktree(&task.id).await;
    tasks::delete_task(&solo.harness.context, &task.id)
        .await
        .expect("delete the task");
    solo.server("remove_repository", json!({ "id": doomed.id }))
        .await
        .expect("remove");
    assert_eq!(
        repo::get(&solo.harness.context, &doomed.id)
            .await
            .expect_err("the board row is gone")
            .code(),
        ErrorCode::NotFound
    );
    let store = &solo.harness.machine().store;
    assert!(store
        .get_checkout(&doomed.id)
        .await
        .expect("read")
        .is_some());
    assert!(solo.harness.worktree_path(&task.id).await.is_some());
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Team A's owner, through a connected desktop's door.
fn team_a(teams: &TwoTeams) -> Caller {
    testing_api::owner(&teams.team_a, testing_api::desktop("team-a"))
}

/// The solo shell's view: the solo identity's caller, a host over the
/// harness's context, and the harness's machine.
struct Solo {
    harness: TestContext,
    host: BoardHost,
    caller: Caller,
    worktrees: tempfile::TempDir,
    /// Each registered repository's id and its source clone.
    sources: Vec<(String, TempRepo)>,
    _data: tempfile::TempDir,
}

impl Solo {
    async fn new() -> Self {
        let harness = TestContext::new().await;
        let data = tempfile::Builder::new()
            .prefix("rimaia-data-")
            .tempdir()
            .expect("a data directory");
        let worktrees = tempfile::Builder::new()
            .prefix("rimaia-worktrees ")
            .tempdir()
            .expect("a worktree root");
        let host = BoardHost {
            context: harness.context.clone(),
            provider: ProviderProfile::of(&ClaudeProvider),
            paths: AppPaths::new(data.path()),
            rate_limits: Arc::new(RateLimiter::new(Arc::new(harness.clock.clone()))),
            tails: rimaia_core::api::TailRelay::new(
                rimaia_core::api::tail::SOLO_TAIL_RELAY_CAPACITY,
            ),
            streams: rimaia_core::api::StreamControl::new(),
        };
        let caller = Caller::solo(&harness.solo);
        assert_eq!(
            caller.teams,
            vec![TeamGrant {
                team_id: harness.solo.team_id.clone(),
                role: Role::Owner,
            }]
        );
        Self {
            harness,
            host,
            caller,
            worktrees,
            sources: Vec::new(),
            _data: data,
        }
    }

    /// What `commands::board::route` sends: the solo caller, with this
    /// machine.
    async fn solo(&self, name: &str, args: Value) -> rimaia_core::Result<Value> {
        api::dispatch_with_machine(
            &self.host,
            self.caller.clone(),
            self.harness.machine(),
            name,
            args,
        )
        .await
    }

    /// What a server sends: the same caller, no machine.
    async fn server(&self, name: &str, args: Value) -> rimaia_core::Result<Value> {
        api::dispatch(&self.host, self.caller.clone(), name, args).await
    }

    async fn repository(&mut self) -> Repository {
        let source = TempRepo::init();
        let repository = repo::register(
            &self.harness.context,
            self.harness.machine(),
            self.worktrees.path(),
            NewRepository {
                path: source.path().to_string_lossy().into_owned(),
                name: Some(format!("repository {}", self.sources.len() + 1)),
                worktree_root: None,
            },
        )
        .await
        .expect("register a repository");
        self.sources.push((repository.id.clone(), source));
        repository
    }

    async fn on_archive(&self, repository: &Repository, mode: OnArchive) {
        archive::set_repository_on_archive(
            &self.harness.context,
            self.harness.machine(),
            &repository.id,
            mode,
            None,
        )
        .await
        .expect("set the on-archive policy");
    }

    async fn auto_cleanup(&self) {
        worktree::set_auto_cleanup(self.harness.machine(), AutoCleanup::OnDoneAcknowledged)
            .await
            .expect("turn auto-cleanup on");
    }

    async fn task(&self, repository: &Repository, title: &str, column: BoardColumn) -> Task {
        tasks::create_task(
            &self.harness.context,
            NewTask {
                repository_id: repository.id.clone(),
                title: title.to_string(),
                plan: Some("1. Do the work".to_string()),
                extra_instructions: None,
                column: Some(column),
                links: Vec::new(),
            },
        )
        .await
        .expect("create a task")
    }

    /// A task sitting in review, alone in its column so it lands with no
    /// neighbours to name.
    async fn in_review(&self, repository: &Repository, title: &str) -> Task {
        let task = self.task(repository, title, BoardColumn::Ready).await;
        let reviewing = tasks::list_tasks(
            &self.harness.context,
            tasks::TaskFilter {
                column: Some(BoardColumn::InReview),
                ..Default::default()
            },
        )
        .await
        .expect("read the review column");
        tasks::move_task(
            &self.harness.context,
            None,
            &task.id,
            BoardColumn::InReview,
            reviewing.last().map(|summary| summary.task.id.as_str()),
            None,
        )
        .await
        .expect("move into review")
    }

    /// A real worktree for `task_id`, recorded on this machine.
    async fn worktree(&self, task_id: &str) -> PathBuf {
        let prepared = self
            .harness
            .prepare_worktree(task_id)
            .await
            .expect("prepare a worktree");
        let path = PathBuf::from(prepared.path);
        assert!(path.exists());
        path
    }

    /// Whether `branch` exists in `repository`'s source clone.
    fn branch_exists(&self, repository: &Repository, branch: &str) -> bool {
        let (_, source) = self
            .sources
            .iter()
            .find(|(id, _)| *id == repository.id)
            .expect("a repository this test registered");
        git_branch_exists(source.path(), branch)
    }

    async fn reload(&self, task_id: &str) -> Task {
        tasks::get_task(&self.harness.context, task_id)
            .await
            .expect("read the task")
            .task
    }
}

fn git_branch_exists(source: &Path, branch: &str) -> bool {
    Command::new("git")
        .current_dir(source)
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .status()
        .expect("run git show-ref")
        .success()
}
