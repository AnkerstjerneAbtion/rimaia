//! A team two people share, for the tests of who may run whose content
//! (ADR-0032, task 045).
//!
//! # Server-shaped, like `teams::TwoTeams`
//!
//! There is no `solo_identity`. Each person is made by
//! [`identity::create_personal_team`], the service a sign-up uses, so each
//! has a personal team of their own beside the shared one, as a real member
//! does. The shared team is made by [`create_shared_team`], through the helper
//! task 051's team creation uses, and Bob joins by [`add_member`], through an
//! invitation Alice makes and he accepts.
//!
//! Alice owns the shared team and Bob is a member. Each has a runner. The
//! repository is registered in the shared team, its team ceiling allowed by
//! Alice, and consented to on the one machine both runners share here.

use std::sync::Arc;

use sqlx::{SqliteConnection, SqlitePool};
use tempfile::TempDir;

use crate::board::{BoardPort, InProcessBoard, LeaseTerm};
use crate::clock::Clock;
use crate::context::{ServiceContext, TeamScope};
use crate::db::{BoardColumn, MutationSource, Repository};
use crate::events::{LocalEvents, RunnerId, TaskId, TeamId, UserId};
use crate::identity::{create_personal_team, Role};
use crate::machine::MachineContext;
use crate::paths::AppPaths;
use crate::repo::{self, NewRepository};
use crate::runner::provider::ProviderProfile;
use crate::runner::RunnerConfig;
use crate::tasks::{self, NewTask};
use crate::teams::{invitations, members};
use crate::testing::machine::MemoryMachine;
use crate::testing::{test_epoch, test_pool, TempRepo, TestClock};

/// A team nobody's personal team, owned by `owner`, written by the helper
/// task 051's `teams::create_team` writes through, on the caller's
/// transaction.
pub async fn create_shared_team(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    name: &str,
    owner: &str,
) -> TeamId {
    crate::identity::insert_team(conn, clock, name, None, owner)
        .await
        .expect("a shared team must insert")
}

/// Adds `user_id` to `team_id` with `role`, the way a person joins: one of
/// the team's owners makes an invitation, and `user_id` accepts it, through
/// the transaction halves of `create_invitation` and `accept_invitation`, on
/// the caller's connection.
pub async fn add_member(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    team_id: &str,
    user_id: &str,
    role: Role,
) {
    let owner: String = sqlx::query_scalar(
        "SELECT user_id FROM team_memberships WHERE team_id = ?1 AND role = 'owner'
          ORDER BY created_at, user_id LIMIT 1",
    )
    .bind(team_id)
    .fetch_one(&mut *conn)
    .await
    .expect("the team has an owner to invite with");
    let (_, token) = invitations::create_invitation_in(conn, clock, team_id, role, &owner)
        .await
        .expect("an owner's invitation");
    invitations::accept_invitation_in(conn, clock, token.expose(), user_id)
        .await
        .expect("a membership of an existing team must be accepted");
}

/// Takes `user_id` out of `team_id`, withdrawing them from its work, through
/// the transaction half of task 051's removal, on the caller's connection.
pub async fn remove_member(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    team_id: &str,
    user_id: &str,
) {
    members::remove_member_in(conn, clock, team_id, user_id)
        .await
        .expect("removing a membership must succeed");
}

/// One person on the shared board.
pub struct Member {
    pub user_id: UserId,
    pub login: String,
    pub personal_team: TeamId,
    pub runner_id: RunnerId,
    /// Scoped to the shared team, acting as this person.
    pub ctx: ServiceContext,
    /// Scoped to this person's personal team, acting as them.
    pub personal: ServiceContext,
}

/// Two people, one shared team, a repository in it.
pub struct SharedTeam {
    pub clock: TestClock,
    pub team_id: TeamId,
    /// The shared team's owner.
    pub alice: Member,
    /// A member, not an owner.
    pub bob: Member,
    pub repository: Repository,
    pub machine: MachineContext,
    pub paths: AppPaths,
    _source: TempRepo,
    _data: TempDir,
}

impl SharedTeam {
    /// Over the in-memory test pool, for a service test.
    pub async fn new() -> Self {
        Self::over(test_pool().await, TestClock::new(test_epoch())).await
    }

    /// Over `pool`, already migrated and holding no team: the contract
    /// harness passes a file-backed one, so two runners race for real.
    pub async fn over(pool: SqlitePool, clock: TestClock) -> Self {
        let (alice_personal, bob_personal, team_id, alice_runner, bob_runner) = {
            let mut tx = pool.begin().await.expect("a transaction");
            let alice = create_personal_team(&mut tx, &clock, "alice")
                .await
                .expect("alice");
            let bob = create_personal_team(&mut tx, &clock, "bob")
                .await
                .expect("bob");
            let team_id = create_shared_team(&mut tx, &clock, "Acme", &alice.user_id).await;
            add_member(&mut tx, &clock, &team_id, &bob.user_id, Role::Member).await;
            let alice_runner = crate::testing::db::insert_runner(
                &mut tx,
                &clock,
                &alice.user_id,
                "Alice's laptop",
            )
            .await;
            let bob_runner =
                crate::testing::db::insert_runner(&mut tx, &clock, &bob.user_id, "Mac mini").await;
            tx.commit().await.expect("commit the shared team");
            (alice, bob, team_id, alice_runner, bob_runner)
        };

        let base = ServiceContext::new(
            pool,
            Arc::new(clock.clone()),
            MutationSource::Ui,
            TeamScope::one(team_id.clone()),
            alice_personal.user_id.clone(),
        );
        let member = |user_id: &str, login: &str, personal: &str, runner: &str| Member {
            user_id: user_id.to_string(),
            login: login.to_string(),
            personal_team: personal.to_string(),
            runner_id: runner.to_string(),
            ctx: ServiceContext {
                actor: user_id.to_string(),
                ..base.clone()
            },
            personal: ServiceContext {
                actor: user_id.to_string(),
                scope: TeamScope::one(personal.to_string()),
                ..base.clone()
            },
        };
        let alice = member(
            &alice_personal.user_id,
            "alice",
            &alice_personal.team_id,
            &alice_runner,
        );
        let bob = member(
            &bob_personal.user_id,
            "bob",
            &bob_personal.team_id,
            &bob_runner,
        );

        let data = tempfile::Builder::new()
            .prefix("rimaia-shared-team-")
            .tempdir()
            .expect("a data directory");
        let paths = AppPaths::new(data.path());
        paths.create_all().expect("the app directories");
        let machine = MachineContext {
            store: Arc::new(MemoryMachine::new()),
            clock: Arc::new(clock.clone()),
            local: LocalEvents::new(),
        };
        // The base instructions a new team is seeded with are Alice's first
        // revision of them (ADR-0032 point 3). Bob has read and accepted
        // them, so a test starts from a shared team where nothing is owed and
        // a later edit is the first thing his runner has to consent to.
        crate::consent::accept(
            &bob.ctx,
            &team_id,
            None,
            crate::consent::pieces::ContentKind::BaseInstructions,
            "1",
        )
        .await
        .expect("bob accepts the seeded base instructions");
        let source = TempRepo::init();
        let repository = repo::register(
            &alice.ctx,
            &machine,
            &paths.worktrees_dir(),
            NewRepository {
                path: source.path().to_string_lossy().into_owned(),
                name: Some("widgets".to_string()),
                worktree_root: None,
            },
        )
        .await
        .expect("register the shared repository");
        repo::set_repository_unattended_ceiling(&alice.ctx, &repository.id, true)
            .await
            .expect("the owner allows unattended runs");
        repo::set_allow_unattended_runs(&alice.ctx, &machine, &repository.id, true)
            .await
            .expect("this machine consents");

        Self {
            clock,
            team_id,
            alice,
            bob,
            repository,
            machine,
            paths,
            _source: source,
            _data: data,
        }
    }

    /// The board port serving `member`'s runner, over their own context.
    pub fn board(&self, member: &Member) -> Arc<dyn BoardPort> {
        Arc::new(InProcessBoard::new(
            member.ctx.clone(),
            self.paths.clone(),
            ProviderProfile::of(RunnerConfig::default().provider.as_ref()),
            member.runner_id.clone(),
            LeaseTerm::Never,
        ))
    }

    /// A `ready` task `by` writes, with a plan, assigned to `assignee`.
    pub async fn task(&self, by: &Member, title: &str, assignee: Option<&Member>) -> TaskId {
        let task = tasks::create_task(
            &by.ctx,
            NewTask {
                repository_id: self.repository.id.clone(),
                title: title.to_string(),
                plan: Some(format!("1. {title}")),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a task");
        tasks::assign_task(
            &by.ctx,
            &task.id,
            assignee.map(|member| member.user_id.as_str()),
        )
        .await
        .expect("assign the task");
        task.id
    }
}
