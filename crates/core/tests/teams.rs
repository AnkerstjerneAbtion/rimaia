//! Teams, invitations and roles, through the store (task 051, ADR-0029
//! points 3, 4 and 6, ADR-0037 point 6).
//!
//! Every board here is server-shaped, with no `solo_identity`, and is built
//! the way the product builds one: people sign up through
//! `create_personal_team`, shared teams come from `testing::shared` (045's
//! fixture, whose member helper now goes through an invitation) or
//! `create_team`, and two-team boards from 039's `TwoTeams`. Nothing writes
//! `teams` or `team_memberships` by hand. The clock is a `TestClock`, and
//! nothing sleeps.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::Duration;
use pretty_assertions::assert_eq;
use rimaia_core::api::tail::{TailRelay, SERVER_TAIL_RELAY_CAPACITY};
use rimaia_core::api::TeamGrant;
use rimaia_core::api::{self, Authenticate, BoardHost, Caller, Credential, Door, StreamControl};
use rimaia_core::board::{BoardPort, ClaimTarget, InProcessBoard, LeaseTerm};
use rimaia_core::consent::eligibility::RunnerEligibility;
use rimaia_core::consent::{self, pieces::ContentKind};
use rimaia_core::db::settings::DEFAULT_BASE_INSTRUCTIONS;
use rimaia_core::db::BoardColumn;
use rimaia_core::events::{Change, ChangeEvent};
use rimaia_core::identity::authenticate::SessionsAndTokens;
use rimaia_core::identity::rate_limit::RateLimiter;
use rimaia_core::identity::secret;
use rimaia_core::identity::{create_personal_team, sessions, tokens, Role};
use rimaia_core::mcp::{self, McpHandle, RunHandles};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::provider::ProviderProfile;
use rimaia_core::runner::{RunTrigger, RunnerConfig};
use rimaia_core::tasks::{self, NewTask, Patch, TaskPatch};
use rimaia_core::teams::copy::{copy_task_to_team, list_copy_targets};
use rimaia_core::teams::delete::{
    delete_account, delete_team, PERSONAL_TEAM_GOES_WITH_ITS_ACCOUNT, TYPE_THE_TEAM_NAME,
};
use rimaia_core::teams::invitations::{
    self, accept_invitation, create_invitation, list_invitations, preview_invitation,
    revoke_invitation, InvitationStatus, INVALID_LINK,
};
use rimaia_core::teams::members::{leave_team, list_team_members, remove_member, set_member_role};
use rimaia_core::teams::{self, PERSONAL_TEAM_KEEPS_ITS_PERSON, SOLO_HAS_ONE_TEAM};
use rimaia_core::testing::api::FixedCaller;
use rimaia_core::testing::shared::{Member, SharedTeam};
use rimaia_core::testing::teams::{fill_team, TwoTeams};
use rimaia_core::testing::{TempRepo, TestContext};
use rimaia_core::{Audience, Clock, Error, ErrorCode, ServiceContext, TeamScope};
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::broadcast::Receiver;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// Someone new signing up: their user and personal team, and a context
/// acting as them, scoped to their personal team.
async fn sign_up(team: &SharedTeam, login: &str) -> ServiceContext {
    let mut conn = team.alice.ctx.pool.acquire().await.expect("a connection");
    let person = create_personal_team(&mut conn, &team.clock, login)
        .await
        .expect("sign up");
    ServiceContext {
        actor: person.user_id,
        scope: TeamScope::one(person.team_id),
        ..team.alice.ctx.clone()
    }
}

/// `member` acting in `team_id`, as a request narrowed to it would.
fn acting_in(member: &Member, team_id: &str) -> ServiceContext {
    ServiceContext {
        scope: TeamScope::one(team_id.to_string()),
        ..member.ctx.clone()
    }
}

/// Every row of the tables a team's commands touch, as text: equal before
/// and after a call exactly when the call wrote nothing.
async fn rows(pool: &SqlitePool) -> String {
    let tables = [
        "teams",
        "team_memberships",
        "invitations",
        "repositories",
        "tasks",
        "team_settings",
        "runner_pool_teams",
        "runner_leases",
        "trusted_authors",
        "api_tokens",
        "api_token_teams",
        "users",
    ];
    let mut dump = String::new();
    for table in tables {
        dump.push_str(&table_text(pool, table).await);
    }
    dump
}

/// One table, every column of every row, in a stable order.
async fn table_text(pool: &SqlitePool, table: &str) -> String {
    let columns: Vec<String> =
        sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .fetch_all(pool)
            .await
            .expect("read a table's columns");
    let row = columns
        .iter()
        .map(|column| format!("quote(\"{column}\")"))
        .collect::<Vec<_>>()
        .join(" || '|' || ");
    let rows: Vec<String> =
        sqlx::query_scalar(&format!("SELECT {row} FROM \"{table}\" ORDER BY 1"))
            .fetch_all(pool)
            .await
            .expect("dump a table");
    format!("{table}:\n{}\n", rows.join("\n"))
}

/// Every table on the board, every row.
async fn everything(pool: &SqlitePool) -> String {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
            AND name <> '_sqlx_migrations' ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .expect("list the tables");
    let mut dump = String::new();
    for table in tables {
        dump.push_str(&table_text(pool, &table).await);
    }
    dump
}

fn drain(changes: &mut Receiver<ChangeEvent>) -> Vec<ChangeEvent> {
    let mut events = Vec::new();
    while let Ok(event) = changes.try_recv() {
        events.push(event);
    }
    events
}

fn refused(answer: rimaia_core::Result<impl std::fmt::Debug>) -> Error {
    answer.expect_err("refused")
}

fn assert_invalid(error: &Error, sentence: &str) {
    assert_eq!(error.code(), ErrorCode::Invalid, "{error}");
    assert_eq!(error.to_string(), sentence);
}

fn last_owner(team_name: &str) -> String {
    format!("{team_name} needs at least one owner: make someone else an owner first")
}

/// A board host over the shared team's base context: what a server builds.
fn host(team: &SharedTeam) -> BoardHost {
    BoardHost {
        context: team.alice.ctx.clone(),
        provider: ProviderProfile::of(RunnerConfig::default().provider.as_ref()),
        paths: team.paths.clone(),
        rate_limits: Arc::new(RateLimiter::new(Arc::new(team.clock.clone()))),
        tails: TailRelay::new(SERVER_TAIL_RELAY_CAPACITY),
        streams: StreamControl::new(),
    }
}

/// `member`'s caller through 046's `FixedCaller`, holding `role` in the shared
/// team: the grant is what the request began with, which the role check
/// never trusts.
async fn caller(team: &SharedTeam, member: &Member, role: Role) -> Caller {
    let token = format!("rmd_{}", member.login);
    let auth = FixedCaller::new().with(
        token.clone(),
        Caller {
            user_id: member.user_id.clone(),
            teams: vec![TeamGrant {
                team_id: team.team_id.clone(),
                role,
            }],
            door: Door::Desktop {
                token_id: format!("{}-desktop", member.login),
            },
        },
    );
    auth.authenticate(Credential::Bearer(&token))
        .await
        .expect("the fixed caller answers its token")
}

/// A board port for `member`'s runner whose leases expire `minutes` after a
/// claim, unlike `SharedTeam::board`'s, which never do.
fn expiring_board(team: &SharedTeam, ctx: &ServiceContext, runner_id: &str) -> Arc<dyn BoardPort> {
    Arc::new(InProcessBoard::new(
        ctx.clone(),
        team.paths.clone(),
        ProviderProfile::of(RunnerConfig::default().provider.as_ref()),
        runner_id.to_string(),
        LeaseTerm::Renewable(StdDuration::from_secs(10 * 60)),
    ))
}

fn run_now(task: &str) -> ClaimTarget {
    ClaimTarget::Run {
        task_id: task.to_string(),
        trigger: RunTrigger::Queued,
        continue_session: false,
        ceiling: Default::default(),
    }
}

/// `ctx`'s actor trusts Alice's changes in `team_id`, so their runner takes
/// work under the base instructions she seeded.
async fn trust_alice(team: &SharedTeam, ctx: &ServiceContext, team_id: &str) {
    consent::set_trust(ctx, team_id, &team.alice.user_id, true)
        .await
        .expect("trust alice");
}

async fn claim(board: &Arc<dyn BoardPort>, task: &str) {
    board
        .claim(run_now(task))
        .await
        .unwrap_or_else(|error| panic!("the runner may take it: {error}"))
        .expect("nobody else holds it");
}

/// A repository in `ctx`'s one team, registered from a clone of its own.
async fn register(team: &SharedTeam, ctx: &ServiceContext, name: &str) -> String {
    let source = TempRepo::init();
    let repository = repo::register(
        ctx,
        &team.machine,
        &team.paths.worktrees_dir(),
        NewRepository {
            path: source.path().to_string_lossy().into_owned(),
            name: Some(name.to_string()),
            worktree_root: None,
        },
    )
    .await
    .expect("register a repository");
    drop(source);
    repository.id
}

async fn task_in(ctx: &ServiceContext, repository_id: &str, title: &str) -> String {
    tasks::create_task(
        ctx,
        NewTask {
            repository_id: repository_id.to_string(),
            title: title.to_string(),
            plan: Some(format!("1. {title}")),
            extra_instructions: None,
            column: Some(BoardColumn::Ready),
            links: vec![],
        },
    )
    .await
    .expect("create a task")
    .id
}

async fn scalar_text(pool: &SqlitePool, sql: &str, bind: &str) -> Option<String> {
    sqlx::query_scalar(sql)
        .bind(bind)
        .fetch_one(pool)
        .await
        .expect("read one value")
}

async fn count(pool: &SqlitePool, sql: &str, bind: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(bind)
        .fetch_one(pool)
        .await
        .expect("count")
}

async fn role_in(pool: &SqlitePool, team_id: &str, user_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT role FROM team_memberships WHERE team_id = ?1 AND user_id = ?2")
        .bind(team_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .expect("read a role")
}

// ---------------------------------------------------------------------------
// Creating a team
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_new_team_has_its_creator_as_its_only_owner() {
    let team = SharedTeam::new().await;
    let pool = &team.alice.ctx.pool;

    let created = teams::create_team(&team.alice.ctx, "  Second team  ")
        .await
        .expect("create a team");

    assert_eq!(created.name, "Second team");
    assert_eq!(created.role, Role::Owner);
    assert!(!created.personal);
    assert_eq!((created.member_count, created.owner_count), (1, 1));
    let members: Vec<(String, String)> =
        sqlx::query_as("SELECT user_id, role FROM team_memberships WHERE team_id = ?1")
            .bind(&created.id)
            .fetch_all(pool)
            .await
            .expect("read the memberships");
    assert_eq!(
        members,
        vec![(team.alice.user_id.clone(), "owner".to_string())]
    );
    let (name, personal): (String, Option<String>) =
        sqlx::query_as("SELECT name, personal_user_id FROM teams WHERE id = ?1")
            .bind(&created.id)
            .fetch_one(pool)
            .await
            .expect("read the team");
    assert_eq!((name.as_str(), personal), ("Second team", None));
    let seeded: (String, i64, String) = sqlx::query_as(
        "SELECT value, revision, updated_by FROM team_settings
          WHERE team_id = ?1 AND key = 'base_instructions'",
    )
    .bind(&created.id)
    .fetch_one(pool)
    .await
    .expect("the seeded base instructions");
    assert_eq!(
        seeded,
        (
            DEFAULT_BASE_INSTRUCTIONS.to_string(),
            1,
            team.alice.user_id.clone()
        )
    );

    for name in ["", "   ", &"x".repeat(101)] {
        assert_invalid(
            &refused(teams::create_team(&team.alice.ctx, name).await),
            "A team name is 1 to 100 characters.",
        );
    }
}

// ---------------------------------------------------------------------------
// Invitations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_invitation_admits_any_signed_in_account_once() {
    let team = SharedTeam::new().await;
    let pool = &team.alice.ctx.pool;
    let carol = sign_up(&team, "carol").await;
    let dave = sign_up(&team, "dave").await;
    let created = create_invitation(&team.alice.ctx, &team.team_id, Role::Member)
        .await
        .expect("an owner invites");
    let before = count(
        pool,
        "SELECT count(*) FROM team_memberships WHERE team_id = ?1",
        &team.team_id,
    )
    .await;

    let joined = accept_invitation(&carol, created.token.expose())
        .await
        .expect("carol joins");
    let second = refused(accept_invitation(&dave, created.token.expose()).await);

    assert_eq!(joined.id, team.team_id);
    assert_eq!(second.code(), ErrorCode::NotFound);
    assert_eq!(second.to_string(), INVALID_LINK);
    let after = count(
        pool,
        "SELECT count(*) FROM team_memberships WHERE team_id = ?1",
        &team.team_id,
    )
    .await;
    assert_eq!(after, before + 1);
    assert_eq!(role_in(pool, &team.team_id, &dave.actor).await, None);
}

#[tokio::test]
async fn the_member_gets_the_role_the_invitation_names() {
    let team = SharedTeam::new().await;

    for (login, role) in [("carol", Role::Owner), ("dave", Role::Member)] {
        let person = sign_up(&team, login).await;
        let created = create_invitation(&team.alice.ctx, &team.team_id, role)
            .await
            .expect("an owner invites");
        let joined = accept_invitation(&person, created.token.expose())
            .await
            .expect("they join");

        assert_eq!(joined.role, role);
        assert_eq!(
            role_in(&team.alice.ctx.pool, &team.team_id, &person.actor).await,
            Some(role.as_str().to_string())
        );
    }
}

#[tokio::test]
async fn an_invitation_expires_seven_days_after_it_was_made() {
    let team = SharedTeam::new().await;
    let carol = sign_up(&team, "carol").await;
    let dave = sign_up(&team, "dave").await;

    let first = create_invitation(&team.alice.ctx, &team.team_id, Role::Member)
        .await
        .expect("an owner invites");
    assert_eq!(
        first.invitation.expires_at,
        first.invitation.created_at + Duration::days(7)
    );
    team.clock.advance(Duration::days(7) - Duration::seconds(1));
    accept_invitation(&carol, first.token.expose())
        .await
        .expect("one second before it expires");

    let second = create_invitation(&team.alice.ctx, &team.team_id, Role::Member)
        .await
        .expect("an owner invites again");
    team.clock.set(second.invitation.expires_at);
    let error = refused(accept_invitation(&dave, second.token.expose()).await);
    assert_eq!(error.code(), ErrorCode::NotFound);
    assert_eq!(error.to_string(), INVALID_LINK);
}

#[tokio::test]
async fn a_revoked_invitation_admits_nobody() {
    let team = SharedTeam::new().await;
    let carol = sign_up(&team, "carol").await;
    let created = create_invitation(&team.alice.ctx, &team.team_id, Role::Member)
        .await
        .expect("an owner invites");

    revoke_invitation(&team.alice.ctx, &created.invitation.id)
        .await
        .expect("an owner revokes");

    let error = refused(accept_invitation(&carol, created.token.expose()).await);
    assert_eq!(error.to_string(), INVALID_LINK);
    assert_eq!(
        role_in(&team.alice.ctx.pool, &team.team_id, &carol.actor).await,
        None
    );
    let listed = list_invitations(&team.alice.ctx, &team.team_id)
        .await
        .expect("an owner lists");
    let revoked = listed
        .iter()
        .find(|invitation| invitation.id == created.invitation.id)
        .expect("the revoked invitation is listed");
    assert_eq!(revoked.status, InvitationStatus::Revoked);
}

#[tokio::test]
async fn every_unusable_invitation_is_answered_the_same_way() {
    let team = SharedTeam::new().await;
    let carol = sign_up(&team, "carol").await;
    let dave = sign_up(&team, "dave").await;
    let invite = || async {
        create_invitation(&team.alice.ctx, &team.team_id, Role::Member)
            .await
            .expect("an owner invites")
    };

    let expired = invite().await;
    let revoked = invite().await;
    revoke_invitation(&team.alice.ctx, &revoked.invitation.id)
        .await
        .expect("revoke");
    let accepted = invite().await;
    accept_invitation(&carol, accepted.token.expose())
        .await
        .expect("carol uses it");
    team.clock.advance(Duration::days(8));
    let unknown = secret::mint_invitation_token();

    let mut answers = Vec::new();
    for token in [&unknown, &expired.token, &revoked.token, &accepted.token] {
        let preview = refused(preview_invitation(&dave, token.expose()).await);
        let accept = refused(accept_invitation(&dave, token.expose()).await);
        answers.push(serde_json::to_string(&preview).expect("an error serializes"));
        answers.push(serde_json::to_string(&accept).expect("an error serializes"));
    }

    let expected = serde_json::to_string(&Error::not_found(INVALID_LINK)).expect("serializes");
    assert_eq!(answers, vec![expected; 8]);
}

#[tokio::test]
async fn only_the_hash_of_an_invitation_token_is_stored() {
    let team = SharedTeam::new().await;
    let created = create_invitation(&team.alice.ctx, &team.team_id, Role::Member)
        .await
        .expect("an owner invites");
    let token = created.token.expose();
    let secret_part = token
        .strip_prefix(secret::INVITATION_PREFIX)
        .expect("the invitation prefix");

    let dump = everything(&team.alice.ctx.pool).await;

    assert!(!dump.contains(token), "the token is stored somewhere");
    assert!(
        !dump.contains(secret_part),
        "its secret part is stored somewhere"
    );
    let stored = scalar_text(
        &team.alice.ctx.pool,
        "SELECT token_hash FROM invitations WHERE id = ?1",
        &created.invitation.id,
    )
    .await;
    assert_eq!(stored, Some(secret::hash(token)));
}

#[tokio::test]
async fn accepting_while_already_a_member_changes_nothing_and_keeps_the_invitation() {
    let team = SharedTeam::new().await;
    let created = create_invitation(&team.alice.ctx, &team.team_id, Role::Owner)
        .await
        .expect("an owner invites");
    let before = rows(&team.alice.ctx.pool).await;

    let error = refused(accept_invitation(&team.bob.ctx, created.token.expose()).await);

    assert_invalid(&error, "you are already a member of Acme");
    assert_eq!(rows(&team.alice.ctx.pool).await, before);
    assert_eq!(
        role_in(&team.alice.ctx.pool, &team.team_id, &team.bob.user_id).await,
        Some("member".to_string())
    );
    let listed = list_invitations(&team.alice.ctx, &team.team_id)
        .await
        .expect("an owner lists");
    let kept = listed
        .iter()
        .find(|invitation| invitation.id == created.invitation.id)
        .expect("the invitation is listed");
    assert_eq!(kept.status, InvitationStatus::Pending);
}

// ---------------------------------------------------------------------------
// Roles
// ---------------------------------------------------------------------------

/// What a case's arguments may name beyond the fixture: a pending invitation.
struct Arranged {
    invitation_id: String,
}

struct OwnerOnly {
    name: &'static str,
    args: fn(&SharedTeam, &Arranged) -> Value,
    action: &'static str,
}

const OWNER_ONLY: &[OwnerOnly] = &[
    OwnerOnly {
        name: "update_repository",
        args: |t, _| json!({ "id": t.repository.id, "patch": { "name": "renamed" } }),
        action: "change a repository",
    },
    OwnerOnly {
        name: "remove_repository",
        args: |t, _| json!({ "id": t.repository.id }),
        action: "remove a repository",
    },
    OwnerOnly {
        name: "set_repository_unattended_ceiling",
        args: |t, _| json!({ "id": t.repository.id, "allowed": false }),
        action: "change whether a repository allows unattended runs",
    },
    OwnerOnly {
        name: "set_repository_review_config",
        args: |t, _| json!({ "repositoryId": t.repository.id, "config": null }),
        action: "change a repository's review settings",
    },
    OwnerOnly {
        name: "set_base_instructions",
        args: |_, _| json!({ "value": "Open a draft PR." }),
        action: "change the team's settings",
    },
    OwnerOnly {
        name: "set_strategy_catalogue",
        args: |_, _| json!({ "value": r#"{"models":[]}"# }),
        action: "change the team's settings",
    },
    OwnerOnly {
        name: "set_strategy_defaults",
        args: |t, _| json!({ "repositoryId": t.repository.id, "value": { "mode": "default" } }),
        action: "change the team's settings",
    },
    OwnerOnly {
        name: "set_strategy_approval",
        args: |_, _| json!({ "value": "manual" }),
        action: "change the team's settings",
    },
    OwnerOnly {
        name: "set_review_settings",
        args: |_, _| json!({ "instructions": "Review it.", "config": null }),
        action: "change the team's settings",
    },
    OwnerOnly {
        name: "set_member_role",
        args: |t, _| json!({ "teamId": t.team_id, "userId": t.bob.user_id, "role": "owner" }),
        action: "change someone's role",
    },
    OwnerOnly {
        name: "remove_member",
        args: |t, _| json!({ "teamId": t.team_id, "userId": t.bob.user_id }),
        action: "remove someone from the team",
    },
    OwnerOnly {
        name: "create_invitation",
        args: |t, _| json!({ "teamId": t.team_id, "role": "member" }),
        action: "invite people",
    },
    OwnerOnly {
        name: "list_invitations",
        args: |t, _| json!({ "teamId": t.team_id }),
        action: "see the team's invitations",
    },
    OwnerOnly {
        name: "revoke_invitation",
        args: |_, a| json!({ "id": a.invitation_id }),
        action: "revoke an invitation",
    },
    OwnerOnly {
        name: "delete_team",
        args: |t, _| json!({ "teamId": t.team_id, "confirmName": "Acme" }),
        action: "delete the team",
    },
];

#[tokio::test]
async fn a_member_cannot_do_what_only_an_owner_can() {
    let mut failures = Vec::new();
    for case in OWNER_ONLY {
        let team = SharedTeam::new().await;
        let arranged = Arranged {
            invitation_id: create_invitation(&team.alice.ctx, &team.team_id, Role::Member)
                .await
                .expect("a pending invitation")
                .invitation
                .id,
        };
        let host = host(&team);
        let mut changes = team.alice.ctx.subscribe();
        let args = (case.args)(&team, &arranged);
        let before = rows(&team.alice.ctx.pool).await;

        let as_member = caller(&team, &team.bob, Role::Member).await;
        let answer = api::dispatch(&host, as_member, case.name, args.clone()).await;

        let expected = format!("only an owner of this team can {}", case.action);
        match &answer {
            Err(error) if error.code() == ErrorCode::Invalid && error.to_string() == expected => {}
            other => failures.push(format!("{}: as a member, {other:?}", case.name)),
        }
        if rows(&team.alice.ctx.pool).await != before {
            failures.push(format!("{}: a member's refused call wrote rows", case.name));
        }
        let events = drain(&mut changes);
        if !events.is_empty() {
            failures.push(format!("{}: a refusal published {events:?}", case.name));
        }

        let as_owner = caller(&team, &team.alice, Role::Owner).await;
        if let Err(error) = api::dispatch(&host, as_owner, case.name, args).await {
            failures.push(format!("{}: as an owner, {error}", case.name));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));

    // `register_repository` is local until task 054, so its check sits in
    // `repo::register`, which every door calls.
    let team = SharedTeam::new().await;
    let mut changes = team.alice.ctx.subscribe();
    let source = TempRepo::init();
    let new = || NewRepository {
        path: source.path().to_string_lossy().into_owned(),
        name: Some("gadgets".to_string()),
        worktree_root: None,
    };
    let before = rows(&team.alice.ctx.pool).await;
    let error = refused(
        repo::register(
            &team.bob.ctx,
            &team.machine,
            &team.paths.worktrees_dir(),
            new(),
        )
        .await,
    );
    assert_invalid(
        &error,
        "only an owner of this team can register a repository",
    );
    assert_eq!(rows(&team.alice.ctx.pool).await, before);
    assert_eq!(drain(&mut changes), Vec::<ChangeEvent>::new());
    repo::register(
        &team.alice.ctx,
        &team.machine,
        &team.paths.worktrees_dir(),
        new(),
    )
    .await
    .expect("an owner registers");

    // Task edits stay open to members, the task's own review settings
    // included.
    let task = team.task(&team.alice, "Alice's", None).await;
    let as_member = caller(&team, &team.bob, Role::Member).await;
    api::dispatch(
        &host(&team),
        as_member,
        "set_task_review",
        json!({ "taskId": task, "config": null }),
    )
    .await
    .expect("a member edits a task's review settings");
}

#[tokio::test]
async fn a_demotion_takes_effect_on_the_next_request() {
    let team = SharedTeam::new().await;
    let host = host(&team);
    set_member_role(
        &team.alice.ctx,
        &team.team_id,
        &team.bob.user_id,
        Role::Owner,
    )
    .await
    .expect("bob becomes an owner");
    // The grant the request began with says owner, and keeps saying it.
    let stale = caller(&team, &team.bob, Role::Owner).await;
    let invite = json!({ "teamId": team.team_id, "role": "member" });
    api::dispatch(&host, stale.clone(), "create_invitation", invite.clone())
        .await
        .expect("an owner invites");

    set_member_role(
        &team.alice.ctx,
        &team.team_id,
        &team.bob.user_id,
        Role::Member,
    )
    .await
    .expect("bob is demoted");

    let error = refused(api::dispatch(&host, stale, "create_invitation", invite).await);
    assert_invalid(&error, "only an owner of this team can invite people");
}

#[tokio::test]
async fn the_last_owner_cannot_leave_be_removed_or_be_demoted() {
    let team = SharedTeam::new().await;
    let pool = &team.alice.ctx.pool;
    let before = rows(pool).await;

    assert_invalid(
        &refused(leave_team(&team.alice.ctx, &team.team_id).await),
        &last_owner("Acme"),
    );
    assert_eq!(rows(pool).await, before);
    assert_invalid(
        &refused(remove_member(&team.alice.ctx, &team.team_id, &team.alice.user_id).await),
        &last_owner("Acme"),
    );
    assert_eq!(rows(pool).await, before);
    assert_invalid(
        &refused(
            set_member_role(
                &team.alice.ctx,
                &team.team_id,
                &team.alice.user_id,
                Role::Member,
            )
            .await,
        ),
        &last_owner("Acme"),
    );
    assert_eq!(rows(pool).await, before);
}

#[tokio::test]
async fn an_owner_can_leave_once_another_owner_exists() {
    let team = SharedTeam::new().await;
    set_member_role(
        &team.alice.ctx,
        &team.team_id,
        &team.bob.user_id,
        Role::Owner,
    )
    .await
    .expect("bob becomes an owner");

    leave_team(&team.alice.ctx, &team.team_id)
        .await
        .expect("alice leaves");

    let members = list_team_members(&team.bob.ctx, &team.team_id)
        .await
        .expect("bob lists the members");
    assert_eq!(
        members
            .iter()
            .map(|member| (member.login.as_str(), member.role))
            .collect::<Vec<_>>(),
        vec![("bob", Role::Owner)]
    );
}

#[tokio::test]
async fn a_personal_team_keeps_the_person_it_belongs_to() {
    let team = SharedTeam::new().await;
    let personal = team.alice.personal_team.clone();
    // Bob is invited into Alice's personal team, as an owner.
    let created = create_invitation(&team.alice.personal, &personal, Role::Owner)
        .await
        .expect("alice invites bob into her personal team");
    accept_invitation(&team.bob.ctx, created.token.expose())
        .await
        .expect("bob joins");
    let bob = acting_in(&team.bob, &personal);
    let before = rows(&team.alice.ctx.pool).await;

    assert_invalid(
        &refused(remove_member(&bob, &personal, &team.alice.user_id).await),
        PERSONAL_TEAM_KEEPS_ITS_PERSON,
    );
    assert_invalid(
        &refused(set_member_role(&bob, &personal, &team.alice.user_id, Role::Member).await),
        PERSONAL_TEAM_KEEPS_ITS_PERSON,
    );
    assert_invalid(
        &refused(leave_team(&team.alice.personal, &personal).await),
        PERSONAL_TEAM_KEEPS_ITS_PERSON,
    );
    for ctx in [&team.alice.personal, &bob] {
        assert_invalid(
            &refused(delete_team(ctx, &personal, "Personal").await),
            PERSONAL_TEAM_GOES_WITH_ITS_ACCOUNT,
        );
    }
    assert_eq!(rows(&team.alice.ctx.pool).await, before);
}

// ---------------------------------------------------------------------------
// What removal withdraws
// ---------------------------------------------------------------------------

#[tokio::test]
async fn removing_a_member_withdraws_their_runners_from_the_teams_work() {
    let team = SharedTeam::new().await;
    let pool = team.alice.ctx.pool.clone();
    let bob = &team.bob;

    // In the shared team: a task assigned to Bob, one pinned to his runner,
    // one his runner holds a lease on, and one that is nobody's business.
    let assigned = team.task(&team.alice, "Assigned", Some(bob)).await;
    let pinned = team.task(&team.alice, "Pinned", None).await;
    let leased = team.task(bob, "Leased", Some(bob)).await;
    let untouched = team.task(&team.alice, "Alice's", Some(&team.alice)).await;
    for task in [&pinned, &untouched] {
        sqlx::query("UPDATE tasks SET pinned_runner_id = ?1 WHERE id = ?2")
            .bind(if *task == pinned {
                &bob.runner_id
            } else {
                &team.alice.runner_id
            })
            .bind(task)
            .execute(&pool)
            .await
            .expect("pin a task");
    }
    consent::set_runner_eligibility(
        &bob.ctx,
        &bob.runner_id,
        RunnerEligibility::AssignedThenPool,
        &[team.team_id.clone(), bob.personal_team.clone()],
    )
    .await
    .expect("bob's runner takes both teams' pools");

    // In Bob's personal team: the same three, which removal leaves alone.
    let repository = register(&team, &bob.personal, "bobs").await;
    let own_assigned = task_in(&bob.personal, &repository, "Own").await;
    tasks::assign_task(&bob.personal, &own_assigned, Some(&bob.user_id))
        .await
        .expect("assign it");
    sqlx::query("UPDATE tasks SET pinned_runner_id = ?1 WHERE id = ?2")
        .bind(&bob.runner_id)
        .bind(&own_assigned)
        .execute(&pool)
        .await
        .expect("pin a personal task");
    let own_leased = task_in(&bob.personal, &repository, "Own leased").await;
    claim(
        &expiring_board(&team, &bob.personal, &bob.runner_id),
        &own_leased,
    )
    .await;
    // The shared team's lease, taken after the personal tasks are written: a
    // plan Bob writes while his runner holds someone's work is marked.
    trust_alice(&team, &bob.ctx, &team.team_id).await;
    claim(&expiring_board(&team, &bob.ctx, &bob.runner_id), &leased).await;
    let personal_before = [
        team_row(&pool, "tasks", &own_assigned).await,
        team_row(&pool, "tasks", &own_leased).await,
        lease_row(&pool, &own_leased).await,
    ];
    team.clock.advance(Duration::minutes(1));
    let mut changes = team.alice.ctx.subscribe();

    remove_member(&team.alice.ctx, &team.team_id, &bob.user_id)
        .await
        .expect("alice removes bob");

    let (assignee, assigned_by): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT assignee_id, assigned_by FROM tasks WHERE id = ?1")
            .bind(&assigned)
            .fetch_one(&pool)
            .await
            .expect("read the assignment");
    assert_eq!((assignee, assigned_by), (None, None));
    assert_eq!(
        scalar_text(
            &pool,
            "SELECT pinned_runner_id FROM tasks WHERE id = ?1",
            &pinned
        )
        .await,
        None
    );
    assert_eq!(
        scalar_text(
            &pool,
            "SELECT pinned_runner_id FROM tasks WHERE id = ?1",
            &untouched
        )
        .await,
        Some(team.alice.runner_id.clone()),
        "another member's pin stays"
    );
    let pool_teams: Vec<String> =
        sqlx::query_scalar("SELECT team_id FROM runner_pool_teams WHERE runner_id = ?1")
            .bind(&bob.runner_id)
            .fetch_all(&pool)
            .await
            .expect("read the pool list");
    assert_eq!(pool_teams, vec![bob.personal_team.clone()]);
    let expires_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT expires_at FROM runner_leases WHERE task_id = ?1")
            .bind(&leased)
            .fetch_one(&pool)
            .await
            .expect("the lease is expired, not deleted");
    assert_eq!(expires_at, Some(team.clock.now()));

    let task_events: Vec<Vec<String>> = drain(&mut changes)
        .into_iter()
        .filter_map(|event| match event.change {
            Change::Tasks(ids) => {
                assert_eq!(event.audience, Audience::Team(team.team_id.clone()));
                let mut ids = ids.to_vec();
                ids.sort();
                Some(ids)
            }
            _ => None,
        })
        .collect();
    let mut expected = vec![assigned, pinned, leased];
    expected.sort();
    assert_eq!(task_events, vec![expected]);

    assert_eq!(
        [
            team_row(&pool, "tasks", &own_assigned).await,
            team_row(&pool, "tasks", &own_leased).await,
            lease_row(&pool, &own_leased).await,
        ],
        personal_before,
        "Bob's work in another team is untouched"
    );
}

async fn team_row(pool: &SqlitePool, table: &str, id: &str) -> String {
    row_text(pool, table, "id", id).await
}

async fn lease_row(pool: &SqlitePool, task_id: &str) -> String {
    row_text(pool, "runner_leases", "task_id", task_id).await
}

async fn row_text(pool: &SqlitePool, table: &str, key: &str, id: &str) -> String {
    let columns: Vec<String> =
        sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .fetch_all(pool)
            .await
            .expect("read a table's columns");
    let row = columns
        .iter()
        .map(|column| format!("quote(\"{column}\")"))
        .collect::<Vec<_>>()
        .join(" || '|' || ");
    sqlx::query_scalar(&format!("SELECT {row} FROM {table} WHERE {key} = ?1"))
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read a row")
}

#[tokio::test]
async fn a_member_invited_back_starts_without_trust_or_restricted_tokens() {
    let team = SharedTeam::new().await;
    let pool = team.alice.ctx.pool.clone();
    let bob = &team.bob;
    consent::set_trust(&bob.ctx, &team.team_id, &team.alice.user_id, true)
        .await
        .expect("bob trusts alice");
    consent::set_trust(&team.alice.ctx, &team.team_id, &bob.user_id, true)
        .await
        .expect("alice trusts bob");
    let both = ServiceContext {
        scope: TeamScope::of([team.team_id.clone(), bob.personal_team.clone()]).expect("two"),
        ..bob.ctx.clone()
    };
    let alone = tokens::create_personal(
        &both,
        "Acme only",
        std::slice::from_ref(&team.team_id),
        None,
    )
    .await
    .expect("a token restricted to the team");
    let shared = tokens::create_personal(
        &both,
        "Acme and mine",
        &[team.team_id.clone(), bob.personal_team.clone()],
        None,
    )
    .await
    .expect("a token restricted to two teams");

    remove_member(&team.alice.ctx, &team.team_id, &bob.user_id)
        .await
        .expect("alice removes bob");
    let created = create_invitation(&team.alice.ctx, &team.team_id, Role::Member)
        .await
        .expect("alice invites bob back");
    accept_invitation(&bob.personal, created.token.expose())
        .await
        .expect("bob joins again");

    let trust = count(
        &pool,
        "SELECT count(*) FROM trusted_authors WHERE team_id = ?1",
        &team.team_id,
    )
    .await;
    assert_eq!(trust, 0, "trust in both directions is gone");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM api_tokens WHERE id = ?1",
            &alone.summary.id
        )
        .await,
        0,
        "the token restricted to the team alone is gone"
    );
    let left: Vec<String> =
        sqlx::query_scalar("SELECT team_id FROM api_token_teams WHERE token_id = ?1")
            .bind(&shared.summary.id)
            .fetch_all(&pool)
            .await
            .expect("read the restriction");
    assert_eq!(left, vec![bob.personal_team.clone()]);
}

// ---------------------------------------------------------------------------
// Deleting a team
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deleting_a_team_needs_its_exact_name() {
    let team = SharedTeam::new().await;
    let before = rows(&team.alice.ctx.pool).await;

    for typed in ["acme", "Acme ", "Acm"] {
        assert_invalid(
            &refused(delete_team(&team.alice.ctx, &team.team_id, typed).await),
            TYPE_THE_TEAM_NAME,
        );
    }
    assert_eq!(rows(&team.alice.ctx.pool).await, before);
}

#[tokio::test]
async fn deleting_a_team_removes_everything_it_owned() {
    let teams = TwoTeams::new().await;
    let owner = &teams.a;
    let shared = teams::create_team(owner, "Doomed")
        .await
        .expect("create a team");
    let in_shared = owner.with_scope(TeamScope::one(shared.id.clone()));
    let board = fill_team(&in_shared, &teams.machine, &teams.clock, &teams.paths).await;
    // Acceptances, task and team-wide, and trust in both directions.
    let guest = ServiceContext {
        actor: board.guest_id.clone(),
        ..in_shared.clone()
    };
    let plan_revision: i64 = sqlx::query_scalar("SELECT plan_revision FROM tasks WHERE id = ?1")
        .bind(&board.ready)
        .fetch_one(&owner.pool)
        .await
        .expect("the plan's revision");
    consent::accept(
        &guest,
        &shared.id,
        Some(&board.ready),
        ContentKind::Plan,
        &plan_revision.to_string(),
    )
    .await
    .expect("the guest accepts a plan");
    let instructions_revision: i64 = sqlx::query_scalar(
        "SELECT revision FROM team_settings WHERE team_id = ?1 AND key = 'base_instructions'",
    )
    .bind(&shared.id)
    .fetch_one(&owner.pool)
    .await
    .expect("the instructions' revision");
    consent::accept(
        &guest,
        &shared.id,
        None,
        ContentKind::BaseInstructions,
        &instructions_revision.to_string(),
    )
    .await
    .expect("the guest accepts the base instructions");
    consent::set_trust(&guest, &shared.id, &board.owner_id, true)
        .await
        .expect("the guest trusts the owner");
    consent::set_trust(&in_shared, &shared.id, &board.guest_id, true)
        .await
        .expect("the owner trusts the guest");
    let others_before = (
        teams.snapshot(&teams.team_a).await,
        teams.snapshot_b().await,
    );
    let mut doomed_ids = board.ids();
    doomed_ids.retain(|id| {
        // The people outlive the team: the owner, their runners and
        // credentials, and the guest with their personal team.
        ![
            &board.owner_id,
            &board.runner_id,
            &board.guest_id,
            &board.guest_personal_team,
        ]
        .contains(&id)
            && !board_credentials(&board).contains(id)
    });
    assert!(doomed_ids.contains(&shared.id));

    let purged = delete_team(&in_shared, &shared.id, "Doomed")
        .await
        .expect("delete the team");

    assert_eq!(purged.repository_ids, vec![board.repository.id.clone()]);
    assert_eq!(purged.task_ids.len(), 5);
    let dump = everything(&owner.pool).await;
    for id in &doomed_ids {
        assert!(!dump.contains(id.as_str()), "a row still names {id}");
    }
    let dangling: Vec<(String, i64, String, i64)> =
        sqlx::query_as("SELECT * FROM pragma_foreign_key_check")
            .fetch_all(&owner.pool)
            .await
            .expect("check the foreign keys");
    assert_eq!(dangling, Vec::new());
    assert_eq!(
        (
            teams.snapshot(&teams.team_a).await,
            teams.snapshot_b().await
        ),
        others_before,
        "the other teams' rows are byte-identical"
    );
}

fn board_credentials(board: &rimaia_core::testing::teams::TeamBoard) -> Vec<String> {
    let credentials = &board.credentials;
    vec![
        credentials.session_id.clone(),
        credentials.desktop_token_id.clone(),
        credentials.personal_token_id.clone(),
        credentials.runner_token_id.clone(),
        credentials.paired_runner_id.clone(),
    ]
}

#[tokio::test]
async fn deleting_a_team_does_not_widen_a_token_restricted_to_it() {
    let team = SharedTeam::new().await;
    let pool = team.alice.ctx.pool.clone();
    let both = ServiceContext {
        scope: TeamScope::of([team.team_id.clone(), team.alice.personal_team.clone()])
            .expect("two"),
        ..team.alice.ctx.clone()
    };
    let alone = tokens::create_personal(
        &both,
        "Acme only",
        std::slice::from_ref(&team.team_id),
        None,
    )
    .await
    .expect("a token restricted to the team");
    let shared = tokens::create_personal(
        &both,
        "Acme and mine",
        &[team.team_id.clone(), team.alice.personal_team.clone()],
        None,
    )
    .await
    .expect("a token restricted to two teams");

    delete_team(&team.alice.ctx, &team.team_id, "Acme")
        .await
        .expect("delete the team");

    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM api_tokens WHERE id = ?1",
            &alone.summary.id
        )
        .await,
        0
    );
    let left: Vec<String> =
        sqlx::query_scalar("SELECT team_id FROM api_token_teams WHERE token_id = ?1")
            .bind(&shared.summary.id)
            .fetch_all(&pool)
            .await
            .expect("read the restriction");
    assert_eq!(left, vec![team.alice.personal_team.clone()]);
}

#[tokio::test]
async fn a_team_with_running_work_cannot_be_deleted() {
    let team = SharedTeam::new().await;
    let task = team.task(&team.bob, "Bob's", Some(&team.bob)).await;
    trust_alice(&team, &team.bob.ctx, &team.team_id).await;
    claim(
        &expiring_board(&team, &team.bob.ctx, &team.bob.runner_id),
        &task,
    )
    .await;
    let before = rows(&team.alice.ctx.pool).await;

    assert_invalid(
        &refused(delete_team(&team.alice.ctx, &team.team_id, "Acme").await),
        "1 of Acme's tasks are running: wait for them or cancel them first",
    );
    assert_eq!(rows(&team.alice.ctx.pool).await, before);

    // Expired, the lease no longer blocks: its task goes with the team.
    team.clock.advance(Duration::minutes(11));
    delete_team(&team.alice.ctx, &team.team_id, "Acme")
        .await
        .expect("an expired lease does not block");
}

// ---------------------------------------------------------------------------
// Deleting an account
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deleting_an_account_deletes_its_personal_team_and_leaves_its_other_teams() {
    let team = SharedTeam::new().await;
    let pool = team.alice.ctx.pool.clone();
    let bob = &team.bob;
    let written = team.task(bob, "Bob's", None).await;
    sessions::create(&pool, &team.clock, &bob.user_id, Some("Firefox"))
        .await
        .expect("a session");
    tokens::create_desktop(&pool, &team.clock, &bob.user_id, "Desktop app")
        .await
        .expect("a desktop token");
    let mut changes = team.alice.ctx.subscribe();

    let deleted = delete_account(&bob.ctx, "bob")
        .await
        .expect("bob leaves Rimaia");

    assert_eq!(deleted.affected_users, vec![bob.user_id.clone()]);
    assert_eq!(
        scalar_text(
            &pool,
            "SELECT created_by FROM tasks WHERE id = ?1",
            &written
        )
        .await,
        None,
        "the task stays, by a former member"
    );
    let task_events: Vec<ChangeEvent> = drain(&mut changes)
        .into_iter()
        .filter(|event| matches!(event.change, Change::Tasks(_)))
        .collect();
    assert_eq!(
        task_events,
        vec![ChangeEvent::tasks(team.team_id.clone(), [written.clone()])]
    );
    for table in [
        "team_memberships",
        "sessions",
        "api_tokens",
        "runners",
        "users",
    ] {
        let column = if table == "users" { "id" } else { "user_id" };
        assert_eq!(
            count(
                &pool,
                &format!("SELECT count(*) FROM {table} WHERE {column} = ?1"),
                &bob.user_id
            )
            .await,
            0,
            "{table}"
        );
    }
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM teams WHERE id = ?1",
            &bob.personal_team
        )
        .await,
        0,
        "the personal team went with the account"
    );
}

#[tokio::test]
async fn an_account_that_is_a_teams_only_owner_cannot_be_deleted() {
    let team = SharedTeam::new().await;
    let before = rows(&team.alice.ctx.pool).await;

    assert_invalid(
        &refused(delete_account(&team.alice.ctx, "alice").await),
        "you are the only owner of Acme: make someone else an owner, or delete the team, first",
    );
    assert_eq!(rows(&team.alice.ctx.pool).await, before);
}

#[tokio::test]
async fn an_account_whose_runner_holds_work_cannot_be_deleted() {
    let team = SharedTeam::new().await;
    let task = team.task(&team.bob, "Bob's", Some(&team.bob)).await;
    trust_alice(&team, &team.bob.ctx, &team.team_id).await;
    claim(
        &expiring_board(&team, &team.bob.ctx, &team.bob.runner_id),
        &task,
    )
    .await;
    let refusal = "your runner Mac mini is still holding work: wait for it to be released";

    assert_invalid(
        &refused(delete_account(&team.bob.ctx, "bob").await),
        refusal,
    );

    team.clock.advance(Duration::minutes(11));
    assert_invalid(
        &refused(delete_account(&team.bob.ctx, "bob").await),
        refusal,
    );
}

#[tokio::test]
async fn an_account_whose_personal_team_has_running_work_cannot_be_deleted() {
    let team = SharedTeam::new().await;
    let pool = team.alice.ctx.pool.clone();
    let personal = team.alice.personal_team.clone();
    // So that Acme does not refuse the deletion first.
    set_member_role(
        &team.alice.ctx,
        &team.team_id,
        &team.bob.user_id,
        Role::Owner,
    )
    .await
    .expect("bob becomes an owner of Acme");
    let created = create_invitation(&team.alice.personal, &personal, Role::Member)
        .await
        .expect("alice invites bob into her personal team");
    accept_invitation(&team.bob.ctx, created.token.expose())
        .await
        .expect("bob joins");
    let repository = register(&team, &team.alice.personal, "alices").await;
    let bob = acting_in(&team.bob, &personal);
    let task = task_in(&bob, &repository, "Bob's, in Alice's team").await;
    tasks::assign_task(&bob, &task, Some(&team.bob.user_id))
        .await
        .expect("assign it to bob");
    trust_alice(&team, &bob, &personal).await;
    claim(&expiring_board(&team, &bob, &team.bob.runner_id), &task).await;
    let lease = lease_row(&pool, &task).await;

    assert_invalid(
        &refused(delete_account(&team.alice.ctx, "alice").await),
        "1 of Personal's tasks are running: wait for them or cancel them first",
    );
    assert_eq!(
        lease_row(&pool, &task).await,
        lease,
        "the lease is unchanged"
    );
}

// ---------------------------------------------------------------------------
// Copying a task
// ---------------------------------------------------------------------------

#[tokio::test]
async fn copying_a_task_to_another_team_brings_the_plan_and_nothing_else() {
    let teams = TwoTeams::new().await;
    let source = &teams.team_a.in_review;
    tasks::update_task(
        &teams.a,
        source,
        TaskPatch {
            extra_instructions: Patch::Set("Skip the migration".to_string()),
            ..TaskPatch::default()
        },
    )
    .await
    .expect("give the source extra instructions");
    let second = teams::create_team(&teams.a, "Second")
        .await
        .expect("a second team");
    let in_second = teams.a.with_scope(TeamScope::one(second.id.clone()));
    let worktrees = tempfile::tempdir().expect("a worktrees directory");
    let checkout = TempRepo::init();
    let target = repo::register(
        &in_second,
        &teams.machine,
        worktrees.path(),
        NewRepository {
            path: checkout.path().to_string_lossy().into_owned(),
            name: Some("target".to_string()),
            worktree_root: None,
        },
    )
    .await
    .expect("a repository in the second team");
    let both = teams.a.with_scope(
        TeamScope::of([teams.team_a.team_id.clone(), second.id.clone()]).expect("two teams"),
    );
    let source_before = teams.row_text("tasks", source).await;
    let snapshot_before = teams.snapshot(&teams.team_a).await;
    let original = tasks::get_task(&teams.a, source)
        .await
        .expect("read the source")
        .task;

    let copy = copy_task_to_team(&both, source, &target.id)
        .await
        .expect("copy the task");

    // Every other field is what creating the same task fresh would give.
    let fresh = tasks::create_task(
        &in_second,
        NewTask {
            repository_id: target.id.clone(),
            title: original.title.clone(),
            plan: original.plan.clone(),
            extra_instructions: original.extra_instructions.clone(),
            column: Some(BoardColumn::NotReady),
            links: vec![],
        },
    )
    .await
    .expect("the same task, created fresh");
    assert_eq!(copy.title, original.title);
    assert_eq!(copy.plan, original.plan);
    assert_eq!(
        copy.extra_instructions.as_deref(),
        Some("Skip the migration")
    );
    assert_eq!(copy.column, BoardColumn::NotReady);
    let author: (Option<String>, Option<String>, i64, String) = sqlx::query_as(
        "SELECT created_by, plan_updated_by, plan_revision, team_id FROM tasks WHERE id = ?1",
    )
    .bind(&copy.id)
    .fetch_one(&teams.a.pool)
    .await
    .expect("read the copy's authorship");
    assert_eq!(
        author,
        (
            Some(teams.team_a.owner_id.clone()),
            Some(teams.team_a.owner_id.clone()),
            1,
            second.id.clone()
        )
    );
    assert!(
        copy.position < fresh.position,
        "the copy went to the bottom"
    );
    assert_eq!(
        task_without_identity(&teams.a.pool, &copy.id).await,
        task_without_identity(&teams.a.pool, &fresh.id).await,
    );
    assert_eq!(teams.row_text("tasks", source).await, source_before);
    assert_eq!(teams.snapshot(&teams.team_a).await, snapshot_before);
}

/// Every column of a task but its id and position, as text.
async fn task_without_identity(pool: &SqlitePool, id: &str) -> String {
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('tasks') WHERE name NOT IN ('id', 'position')",
    )
    .fetch_all(pool)
    .await
    .expect("read the task columns");
    let row = columns
        .iter()
        .map(|column| format!("'{column}=' || quote(\"{column}\")"))
        .collect::<Vec<_>>()
        .join(" || '|' || ");
    sqlx::query_scalar(&format!("SELECT {row} FROM tasks WHERE id = ?1"))
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read a task")
}

#[tokio::test]
async fn a_task_cannot_be_copied_into_a_team_the_caller_is_not_in() {
    let teams = TwoTeams::new().await;
    let foreign = &teams.team_b.repository.id;
    let never_issued = TwoTeams::never_issued();

    let into_b = refused(copy_task_to_team(&teams.a, &teams.team_a.ready, foreign).await);
    let into_nothing =
        refused(copy_task_to_team(&teams.a, &teams.team_a.ready, &never_issued).await);

    assert_eq!(into_b.code(), ErrorCode::NotFound);
    assert_eq!(
        serde_json::to_string(&into_b).expect("serializes"),
        serde_json::to_string(&into_nothing)
            .expect("serializes")
            .replace(&never_issued, foreign)
    );
}

// ---------------------------------------------------------------------------
// Solo
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_solo_board_refuses_team_management() {
    let h = TestContext::new().await;
    let ctx = &h.context;
    let team = h.solo.team_id.clone();
    let user = h.solo.user_id.clone();
    let never = TwoTeams::never_issued();
    let before = everything(&ctx.pool).await;

    let answers: Vec<(&str, Error)> = vec![
        (
            "create_team",
            refused(teams::create_team(ctx, "Second").await),
        ),
        (
            "set_member_role",
            refused(set_member_role(ctx, &team, &user, Role::Member).await),
        ),
        (
            "remove_member",
            refused(remove_member(ctx, &team, &user).await),
        ),
        ("leave_team", refused(leave_team(ctx, &team).await)),
        (
            "create_invitation",
            refused(create_invitation(ctx, &team, Role::Member).await),
        ),
        (
            "revoke_invitation",
            refused(revoke_invitation(ctx, &never).await),
        ),
        (
            "preview_invitation",
            refused(preview_invitation(ctx, "rmi_anything").await),
        ),
        (
            "accept_invitation",
            refused(accept_invitation(ctx, "rmi_anything").await),
        ),
        (
            "delete_team",
            refused(delete_team(ctx, &team, "Personal").await),
        ),
        ("delete_account", refused(delete_account(ctx, "solo").await)),
        (
            "copy_task_to_team",
            refused(copy_task_to_team(ctx, &never, &never).await),
        ),
    ];
    for (name, error) in answers {
        assert_eq!(error.code(), ErrorCode::Invalid, "{name}");
        assert_eq!(error.to_string(), SOLO_HAS_ONE_TEAM, "{name}");
    }
    assert_eq!(everything(&ctx.pool).await, before, "nothing was written");

    // The two reads still answer, with solo's one member and one team.
    let members = list_team_members(ctx, &team).await.expect("the members");
    assert_eq!(
        members
            .iter()
            .map(|member| (member.user_id.as_str(), member.role))
            .collect::<Vec<_>>(),
        vec![(user.as_str(), Role::Owner)]
    );
    let targets = list_copy_targets(ctx).await.expect("the copy targets");
    assert_eq!(
        targets
            .iter()
            .map(|target| target.team_id.as_str())
            .collect::<Vec<_>>(),
        vec![team.as_str()]
    );
}

// ---------------------------------------------------------------------------
// Announcements
// ---------------------------------------------------------------------------

#[tokio::test]
async fn team_changes_are_announced_to_the_team() {
    let team = SharedTeam::new().await;
    let mut changes = team.alice.ctx.subscribe();
    let teams_of = |events: &[ChangeEvent]| -> Vec<Audience> {
        events
            .iter()
            .filter(|event| matches!(event.change, Change::Teams(_)))
            .map(|event| event.audience.clone())
            .collect()
    };
    let acme = Audience::Team(team.team_id.clone());

    let created = teams::create_team(&team.alice.ctx, "Second")
        .await
        .expect("create");
    let events = drain(&mut changes);
    assert_eq!(events, vec![ChangeEvent::teams(created.id.clone())]);

    set_member_role(
        &team.alice.ctx,
        &team.team_id,
        &team.bob.user_id,
        Role::Owner,
    )
    .await
    .expect("promote");
    assert_eq!(teams_of(&drain(&mut changes)), vec![acme.clone()]);

    let carol = sign_up(&team, "carol").await;
    let invitation = create_invitation(&team.alice.ctx, &team.team_id, Role::Member)
        .await
        .expect("invite");
    assert_eq!(drain(&mut changes), Vec::<ChangeEvent>::new());
    accept_invitation(&carol, invitation.token.expose())
        .await
        .expect("accept");
    assert_eq!(teams_of(&drain(&mut changes)), vec![acme.clone()]);

    let revoked = create_invitation(&team.alice.ctx, &team.team_id, Role::Member)
        .await
        .expect("invite");
    revoke_invitation(&team.alice.ctx, &revoked.invitation.id)
        .await
        .expect("revoke");
    assert_eq!(teams_of(&drain(&mut changes)), vec![acme.clone()]);

    let assigned = team.task(&team.alice, "Carol's", None).await;
    let as_carol = ServiceContext {
        scope: TeamScope::one(team.team_id.clone()),
        ..carol.clone()
    };
    tasks::assign_task(&as_carol, &assigned, Some(&carol.actor))
        .await
        .expect("carol takes it");
    drain(&mut changes);
    remove_member(&team.alice.ctx, &team.team_id, &carol.actor)
        .await
        .expect("remove");
    let events = drain(&mut changes);
    assert_eq!(teams_of(&events), vec![acme.clone()]);
    assert!(
        events.contains(&ChangeEvent::tasks(
            team.team_id.clone(),
            [assigned.clone()]
        )),
        "{events:?}"
    );

    let purged = delete_team(&team.alice.ctx, &team.team_id, "Acme")
        .await
        .expect("delete");
    let events = drain(&mut changes);
    assert_eq!(
        events,
        vec![
            ChangeEvent::tasks(team.team_id.clone(), purged.task_ids.clone()),
            ChangeEvent::repositories(team.team_id.clone(), [team.repository.id.clone()]),
            ChangeEvent::teams(team.team_id.clone()),
        ]
    );
    assert!(purged.task_ids.contains(&assigned));
}

// ---------------------------------------------------------------------------
// A token is not a credential
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_invitation_token_is_not_a_bearer_token() {
    let team = SharedTeam::new().await;
    let created = create_invitation(&team.alice.ctx, &team.team_id, Role::Member)
        .await
        .expect("an owner invites");
    let auth = SessionsAndTokens {
        pool: team.alice.ctx.pool.clone(),
        clock: Arc::new(team.clock.clone()),
    };
    // A closed pool turns any read into a database error, so `unauthenticated`
    // is proof the prefix was refused before a table was read.
    team.alice.ctx.pool.close().await;

    let error = auth
        .authenticate(Credential::Bearer(created.token.expose()))
        .await
        .expect_err("an invitation authenticates nobody");

    assert_eq!(error.code(), ErrorCode::Unauthenticated);
    assert_eq!(
        serde_json::to_value(&error).expect("serializes"),
        serde_json::to_value(Error::unauthenticated()).expect("serializes")
    );
    assert!(invitations::invitation_path(&created.token).starts_with("/invite#rmi_"));
}

// ---------------------------------------------------------------------------
// Over MCP
// ---------------------------------------------------------------------------

/// A bound MCP server over `ctx`, with no machine, on an OS-chosen port, and
/// a real client connected to it.
async fn mcp_over(ctx: &ServiceContext) -> (McpHandle, RunningService<RoleClient, ()>) {
    let (handle, task) = mcp::build(
        ctx.clone(),
        0,
        RunHandles::default(),
        ProviderProfile::of(RunnerConfig::default().provider.as_ref()),
        None,
    )
    .await;
    tokio::spawn(task.run());
    let client = ()
        .serve(StreamableHttpClientTransport::with_client(
            reqwest::Client::default(),
            StreamableHttpClientTransportConfig::with_uri(handle.url().expect("listening")),
        ))
        .await
        .expect("the server answers `initialize`");
    (handle, client)
}

/// One tool call by name; its structured answer, or a panic naming the
/// refusal.
async fn call(client: &RunningService<RoleClient, ()>, tool: &'static str, args: Value) -> Value {
    let mut params = CallToolRequestParams::new(tool);
    if let Value::Object(arguments) = args {
        params = params.with_arguments(arguments);
    }
    let result = client
        .call_tool(params)
        .await
        .unwrap_or_else(|error| panic!("{tool}: the call completes: {error}"));
    assert_ne!(result.is_error, Some(true), "{tool} refused: {result:?}");
    let described = format!("{result:?}");
    result
        .structured_content
        .unwrap_or_else(|| panic!("{tool} answered no structured content: {described}"))
}

#[tokio::test]
async fn the_operator_manages_a_team_over_mcp() {
    let team = SharedTeam::new().await;
    let alice_ctx = ServiceContext {
        scope: TeamScope::of([team.team_id.clone(), team.alice.personal_team.clone()])
            .expect("alice's teams"),
        ..team.alice.ctx.clone()
    };
    let carol_ctx = sign_up(&team, "carol").await;
    let personal_repository = register(&team, &team.alice.personal, "alices").await;
    let task = team.task(&team.alice, "Share me", None).await;
    let (alice_handle, alice) = mcp_over(&alice_ctx).await;
    let (carol_handle, carol) = mcp_over(&carol_ctx).await;

    let created = call(&alice, "create_team", json!({ "name": "Guild" })).await;
    assert_eq!(created["name"], "Guild");
    assert_eq!(created["role"], "owner");

    let invitation = call(
        &alice,
        "create_invitation",
        json!({ "team_id": team.team_id, "role": "member" }),
    )
    .await;
    let token = invitation["token"].as_str().expect("a token").to_string();
    assert!(secret::is_invitation_token(&token), "{token}");
    assert_eq!(invitation["path"], format!("/invite#{token}"));

    let preview = call(&carol, "preview_invitation", json!({ "token": token })).await;
    assert_eq!(preview["team_name"], "Acme");
    assert_eq!(preview["role"], "member");
    assert_eq!(preview["invited_by_login"], "alice");

    let joined = call(&carol, "accept_invitation", json!({ "token": token })).await;
    assert_eq!(joined["id"], team.team_id.as_str());
    assert_eq!(joined["role"], "member");

    let role = call(
        &alice,
        "set_member_role",
        json!({ "team_id": team.team_id, "user_id": carol_ctx.actor, "role": "owner" }),
    )
    .await;
    assert_eq!(role["role"], "owner");
    let members = call(
        &alice,
        "list_team_members",
        json!({ "team_id": team.team_id }),
    )
    .await;
    let carol_row = members["members"]
        .as_array()
        .expect("members")
        .iter()
        .find(|member| member["login"] == "carol")
        .expect("carol is a member")
        .clone();
    assert_eq!(carol_row["role"], "owner");

    let targets = call(&alice, "list_copy_targets", Value::Null).await;
    assert_eq!(
        targets["teams"][0]["team_id"],
        team.alice.personal_team.as_str(),
        "the personal team comes first"
    );
    let copy = call(
        &alice,
        "copy_task_to_team",
        json!({ "task_id": task, "repository_id": personal_repository }),
    )
    .await;
    assert_eq!(copy["title"], "Share me");
    assert_eq!(copy["column"], "not_ready");
    assert_eq!(copy["repository_id"], personal_repository.as_str());

    let _ = alice.cancel().await;
    let _ = carol.cancel().await;
    alice_handle.shutdown();
    carol_handle.shutdown();
}
