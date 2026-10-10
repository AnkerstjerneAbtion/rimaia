//! Who may start a run, and with which model (task 067; ADR-0031 points 1 and
//! 7, ADR-0012 point 6, ADR-0029 point 5).
//!
//! The board port's own cases for both rules are in the contract suite
//! (`testing::board_contract`, the `…model…` cases, `run_now_is_not_bound_by_capacity`
//! and `only_a_runners_owner_is_authorized_to_start_it`). This file holds what
//! the start doors do with them: Run now, Retry now and Plan now through the
//! core starters and the operator MCP tools, and a run that reaches the CLI.
//!
//! The CLI is `testing::FakeCli` replaying recorded streams, git runs against
//! real repositories in temporary directories, and the clock is the harness's.
//! Nothing sleeps: the `timeout`s are failure bounds, not waits.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use pretty_assertions::assert_eq;
use rimaia_core::board::lease::{self, LeaseState};
use rimaia_core::board::{authorize_start, BoardPort, ClaimTarget, FreeCapacity, OwnerPresence};
use rimaia_core::db::{BoardColumn, RunState};
use rimaia_core::identity::create_personal_team;
use rimaia_core::machine::leases;
use rimaia_core::mcp::requests::{PlanSelectionRequest, TaskStrategyRequest};
use rimaia_core::mcp::server::{LocalTools, RimaiaServer};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::provider::{ClaudeProvider, PermissionMode};
use rimaia_core::runner::strategy::{claim_for_planning, PlannerAccess};
use rimaia_core::runner::{
    claim_manual_start, run_task, ManualStart, RunRequest, RunTrigger, RunnerConfig, Starter,
};
use rimaia_core::scheduler::{InFlight, SlotOwner};
use rimaia_core::tasks::{self, NewTask, Patch, TaskDetail, TaskPatch};
use rimaia_core::testing::db::{insert_member, insert_runner, unpair_runner};
use rimaia_core::testing::{self, FakeCli, TempRepo, TestContext};
use rimaia_core::{AppPaths, ErrorCode, ServiceContext};
use rmcp::handler::server::wrapper::Parameters;
use tempfile::TempDir;

/// A failure bound for anything that waits on a child.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

const OWNER_ONLY: &str = "only the owner of this runner can start a run on it; assign the task \
                          to them, or leave it ready for their queue";

const UNPAIRED: &str = "this runner has been unpaired and can no longer start runs";

// ---------------------------------------------------------------------------
// Who may start a run
// ---------------------------------------------------------------------------

#[tokio::test]
async fn only_a_runners_owner_can_start_a_run_on_it() {
    // The solo user names the runner of a second member of the same team. A
    // teammate makes a task claimable by assigning it; starting a process on
    // someone else's machine is not a board action (ADR-0031 point 7).
    let f = Fixture::new().await;
    let theirs = f.teammates_runner().await;
    let starter = f.starter(&theirs, OwnerPresence::AtRunner);

    for continue_session in [false, true] {
        let error = f
            .start_by_hand(starter, continue_session)
            .await
            .expect_err("a teammate's runner is not the solo user's to start");
        assert_eq!(error.code(), ErrorCode::Invalid);
        assert_eq!(error.to_string(), OWNER_ONLY);
    }
    f.assert_nothing_written().await;

    let planned = f.planned_task().await;
    let before = f.detail_of(&planned).await;
    let error = f
        .plan_now(starter, &planned)
        .await
        .expect_err("Plan now is refused the same way");
    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(error.to_string(), OWNER_ONLY);
    assert_eq!(f.detail_of(&planned).await, before, "nothing was written");
    assert_eq!(f.lease_state(&planned).await, unclaimed());
    f.assert_nothing_written().await;
}

#[tokio::test]
async fn a_runner_outside_the_callers_team_is_not_found() {
    // ADR-0029 point 5: a runner whose owner is in no team the caller reaches
    // is answered exactly as a runner that does not exist.
    let f = Fixture::new().await;
    let outsider = f.outsiders_runner().await;
    let never_issued = rimaia_core::db::new_id();

    for runner_id in [&outsider, &never_issued] {
        let error = f
            .start_by_hand(f.starter(runner_id, OwnerPresence::AtRunner), false)
            .await
            .expect_err("a runner outside the team is not there");
        assert_eq!(error.code(), ErrorCode::NotFound);
        assert_eq!(error.to_string(), format!("no runner with id {runner_id}"));
    }
    f.assert_nothing_written().await;
}

#[tokio::test]
async fn an_unpaired_runner_is_refused() {
    let f = Fixture::new().await;
    f.unpair_solo_runner().await;

    let error = f
        .start_by_hand(f.harness.starter(OwnerPresence::AtRunner), false)
        .await
        .expect_err("an unpaired runner starts nothing");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(error.to_string(), UNPAIRED);
    f.assert_nothing_written().await;
}

#[tokio::test]
async fn presence_decides_the_permission_posture() {
    // ADR-0031 point 7: whether the owner is at the machine decides the
    // posture, not which button was pressed.
    let f = Fixture::new().await;
    let runner = &f.harness.solo.runner_id;

    let at_runner = authorize_start(f.ctx(), runner, OwnerPresence::AtRunner)
        .await
        .expect("the owner at their runner");
    assert_eq!(at_runner, RunTrigger::Manual);
    assert_eq!(at_runner.permission_mode(), PermissionMode::AcceptEdits);

    let remote = authorize_start(f.ctx(), runner, OwnerPresence::Remote)
        .await
        .expect("the owner away from their runner");
    assert_eq!(remote, RunTrigger::Queued);
    assert_eq!(remote.permission_mode(), PermissionMode::BypassPermissions);

    // A remote start is an unattended run, so it is held to ADR-0012's
    // per-repository opt-in exactly as a queued one is, before anything is
    // written.
    repo::set_allow_unattended_runs(f.ctx(), f.harness.machine(), &f.repository_id, false)
        .await
        .expect("withdraw the opt-in");
    let error = f
        .start_by_hand(f.harness.starter(OwnerPresence::Remote), false)
        .await
        .expect_err("a remote start on a repository without the opt-in");
    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        format!(
            "\"{}\" has not enabled unattended agent runs. Enable it in Settings → Repositories \
             before starting tasks here.",
            f.repository_name
        ),
    );
    f.assert_nothing_written().await;
}

#[tokio::test]
async fn every_start_door_asks_as_an_owner_at_the_runner() {
    // Every door in solo is a desktop command or the loopback operator MCP
    // server on the runner's own machine, so each asks through
    // `Starter::at_runner`. With the solo runner unpaired, each one reaches
    // `authorize_start` and is refused in its sentence before it claims.
    // `src-tauri`'s four commands build the same `Starter::at_runner` from
    // `AppState`; the MCP tools are reached here through their handlers.
    let f = Fixture::new().await;
    let at_runner = Starter::at_runner(f.ctx(), &f.harness.solo.runner_id);
    assert_eq!(at_runner.presence, OwnerPresence::AtRunner);
    let planned = f.planned_task().await;
    f.unpair_solo_runner().await;

    // Run now and Retry now: `start_task_run` and `retry_task_now`.
    for continue_session in [false, true] {
        let error = f
            .start_by_hand(at_runner, continue_session)
            .await
            .expect_err("refused");
        assert_eq!(error.to_string(), UNPAIRED);
    }
    // Plan now: the `plan_task_strategy` command.
    let error = f.plan_now(at_runner, &planned).await.expect_err("refused");
    assert_eq!(error.to_string(), UNPAIRED);

    // The two operator MCP tools.
    let server = f.mcp_server();
    let error = server
        .plan_task_strategy(Parameters(TaskStrategyRequest {
            task_id: planned.clone(),
        }))
        .await
        .err()
        .expect("plan_task_strategy is refused");
    assert_eq!(error.0.to_string(), UNPAIRED);
    let error = server
        .plan_tasks_strategy(Parameters(PlanSelectionRequest {
            column: None,
            repository_id: None,
            task_ids: vec![planned.clone()],
        }))
        .await
        .err()
        .expect("plan_tasks_strategy is refused");
    assert_eq!(error.0.to_string(), UNPAIRED);

    assert_eq!(f.lease_state(&planned).await, unclaimed());
    f.assert_nothing_written().await;
}

// ---------------------------------------------------------------------------
// The model rule, where it does not fire
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_model_no_provider_lists_still_reaches_the_cli() {
    // `tasks.model` is free text, and Claude runs a full model id that no
    // catalogue lists. The model rule refuses only another provider's model,
    // so this card is claimed by the queue and spawned exactly as before.
    let f = Fixture::new().await;
    tasks::update_task(
        f.ctx(),
        &f.task_id,
        TaskPatch {
            model: Patch::Set("claude-opus-4-5".to_string()),
            ..TaskPatch::default()
        },
    )
    .await
    .expect("set a full model id");
    f.cli.replays(&f.task_id, "success", 0);
    let config = f.config();
    let board = f.board(&config);

    let claim = board
        .claim(ClaimTarget::Next {
            capacity: FreeCapacity {
                total: 1,
                per_repository: [(f.repository_id.clone(), 1)].into(),
            },
            repositories: vec![f.repository_id.clone()],
            wait: Duration::ZERO,
        })
        .await
        .expect("claim")
        .expect("the queue offers the task");
    assert_eq!(claim.lease.task_id, f.task_id);
    tokio::time::timeout(
        TEST_TIMEOUT,
        run_task(
            board.as_ref(),
            f.harness.machine(),
            &f.paths,
            &config,
            claim,
            RunRequest::default(),
        ),
    )
    .await
    .expect("a run must finish inside the test timeout")
    .expect("the run");

    let argv = f.cli.argv(&f.task_id, 1);
    let model = argv
        .iter()
        .position(|arg| arg == "--model")
        .map(|at| argv[at + 1].as_str());
    assert_eq!(model, Some("claude-opus-4-5"), "{argv:?}");
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// A task no claim has ever touched: no lease, generation 0, no pin.
fn unclaimed() -> LeaseState {
    LeaseState {
        lease: None,
        generation: 0,
        pinned_runner_id: None,
    }
}

struct Fixture {
    harness: TestContext,
    paths: AppPaths,
    repository_id: String,
    repository_name: String,
    task_id: String,
    cli: FakeCli,
    /// Held for their `Drop`; the paths above point inside them.
    _data: TempDir,
    _repository: TempRepo,
}

impl Fixture {
    async fn new() -> Self {
        let harness = TestContext::new().await;
        let data = tempfile::Builder::new()
            .prefix("rimaia-data-")
            .tempdir()
            .expect("temp dir for the app data directory");
        let paths = AppPaths::new(data.path());
        paths.create_all().expect("the app data directories");

        let repository = TempRepo::init();
        let registered = repo::register(
            &harness.context,
            harness.machine(),
            &paths.worktrees_dir(),
            NewRepository {
                path: repository.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register a test repository");
        repo::set_allow_unattended_runs(&harness.context, harness.machine(), &registered.id, true)
            .await
            .expect("ADR-0012's per-repository opt-in");

        let task_id = tasks::create_task(
            &harness.context,
            NewTask {
                repository_id: registered.id.clone(),
                title: "Alpha".to_string(),
                plan: Some("1. Implement Alpha\n2. Test it".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a ready task")
        .id;

        Self {
            harness,
            paths,
            repository_id: registered.id,
            repository_name: registered.name,
            task_id,
            cli: FakeCli::new(),
            _data: data,
            _repository: repository,
        }
    }

    fn ctx(&self) -> &ServiceContext {
        &self.harness.context
    }

    fn config(&self) -> RunnerConfig {
        RunnerConfig {
            program: self.cli.program(),
            ..RunnerConfig::default()
        }
    }

    /// The solo runner's port: what every solo door claims through.
    fn board(&self, config: &RunnerConfig) -> Arc<dyn BoardPort> {
        self.harness.board(&self.paths, config)
    }

    /// The solo user asking `runner_id` to start.
    fn starter<'a>(&'a self, runner_id: &'a str, presence: OwnerPresence) -> Starter<'a> {
        Starter {
            ctx: self.ctx(),
            runner_id,
            presence,
        }
    }

    /// Run now (`continue_session: false`) or Retry now, through the starter
    /// both buttons call.
    async fn start_by_hand(
        &self,
        starter: Starter<'_>,
        continue_session: bool,
    ) -> rimaia_core::Result<()> {
        let config = self.config();
        claim_manual_start(
            starter,
            self.board(&config).as_ref(),
            self.harness.machine(),
            &self.paths,
            &config,
            &InFlight::new(),
            ManualStart {
                task_id: self.task_id.clone(),
                continue_session,
            },
        )
        .await
        .map(drop)
    }

    /// Plan now, through the function the command and both MCP tools call.
    async fn plan_now(&self, starter: Starter<'_>, task_id: &str) -> rimaia_core::Result<()> {
        let config = self.config();
        claim_for_planning(
            starter,
            self.board(&config).as_ref(),
            self.harness.machine(),
            &InFlight::new(),
            task_id,
            SlotOwner::Manual,
        )
        .await
        .map(drop)
    }

    /// The operator MCP server this machine would serve, over the solo board.
    fn mcp_server(&self) -> RimaiaServer {
        let config = self.config();
        RimaiaServer::new(
            self.ctx().clone(),
            Arc::new(ClaudeProvider),
            Some(LocalTools {
                machine: self.harness.machine().clone(),
                doctor: testing::doctor::environment(),
                planner: PlannerAccess {
                    paths: self.paths.clone(),
                    board: self.board(&config),
                    runner: config,
                    in_flight: InFlight::new(),
                    runner_id: self.harness.solo.runner_id.clone(),
                },
            }),
        )
    }

    /// A second member of the solo team, and a runner of theirs.
    async fn teammates_runner(&self) -> String {
        let mut conn = self.ctx().pool.acquire().await.expect("a connection");
        let member = insert_member(
            &mut conn,
            &self.harness.clock,
            &self.harness.solo.team_id,
            "a-teammate",
        )
        .await;
        insert_runner(&mut conn, &self.harness.clock, &member, "Their laptop").await
    }

    /// A user in a team of their own, which the solo user is not in, and a
    /// runner of theirs.
    async fn outsiders_runner(&self) -> String {
        let mut conn = self.ctx().pool.acquire().await.expect("a connection");
        let outsider = create_personal_team(&mut conn, &self.harness.clock, "an-outsider")
            .await
            .expect("another team");
        insert_runner(
            &mut conn,
            &self.harness.clock,
            &outsider.user_id,
            "Their laptop",
        )
        .await
    }

    async fn unpair_solo_runner(&self) {
        let mut conn = self.ctx().pool.acquire().await.expect("a connection");
        unpair_runner(&mut conn, &self.harness.clock, &self.harness.solo.runner_id).await;
    }

    /// A second ready task, in planned mode, for Plan now.
    async fn planned_task(&self) -> String {
        let task_id = tasks::create_task(
            self.ctx(),
            NewTask {
                repository_id: self.repository_id.clone(),
                title: "Planned".to_string(),
                plan: Some("1. Plan it".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a task")
        .id;
        tasks::update_task(
            self.ctx(),
            &task_id,
            TaskPatch {
                strategy_mode: Some(rimaia_core::db::StrategyMode::Planned),
                ..TaskPatch::default()
            },
        )
        .await
        .expect("planned mode");
        task_id
    }

    async fn detail_of(&self, task_id: &str) -> TaskDetail {
        tasks::get_task(self.ctx(), task_id)
            .await
            .expect("read the task")
    }

    async fn lease_state(&self, task_id: &str) -> LeaseState {
        lease::state_of(self.ctx(), task_id)
            .await
            .expect("read the lease state")
    }

    /// The fixture's task exactly as it was made: no claim, no lease, no run,
    /// no record on this runner and nothing spawned.
    async fn assert_nothing_written(&self) {
        let detail = self.detail_of(&self.task_id).await;
        assert_eq!(detail.task.run_state, RunState::Idle, "no claim");
        assert_eq!(detail.task.column, BoardColumn::Ready);
        assert_eq!(detail.last_run, None, "no runs row");
        assert_eq!(self.lease_state(&self.task_id).await, unclaimed());
        assert_eq!(
            leases::held(self.harness.machine())
                .await
                .expect("read this runner's records"),
            Vec::new(),
            "nothing recorded on this runner",
        );
        assert_eq!(self.cli.started(), Vec::<String>::new(), "nothing spawned");
    }
}
