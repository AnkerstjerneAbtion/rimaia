//! Who owns what a service writes (ADR-0029 point 1, ADR-0034 point 3, task
//! 038).
//!
//! Task 038 filters nothing; task 039 does. What these pin is where the team
//! and the runner of a written row come from: the row's parent, read in the
//! same transaction, and the adapter the run was started through, never the
//! context's scope, which may name several teams.

use std::sync::Arc;

use pretty_assertions::assert_eq;
use rimaia_core::board::{BoardPort, ClaimTarget, InProcessBoard, LeaseTerm, StartRun};
use rimaia_core::db::{new_id, BoardColumn, RunKind};
use rimaia_core::events::TeamId;
use rimaia_core::identity::{create_personal_team, PersonalTeam};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::{RunTrigger, RunnerConfig};
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::db::insert_runner;
use rimaia_core::testing::{TempRepo, TestContext};
use rimaia_core::{AppPaths, Change, ChangeEvent, ServiceContext, TeamScope};
use tempfile::TempDir;

#[tokio::test]
async fn a_change_event_names_the_team_of_the_row_it_announces() {
    let mut h = TestContext::new().await;
    let second = second_team(&h).await;
    let repository = TempRepo::init();
    let worktrees = scratch_dir();

    // Registered through a context that can reach the second team alone: a
    // repository has no parent to take a team from, so it takes the scope's
    // one team.
    let registered = repo::register(
        &h.context.with_scope(TeamScope::one(second.team_id.clone())),
        h.machine(),
        worktrees.path(),
        NewRepository {
            path: repository.path().to_string_lossy().into_owned(),
            name: None,
            worktree_root: None,
        },
    )
    .await
    .expect("register a repository in the second team");
    // A task, through a context that can reach both teams: its team is its
    // repository's, read in the write's own transaction, never the scope's.
    let both = TeamScope::of([h.solo.team_id.clone(), second.team_id.clone()]).expect("two teams");
    let task = tasks::create_task(
        &h.context.with_scope(both),
        NewTask {
            repository_id: registered.id.clone(),
            title: "In the second team".to_string(),
            plan: None,
            extra_instructions: None,
            column: None,
            links: vec![],
        },
    )
    .await
    .expect("create a task in the second team's repository");

    assert_eq!(
        drain(&mut h),
        vec![
            ChangeEvent::repositories(second.team_id.clone(), [registered.id.clone()]),
            // This machine's checkout, which belongs to no team: it rides the
            // board's channel under the machine's `event_team`, the solo team,
            // until task 048 moves it to `LocalEvents` (task 066).
            ChangeEvent::repositories(h.solo.team_id.clone(), [registered.id]),
            ChangeEvent::tasks(second.team_id.clone(), [task.id]),
        ]
    );
}

#[tokio::test]
async fn a_task_takes_its_repositorys_team() {
    let h = TestContext::new().await;
    let second = second_team(&h).await;
    let fixture = Registered::new(&h.context, h.machine()).await;

    let task = fixture.task(&h.context, "Owned").await;

    assert_eq!(team_of(&h, "tasks", &task).await, h.solo.team_id);

    // The store's own backstop, for a writer that is not the service: a task
    // in another team than its repository is refused by the composite key.
    let refused = sqlx::query(
        "INSERT INTO tasks (id, team_id, repository_id, title, board_column, position, run_state,
                            created_at, updated_at)
         VALUES (?1, ?2, ?3, 'Smuggled', 'not_ready', 9.0, 'idle', '2026-08-20T02:00:00+00:00',
                 '2026-08-20T02:00:00+00:00')",
    )
    .bind(new_id())
    .bind(&second.team_id)
    .bind(&fixture.repository_id)
    .execute(&h.context.pool)
    .await
    .expect_err("a task in another team than its repository");
    assert!(
        refused
            .to_string()
            .contains("FOREIGN KEY constraint failed"),
        "{refused}"
    );
}

#[tokio::test]
async fn a_run_records_the_runner_that_started_it() {
    let h = TestContext::new().await;
    let data = scratch_dir();
    let paths = AppPaths::new(data.path());
    let config = RunnerConfig::default();
    let fixture = Registered::new(&h.context, h.machine()).await;
    let second_runner = {
        let mut conn = h.context.pool.acquire().await.expect("a connection");
        insert_runner(&mut conn, &h.clock, &h.solo.user_id, "Another computer").await
    };
    let solo_board = h.board(&paths, &config);
    let other_board: Arc<dyn BoardPort> = Arc::new(InProcessBoard::new(
        h.context.clone(),
        paths.clone(),
        config.provider.clone(),
        second_runner.clone(),
        LeaseTerm::Never,
    ));

    for (board, runner) in [
        (solo_board, h.solo.runner_id.clone()),
        (other_board, second_runner),
    ] {
        let task_id = fixture.task(&h.context, "Run me").await;
        let claim = board
            .claim(run_now(&task_id))
            .await
            .expect("claim")
            .expect("nothing else holds the task");
        let run_id = new_id();

        board
            .start_run(&claim.lease, starting(&run_id))
            .await
            .expect("start the run");

        let recorded: Option<String> =
            sqlx::query_scalar("SELECT runner_id FROM runs WHERE id = ?1")
                .bind(&run_id)
                .fetch_one(&h.context.pool)
                .await
                .expect("the run row");
        assert_eq!(recorded, Some(runner));
    }
}

#[tokio::test]
async fn a_claims_lease_names_the_tasks_team() {
    let h = TestContext::new().await;
    let data = scratch_dir();
    let paths = AppPaths::new(data.path());
    let second = second_team(&h).await;
    // A board serving a runner that reaches both teams: since task 039 a board
    // scoped to one team cannot see the other's task at all, so this is the
    // board whose lease could name the wrong team if it took it from the scope.
    let both = h.context.with_scope(
        TeamScope::of([h.solo.team_id.clone(), second.team_id.clone()]).expect("two teams"),
    );
    let board = InProcessBoard::new(
        both.clone(),
        paths.clone(),
        RunnerConfig::default().provider,
        h.solo.runner_id.clone(),
        LeaseTerm::Never,
    );
    let solo_fixture = Registered::new(&h.context, h.machine()).await;
    let other_fixture = Registered::new(
        &h.context.with_scope(TeamScope::one(second.team_id.clone())),
        h.machine(),
    )
    .await;

    for (fixture, team) in [
        (solo_fixture, h.solo.team_id.clone()),
        // The board's context reaches both teams; the lease names the task's
        // own team, because it is read off the task's row.
        (other_fixture, second.team_id.clone()),
    ] {
        let task_id = fixture.task(&both, "Claim me").await;

        let claim = board
            .claim(run_now(&task_id))
            .await
            .expect("claim")
            .expect("nothing else holds the task");

        assert_eq!(claim.lease.team_id, team);
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A second personal team, through the team-creation service a sign-up uses.
async fn second_team(h: &TestContext) -> PersonalTeam {
    let mut tx = h.context.pool.begin().await.expect("a transaction");
    let team = create_personal_team(&mut tx, &h.clock, "ada")
        .await
        .expect("a second team");
    tx.commit().await.expect("commit the team");
    team
}

/// A registered repository, through the service, and ready tasks in it. The
/// directories are held for their `Drop`.
struct Registered {
    _repository: TempRepo,
    _worktrees: TempDir,
    repository_id: String,
}

impl Registered {
    async fn new(ctx: &ServiceContext, machine: &rimaia_core::machine::MachineContext) -> Self {
        let repository = TempRepo::init();
        let worktrees = scratch_dir();
        let registered = repo::register(
            ctx,
            machine,
            worktrees.path(),
            NewRepository {
                path: repository.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register a repository");
        Self {
            _repository: repository,
            _worktrees: worktrees,
            repository_id: registered.id,
        }
    }

    async fn task(&self, ctx: &ServiceContext, title: &str) -> String {
        tasks::create_task(
            ctx,
            NewTask {
                repository_id: self.repository_id.clone(),
                title: title.to_string(),
                plan: Some("1. Do the work".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a task")
        .id
    }
}

fn run_now(task_id: &str) -> ClaimTarget {
    ClaimTarget::Run {
        task_id: task_id.to_string(),
        trigger: RunTrigger::Manual,
        continue_session: false,
    }
}

fn starting(run_id: &str) -> StartRun {
    StartRun {
        run_id: run_id.to_string(),
        kind: RunKind::Implementation,
        session_id: "session-1".to_string(),
        prompt: "do the work".to_string(),
        base_ref: Some("main".to_string()),
        base_sha: None,
    }
}

async fn team_of(h: &TestContext, table: &str, id: &str) -> TeamId {
    sqlx::query_scalar(&format!("SELECT team_id FROM {table} WHERE id = ?1"))
        .bind(id)
        .fetch_one(&h.context.pool)
        .await
        .expect("the row's team")
}

/// Every event published so far, without the `Settings` noise a setup may
/// make.
fn drain(h: &mut TestContext) -> Vec<ChangeEvent> {
    let mut events = Vec::new();
    while let Ok(event) = h.changes.try_recv() {
        if event.change != Change::Settings {
            events.push(event);
        }
    }
    events
}

fn scratch_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("rimaia-team-ownership-")
        .tempdir()
        .expect("a scratch directory")
}
