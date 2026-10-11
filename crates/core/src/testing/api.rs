//! The board command surface under test (task 046, seam-contract D32 point 5,
//! ADR-0029 point 5).
//!
//! # One case table, two doors
//!
//! [`BOARD_CASES`] holds one cross-team case per board row of the registry.
//! `crates/core/tests/tenant_isolation.rs` runs it through
//! [`api::dispatch`](crate::api::dispatch), and `crates/server/tests/commands.rs`
//! runs **the same table** over HTTP. Two tables could each be complete and
//! still disagree; one table run through two doors is D32's argument for
//! `dispatch` itself. A new board row is not done until it has a case here, and
//! 047, 050, 051 and 060 append theirs to this table.
//!
//! # Callers
//!
//! [`FixedCaller`] compiles only under `testing`. It maps any token to any
//! [`Caller`], any door included, which is how the runner-door and
//! browser-door refusals are written. The contract suite runs every case as
//! the owner's fixture desktop token ([`as_desktop`]), so the account
//! commands' `current` names a real row; the real sessions and tokens are
//! task 047's `identity::authenticate::SessionsAndTokens`, which its own tests
//! drive.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;

use crate::api::registry::Effect;
use crate::api::tail::SERVER_TAIL_RELAY_CAPACITY;
use crate::api::{
    Authenticate, BoardHost, BoxFuture, Caller, Credential, Door, StreamControl, TailRelay,
    TeamGrant,
};
use crate::context::{ServiceContext, TeamScope};
use crate::db::MutationSource;
use crate::error::{Error, Result};
use crate::identity::rate_limit::RateLimiter;
use crate::identity::secret::{self, TokenKind};
use crate::identity::Role;
use crate::runner::events::RunTail;
use crate::runner::provider::ProviderProfile;
use crate::testing::teams::{TeamBoard, TwoTeams};

/// The bearer [`FixedCaller::two_teams`] answers as team A's owner.
pub const TEAM_A_TOKEN: &str = "rmd_team-a-owner";
/// The bearer [`FixedCaller::two_teams`] answers as team B's owner.
pub const TEAM_B_TOKEN: &str = "rmd_team-b-owner";

/// An [`Authenticate`] over a fixed table of tokens.
///
/// A token in the table is its caller, whichever credential shape carried it;
/// anything else is `unauthenticated`, as [`RefuseAll`](crate::api::RefuseAll)
/// answers everything.
#[derive(Debug, Clone, Default)]
pub struct FixedCaller {
    callers: HashMap<String, Caller>,
}

impl FixedCaller {
    pub fn new() -> Self {
        Self::default()
    }

    /// [`TEAM_A_TOKEN`] and [`TEAM_B_TOKEN`], each its team's owner through
    /// [`Door::Desktop`], the door a bearer may drive the board through, over
    /// the owner's fixture desktop token (task 047), so an account command's
    /// `current` names a real row.
    pub fn two_teams(teams: &TwoTeams) -> Self {
        Self::new()
            .with(TEAM_A_TOKEN, as_desktop(&teams.team_a))
            .with(TEAM_B_TOKEN, as_desktop(&teams.team_b))
    }

    /// The same table, with `token` answering as `caller`.
    pub fn with(mut self, token: impl Into<String>, caller: Caller) -> Self {
        self.callers.insert(token.into(), caller);
        self
    }
}

impl Authenticate for FixedCaller {
    fn authenticate<'a>(&'a self, credential: Credential<'a>) -> BoxFuture<'a, Result<Caller>> {
        let token = match credential {
            Credential::Bearer(token) => token,
            Credential::Session { secret, .. } => secret,
        };
        let caller = self
            .callers
            .get(token)
            .cloned()
            .ok_or_else(Error::unauthenticated);
        Box::pin(async move { caller })
    }
}

/// `team`'s owner, as the one member of their own team, through `door`.
pub fn owner(team: &TeamBoard, door: Door) -> Caller {
    Caller {
        user_id: team.owner_id.clone(),
        teams: vec![TeamGrant {
            team_id: team.team_id.clone(),
            role: Role::Owner,
        }],
        door,
    }
}

/// `team`'s owner through their fixture desktop token: the caller both
/// invokers run every case as.
pub fn as_desktop(team: &TeamBoard) -> Caller {
    owner(team, desktop(&team.credentials.desktop_token_id))
}

/// A connected desktop's door, over the token row id `token_id`.
pub fn desktop(token_id: &str) -> Door {
    Door::Desktop {
        token_id: token_id.to_string(),
    }
}

/// A host over `ctx`, with the fixture's provider and paths: what the server
/// and the shell each build over their own context.
///
/// Its tail relay follows `ctx`'s live channel, as every host's does, and has
/// heard one snapshot of each team's implementation run
/// ([`fixture_tail`]), published through the channel like any other, so
/// `get_run_tail`'s case has something to answer.
pub async fn host_over(teams: &TwoTeams, ctx: &ServiceContext) -> BoardHost {
    let tails = TailRelay::new(SERVER_TAIL_RELAY_CAPACITY);
    tokio::spawn(tails.follow(ctx.subscribe_tail()));
    for team in [&teams.team_a, &teams.team_b] {
        ctx.publish_tail(team.team_id.clone(), fixture_tail(team));
        // The follower runs on this runtime; yielding lets it record, and
        // the loop ends the moment it has, with no timer.
        while tails.latest(&team.implementation_run).is_none() {
            tokio::task::yield_now().await;
        }
    }
    BoardHost {
        context: ctx.clone(),
        provider: ProviderProfile::of(teams.runner.provider.as_ref()),
        paths: teams.paths.clone(),
        rate_limits: Arc::new(RateLimiter::new(Arc::new(teams.clock.clone()))),
        tails,
        streams: StreamControl::new(),
    }
}

/// A host over the fixture's own board. Its base context is team A's, which
/// [`dispatch`](crate::api::dispatch) replaces with the caller's before any
/// handler runs.
pub async fn host(teams: &TwoTeams) -> BoardHost {
    host_over(teams, &teams.a).await
}

/// The snapshot [`host_over`]'s relay holds for `team`'s implementation run.
pub fn fixture_tail(team: &TeamBoard) -> RunTail {
    RunTail {
        run_id: team.implementation_run.clone(),
        elapsed_ms: 42_000,
        turns: 3,
        current_tool: None,
        last_assistant_text: Some("Running the tests.".to_string()),
    }
}

/// A second board holding exactly what the fixture's holds now: the same rows
/// and ids, its own pool, its own change channel, the fixture's clock.
///
/// What lets a test run one case through two doors against the same state:
/// each door gets its own copy, so neither sees the other's write. The
/// directory holds the copy and must outlive the context.
pub async fn twin(teams: &TwoTeams) -> (ServiceContext, TempDir) {
    let dir = tempfile::Builder::new()
        .prefix("rimaia-twin-board-")
        .tempdir()
        .expect("a directory for the copy");
    let file = dir.path().join("rimaia.db");
    sqlx::query("VACUUM INTO ?1")
        .bind(file.to_string_lossy().into_owned())
        .execute(&teams.a.pool)
        .await
        .expect("copy the board");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&file)
                .foreign_keys(true)
                .busy_timeout(crate::db::BUSY_TIMEOUT),
        )
        .await
        .expect("open the copy");
    let ctx = ServiceContext::new(
        pool,
        Arc::new(teams.clock.clone()),
        MutationSource::Ui,
        TeamScope::one(teams.team_a.team_id.clone()),
        teams.team_a.owner_id.clone(),
    );
    (ctx, dir)
}

// ---------------------------------------------------------------------------
// The case table
// ---------------------------------------------------------------------------

/// One board row's cross-team case.
pub struct BoardCase {
    pub name: &'static str,
    pub effect: Effect,
    /// Arguments that succeed for team A's owner.
    pub own: fn(&TwoTeams) -> Value,
    pub foreign: Foreign,
}

pub enum Foreign {
    /// Arguments naming team B's entities. Run as team A, the answer is
    /// `not_found`.
    Ids(fn(&TwoTeams) -> Value),
    /// A filter naming a team B entity (`list_tasks`, `list_runs` by
    /// `repositoryId`). Run as team A, the answer equals the one for a
    /// never-issued id.
    Filter(fn(&TwoTeams) -> Value),
    /// The command can name no entity: a team setting, an aggregate.
    EntityLess,
    /// The command carries a token, which is its own capability, and no team
    /// id to substitute: a valid team B token rightly reaches team B (task
    /// 051). Each argument object carries one of team B's tokens nobody can
    /// use any more, expired, revoked and accepted. Run as team A, each answer
    /// carries nothing of B's and equals the answer for a never-issued token
    /// ([`with_never_issued_token`]), and B's rows are unchanged.
    Capability(fn(&TwoTeams) -> Vec<Value>),
}

impl Foreign {
    /// The arguments naming team B, or `None` for an entity-less command.
    /// A token-bearing command's first argument object, of several.
    pub fn args(&self, teams: &TwoTeams) -> Option<Value> {
        self.arg_sets(teams).into_iter().next()
    }

    /// Every argument object a cross-team check runs: one naming team B's
    /// ids, one per unusable token of B's, or none for an entity-less
    /// command, which runs its own arguments instead.
    pub fn arg_sets(&self, teams: &TwoTeams) -> Vec<Value> {
        match self {
            Foreign::Ids(args) | Foreign::Filter(args) => vec![args(teams)],
            Foreign::Capability(args) => args(teams),
            Foreign::EntityLess => Vec::new(),
        }
    }
}

/// `args` with its `token` replaced by a freshly minted invitation token that
/// was never issued: what a capability's foreign answer must equal.
pub fn with_never_issued_token(args: &Value) -> Value {
    let mut args = args.clone();
    args["token"] = Value::String(secret::mint_invitation_token().expose().to_string());
    args
}

/// Team B's three unusable invitation tokens, each as `{ token }`.
fn unusable_tokens(teams: &TwoTeams) -> Vec<Value> {
    teams
        .team_b
        .invitations
        .unusable()
        .into_iter()
        .map(|token| json!({ "token": token.expose() }))
        .collect()
}

/// The case named `name`, if there is one.
pub fn case(name: &str) -> Option<&'static BoardCase> {
    BOARD_CASES.iter().find(|case| case.name == name)
}

/// The cases whose `own` arguments this fixture cannot make succeed, each with
/// why. A refusal here is still answered identically by both doors, which is
/// what the transport test compares; it is listed so that a case quietly
/// starting to fail is a test failure, not a shrug.
pub const REFUSED_FOR_TEAM_A: &[(&str, &str)] = &[
    (
        "remove_repository",
        "team A's one repository still has tasks, and removing it is refused until none does",
    ),
    (
        "accept_task_strategy",
        "no planner has proposed a strategy on team A's tasks",
    ),
    (
        "give_up_on_task",
        "no task of team A's is waiting out a retry",
    ),
    (
        "leave_team",
        "team A's owner is in only team A, a personal team, which always keeps its person",
    ),
    (
        "delete_team",
        "team A is a personal team, which is deleted with its account, not on its own",
    ),
];

/// One per board row. `every_board_command_has_a_case` holds this equal to the
/// registry's board rows, name and effect.
pub static BOARD_CASES: &[BoardCase] = &[
    // repositories
    BoardCase {
        name: "list_repositories",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "update_repository",
        effect: Effect::Write,
        own: |t| json!({ "id": t.team_a.repository.id, "patch": { "name": "renamed" } }),
        foreign: Foreign::Ids(
            |t| json!({ "id": t.team_b.repository.id, "patch": { "name": "renamed" } }),
        ),
    },
    BoardCase {
        name: "set_repository_unattended_ceiling",
        effect: Effect::Write,
        own: |t| json!({ "id": t.team_a.repository.id, "allowed": true }),
        foreign: Foreign::Ids(|t| json!({ "id": t.team_b.repository.id, "allowed": true })),
    },
    BoardCase {
        name: "remove_repository",
        effect: Effect::Write,
        own: |t| json!({ "id": t.team_a.repository.id }),
        foreign: Foreign::Ids(|t| json!({ "id": t.team_b.repository.id })),
    },
    // tasks
    BoardCase {
        name: "create_task",
        effect: Effect::Write,
        own: |t| {
            json!({ "input": {
                "repositoryId": t.team_a.repository.id,
                "title": "planted",
                "plan": "1. Plant it",
            } })
        },
        foreign: Foreign::Ids(|t| {
            json!({ "input": {
                "repositoryId": t.team_b.repository.id,
                "title": "planted",
                "plan": "1. Plant it",
            } })
        }),
    },
    BoardCase {
        name: "get_task",
        effect: Effect::Read,
        own: |t| json!({ "id": t.team_a.ready }),
        foreign: Foreign::Ids(|t| json!({ "id": t.team_b.ready })),
    },
    BoardCase {
        name: "list_tasks",
        effect: Effect::Read,
        own: |_| json!({ "filter": {} }),
        foreign: Foreign::Filter(
            |t| json!({ "filter": { "repositoryId": t.team_b.repository.id } }),
        ),
    },
    BoardCase {
        name: "update_task",
        effect: Effect::Write,
        own: |t| json!({ "id": t.team_a.ready, "patch": { "title": "renamed" } }),
        foreign: Foreign::Ids(|t| json!({ "id": t.team_b.ready, "patch": { "title": "renamed" } })),
    },
    BoardCase {
        name: "delete_task",
        effect: Effect::Write,
        own: |t| json!({ "id": t.team_a.not_ready }),
        foreign: Foreign::Ids(|t| json!({ "id": t.team_b.not_ready })),
    },
    BoardCase {
        name: "archive_task",
        effect: Effect::Write,
        own: |t| json!({ "id": t.team_a.ready }),
        foreign: Foreign::Ids(|t| json!({ "id": t.team_b.ready })),
    },
    // The bulk archive reports a refusal per id rather than failing the call,
    // so a foreign id is not `not_found` at the top: it is a refusal that has
    // to read exactly as a never-issued id's does, which is the `Filter` check.
    BoardCase {
        name: "archive_tasks",
        effect: Effect::Write,
        own: |t| json!({ "ids": [t.team_a.ready, t.team_a.not_ready] }),
        foreign: Foreign::Filter(|t| json!({ "ids": [t.team_b.ready] })),
    },
    BoardCase {
        name: "unarchive_task",
        effect: Effect::Write,
        own: |t| json!({ "id": t.team_a.archived }),
        foreign: Foreign::Ids(|t| json!({ "id": t.team_b.archived })),
    },
    BoardCase {
        name: "move_task",
        effect: Effect::Write,
        own: |t| {
            json!({
                "id": t.team_a.ready,
                "column": "not_ready",
                "beforeId": t.team_a.not_ready,
                "afterId": null,
            })
        },
        foreign: Foreign::Ids(|t| {
            json!({
                "id": t.team_b.ready,
                "column": "not_ready",
                "beforeId": t.team_b.not_ready,
                "afterId": null,
            })
        }),
    },
    BoardCase {
        name: "set_task_run_state",
        effect: Effect::Write,
        own: |t| json!({ "id": t.team_a.ready, "runState": "queued" }),
        foreign: Foreign::Ids(|t| json!({ "id": t.team_b.ready, "runState": "queued" })),
    },
    BoardCase {
        name: "add_task_link",
        effect: Effect::Write,
        own: |t| {
            json!({
                "taskId": t.team_a.ready,
                "input": { "label": "a link", "url": "https://example.invalid/a" },
            })
        },
        foreign: Foreign::Ids(|t| {
            json!({
                "taskId": t.team_b.ready,
                "input": { "label": "a link", "url": "https://example.invalid/a" },
            })
        }),
    },
    BoardCase {
        name: "update_task_link",
        effect: Effect::Write,
        own: |t| json!({ "linkId": t.team_a.link, "patch": { "label": "renamed" } }),
        foreign: Foreign::Ids(
            |t| json!({ "linkId": t.team_b.link, "patch": { "label": "renamed" } }),
        ),
    },
    BoardCase {
        name: "remove_task_link",
        effect: Effect::Write,
        own: |t| json!({ "linkId": t.team_a.link }),
        foreign: Foreign::Ids(|t| json!({ "linkId": t.team_b.link })),
    },
    BoardCase {
        name: "reorder_task_link",
        effect: Effect::Write,
        own: |t| json!({ "linkId": t.team_a.link }),
        foreign: Foreign::Ids(|t| json!({ "linkId": t.team_b.link })),
    },
    BoardCase {
        name: "set_task_dependencies",
        effect: Effect::Write,
        own: |t| json!({ "taskId": t.team_a.in_review, "dependsOn": [t.team_a.done] }),
        foreign: Foreign::Ids(
            |t| json!({ "taskId": t.team_b.in_review, "dependsOn": [t.team_b.done] }),
        ),
    },
    BoardCase {
        name: "assign_task",
        effect: Effect::Write,
        own: |t| json!({ "taskId": t.team_a.ready, "assigneeId": t.team_a.owner_id }),
        foreign: Foreign::Ids(
            |t| json!({ "taskId": t.team_b.ready, "assigneeId": t.team_b.owner_id }),
        ),
    },
    BoardCase {
        name: "get_blocking_reason",
        effect: Effect::Read,
        own: |t| json!({ "taskId": t.team_a.in_review }),
        foreign: Foreign::Ids(|t| json!({ "taskId": t.team_b.in_review })),
    },
    // review
    BoardCase {
        name: "approve_task",
        effect: Effect::Write,
        own: |t| json!({ "taskId": t.team_a.in_review }),
        foreign: Foreign::Ids(|t| json!({ "taskId": t.team_b.in_review })),
    },
    BoardCase {
        name: "reject_task",
        effect: Effect::Write,
        own: |t| json!({ "taskId": t.team_a.in_review, "note": "Not this." }),
        foreign: Foreign::Ids(|t| json!({ "taskId": t.team_b.in_review, "note": "Not this." })),
    },
    BoardCase {
        name: "request_task_changes",
        effect: Effect::Write,
        own: |t| json!({ "taskId": t.team_a.in_review, "note": "More tests." }),
        foreign: Foreign::Ids(|t| json!({ "taskId": t.team_b.in_review, "note": "More tests." })),
    },
    BoardCase {
        name: "get_task_dependents",
        effect: Effect::Read,
        own: |t| json!({ "taskId": t.team_a.done }),
        foreign: Foreign::Ids(|t| json!({ "taskId": t.team_b.done })),
    },
    BoardCase {
        name: "get_review_digest",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "mark_review_digest_seen",
        effect: Effect::Write,
        own: |_| json!({ "through": "2026-08-20T01:00:00Z" }),
        foreign: Foreign::EntityLess,
    },
    // Findings filtered by their task, like `list_runs_for_task`: an unknown
    // task has none.
    BoardCase {
        name: "list_review_findings",
        effect: Effect::Read,
        own: |t| json!({ "taskId": t.team_a.in_review }),
        foreign: Foreign::Filter(|t| json!({ "taskId": t.team_b.in_review })),
    },
    BoardCase {
        name: "get_review_history",
        effect: Effect::Read,
        own: |t| json!({ "taskId": t.team_a.in_review }),
        foreign: Foreign::Ids(|t| json!({ "taskId": t.team_b.in_review })),
    },
    BoardCase {
        name: "get_review_level",
        effect: Effect::Read,
        own: |t| json!({ "level": "task", "id": t.team_a.in_review }),
        foreign: Foreign::Ids(|t| json!({ "level": "task", "id": t.team_b.in_review })),
    },
    BoardCase {
        name: "get_review_settings",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "set_review_settings",
        effect: Effect::Write,
        own: |_| json!({ "instructions": "Review it.", "config": null }),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "set_repository_review_config",
        effect: Effect::Write,
        own: |t| json!({ "repositoryId": t.team_a.repository.id, "config": null }),
        foreign: Foreign::Ids(
            |t| json!({ "repositoryId": t.team_b.repository.id, "config": null }),
        ),
    },
    BoardCase {
        name: "set_task_review",
        effect: Effect::Write,
        own: |t| json!({ "taskId": t.team_a.in_review, "config": null }),
        foreign: Foreign::Ids(|t| json!({ "taskId": t.team_b.in_review, "config": null })),
    },
    // consent
    BoardCase {
        name: "accept_content",
        effect: Effect::Write,
        own: |t| {
            json!({
                "teamId": t.team_a.team_id,
                "taskId": t.team_a.ready,
                "kind": "plan",
                // Created with a plan, then given a link, which revises it.
                "revision": "2",
            })
        },
        foreign: Foreign::Ids(|t| {
            json!({
                "teamId": t.team_b.team_id,
                "taskId": t.team_b.ready,
                "kind": "plan",
                "revision": "2",
            })
        }),
    },
    BoardCase {
        name: "get_task_consent",
        effect: Effect::Read,
        own: |t| json!({ "taskId": t.team_a.ready, "runnerId": t.team_a.runner_id }),
        foreign: Foreign::Ids(
            |t| json!({ "taskId": t.team_b.ready, "runnerId": t.team_a.runner_id }),
        ),
    },
    // settings
    BoardCase {
        name: "get_base_instructions",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "set_base_instructions",
        effect: Effect::Write,
        own: |_| json!({ "value": "Open a draft PR." }),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "get_run_cost_summary",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    // strategy
    BoardCase {
        name: "get_strategy_catalogue",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "set_strategy_catalogue",
        effect: Effect::Write,
        own: |_| json!({ "value": r#"{"models":[]}"# }),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "get_strategy_defaults",
        effect: Effect::Read,
        own: |t| json!({ "repositoryId": t.team_a.repository.id }),
        foreign: Foreign::Ids(|t| json!({ "repositoryId": t.team_b.repository.id })),
    },
    BoardCase {
        name: "set_strategy_defaults",
        effect: Effect::Write,
        own: |t| json!({ "repositoryId": t.team_a.repository.id, "value": { "mode": "default" } }),
        foreign: Foreign::Ids(
            |t| json!({ "repositoryId": t.team_b.repository.id, "value": { "mode": "default" } }),
        ),
    },
    BoardCase {
        name: "get_strategy_approval",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "set_strategy_approval",
        effect: Effect::Write,
        own: |_| json!({ "value": "manual" }),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "accept_task_strategy",
        effect: Effect::Write,
        own: |t| json!({ "taskId": t.team_a.ready }),
        foreign: Foreign::Ids(|t| json!({ "taskId": t.team_b.ready })),
    },
    BoardCase {
        name: "clear_task_strategy",
        effect: Effect::Write,
        own: |t| json!({ "taskId": t.team_a.ready }),
        foreign: Foreign::Ids(|t| json!({ "taskId": t.team_b.ready })),
    },
    // runs
    BoardCase {
        name: "give_up_on_task",
        effect: Effect::Write,
        own: |t| json!({ "taskId": t.team_a.ready }),
        foreign: Foreign::Ids(|t| json!({ "taskId": t.team_b.ready })),
    },
    // Runs filtered by their task: an unknown task has none, which is an
    // empty list rather than a missing entity.
    BoardCase {
        name: "list_runs_for_task",
        effect: Effect::Read,
        own: |t| json!({ "taskId": t.team_a.in_review }),
        foreign: Foreign::Filter(|t| json!({ "taskId": t.team_b.in_review })),
    },
    BoardCase {
        name: "list_runs",
        effect: Effect::Read,
        own: |_| json!({ "filter": {} }),
        foreign: Foreign::Filter(
            |t| json!({ "filter": { "repositoryId": t.team_b.repository.id } }),
        ),
    },
    BoardCase {
        name: "get_run",
        effect: Effect::Read,
        own: |t| json!({ "runId": t.team_a.implementation_run }),
        foreign: Foreign::Ids(|t| json!({ "runId": t.team_b.implementation_run })),
    },
    // The relay's latest snapshot for team A's run, which `host_over` seeds.
    BoardCase {
        name: "get_run_tail",
        effect: Effect::Read,
        own: |t| json!({ "runId": t.team_a.implementation_run }),
        foreign: Foreign::Ids(|t| json!({ "runId": t.team_b.implementation_run })),
    },
    // analytics
    BoardCase {
        name: "get_analytics",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "get_subscription_cost",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "set_subscription_cost",
        effect: Effect::Write,
        own: |_| json!({ "value": 25.0 }),
        foreign: Foreign::EntityLess,
    },
    // account (task 047): "B's ids" are the other user's.
    BoardCase {
        name: "get_account",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "list_sessions",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "revoke_session",
        effect: Effect::Write,
        own: |t| json!({ "id": t.team_a.credentials.session_id }),
        foreign: Foreign::Ids(|t| json!({ "id": t.team_b.credentials.session_id })),
    },
    BoardCase {
        name: "list_api_tokens",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "create_personal_access_token",
        effect: Effect::Write,
        own: |t| json!({ "label": "Claude Code", "teamIds": [t.team_a.team_id] }),
        foreign: Foreign::Ids(|t| json!({ "label": "Claude Code", "teamIds": [t.team_b.team_id] })),
    },
    BoardCase {
        name: "revoke_api_token",
        effect: Effect::Write,
        own: |t| json!({ "id": t.team_a.credentials.personal_token_id }),
        foreign: Foreign::Ids(|t| json!({ "id": t.team_b.credentials.personal_token_id })),
    },
    BoardCase {
        name: "create_pairing_code",
        effect: Effect::Write,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "pair_own_runner",
        effect: Effect::Write,
        own: |_| json!({ "label": "laptop", "provider": "claude-code" }),
        foreign: Foreign::EntityLess,
    },
    // the person's teams and runners (task 050): another user's runner is B's
    BoardCase {
        name: "list_teams",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "list_runners",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "unpair_runner",
        effect: Effect::Write,
        own: |t| json!({ "id": t.team_a.credentials.paired_runner_id }),
        foreign: Foreign::Ids(|t| json!({ "id": t.team_b.credentials.paired_runner_id })),
    },
    // teams, invitations and roles (task 051): each team's guest is the member
    // an owner acts on, and a token is its own capability
    BoardCase {
        name: "create_team",
        effect: Effect::Write,
        own: |_| json!({ "name": "Acme" }),
        foreign: Foreign::EntityLess,
    },
    BoardCase {
        name: "list_team_members",
        effect: Effect::Read,
        own: |t| json!({ "teamId": t.team_a.team_id }),
        foreign: Foreign::Ids(|t| json!({ "teamId": t.team_b.team_id })),
    },
    BoardCase {
        name: "set_member_role",
        effect: Effect::Write,
        own: |t| json!({ "teamId": t.team_a.team_id, "userId": t.team_a.guest_id, "role": "owner" }),
        foreign: Foreign::Ids(
            |t| json!({ "teamId": t.team_b.team_id, "userId": t.team_b.guest_id, "role": "owner" }),
        ),
    },
    BoardCase {
        name: "remove_member",
        effect: Effect::Write,
        own: |t| json!({ "teamId": t.team_a.team_id, "userId": t.team_a.guest_id }),
        foreign: Foreign::Ids(
            |t| json!({ "teamId": t.team_b.team_id, "userId": t.team_b.guest_id }),
        ),
    },
    BoardCase {
        name: "leave_team",
        effect: Effect::Write,
        own: |t| json!({ "teamId": t.team_a.team_id }),
        foreign: Foreign::Ids(|t| json!({ "teamId": t.team_b.team_id })),
    },
    BoardCase {
        name: "create_invitation",
        effect: Effect::Write,
        own: |t| json!({ "teamId": t.team_a.team_id, "role": "member" }),
        foreign: Foreign::Ids(|t| json!({ "teamId": t.team_b.team_id, "role": "member" })),
    },
    BoardCase {
        name: "list_invitations",
        effect: Effect::Read,
        own: |t| json!({ "teamId": t.team_a.team_id }),
        foreign: Foreign::Ids(|t| json!({ "teamId": t.team_b.team_id })),
    },
    BoardCase {
        name: "revoke_invitation",
        effect: Effect::Write,
        own: |t| json!({ "id": t.team_a.invitations.pending_id }),
        foreign: Foreign::Ids(|t| json!({ "id": t.team_b.invitations.pending_id })),
    },
    // Team B's pending link: the one usable token a team A owner can be
    // handed, and the preview is the one read that names a team outside the
    // scope, because the token is the capability.
    BoardCase {
        name: "preview_invitation",
        effect: Effect::Read,
        own: |t| json!({ "token": t.team_b.invitations.pending.expose() }),
        foreign: Foreign::Capability(unusable_tokens),
    },
    BoardCase {
        name: "accept_invitation",
        effect: Effect::Write,
        own: |t| json!({ "token": t.team_b.invitations.pending.expose() }),
        foreign: Foreign::Capability(unusable_tokens),
    },
    BoardCase {
        name: "list_copy_targets",
        effect: Effect::Read,
        own: |_| json!({}),
        foreign: Foreign::EntityLess,
    },
    // B's plan copied into A's repository: the direction that would leak.
    BoardCase {
        name: "copy_task_to_team",
        effect: Effect::Write,
        own: |t| json!({ "taskId": t.team_a.ready, "repositoryId": t.team_a.repository.id }),
        foreign: Foreign::Ids(
            |t| json!({ "taskId": t.team_b.ready, "repositoryId": t.team_a.repository.id }),
        ),
    },
    BoardCase {
        name: "delete_team",
        effect: Effect::Write,
        own: |t| json!({ "teamId": t.team_a.team_id, "confirmName": "Personal" }),
        foreign: Foreign::Ids(|t| json!({ "teamId": t.team_b.team_id, "confirmName": "Personal" })),
    },
    // Last, on purpose: the cross-team loops run every case on one board, and
    // this one deletes team A's owner when nothing refuses it.
    BoardCase {
        name: "delete_account",
        effect: Effect::Write,
        own: |_| json!({ "confirmLogin": "ada" }),
        foreign: Foreign::EntityLess,
    },
];

// ---------------------------------------------------------------------------
// What the checks share
// ---------------------------------------------------------------------------

/// Every one of `team`'s ids in `answer` that `sent` does not mention, and the
/// sentinel if `answer` carries it: 039's check 1. An id the caller sent may
/// come back in a refusal; any other is a leak.
pub fn leaks(team: &TeamBoard, sent: &Value, answer: &str) -> Vec<String> {
    let sent = sent.to_string();
    let mut leaked: Vec<String> = team
        .ids()
        .into_iter()
        .filter(|id| !sent.contains(id.as_str()) && answer.contains(id.as_str()))
        .collect();
    if answer.contains(crate::testing::teams::SENTINEL) {
        leaked.push(crate::testing::teams::SENTINEL.to_string());
    }
    leaked
}

/// `args` with every id of `team`'s it mentions replaced by a fresh one, and
/// the replacements, for 039's check 2.
pub fn with_never_issued_ids(team: &TeamBoard, args: &Value) -> (Value, Vec<(String, String)>) {
    let mut text = args.to_string();
    let mut substitutions = Vec::new();
    for id in team.ids() {
        if text.contains(id.as_str()) {
            let fresh = TwoTeams::never_issued();
            text = text.replace(id.as_str(), &fresh);
            substitutions.push((id, fresh));
        }
    }
    let args = serde_json::from_str(&text).expect("ids substitute inside strings");
    (args, substitutions)
}

/// `answer` with each fresh id put back as the foreign one it replaced, so a
/// never-issued id's answer compares with a foreign id's.
pub fn restore_ids(answer: &str, substitutions: &[(String, String)]) -> String {
    substitutions
        .iter()
        .fold(answer.to_string(), |text, (foreign, fresh)| {
            text.replace(fresh, foreign)
        })
}

/// Every id-shaped string anywhere in the board `ctx` reads: what a call can
/// mention without having minted it.
pub async fn ids_on_the_board(ctx: &ServiceContext) -> BTreeSet<String> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&ctx.pool)
    .await
    .expect("list the tables");
    let mut ids = BTreeSet::new();
    for table in tables {
        let columns: Vec<String> =
            sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .fetch_all(&ctx.pool)
                .await
                .expect("read a table's columns");
        if columns.is_empty() {
            continue;
        }
        let row = columns
            .iter()
            .map(|column| format!("quote(\"{column}\")"))
            .collect::<Vec<_>>()
            .join(" || '|' || ");
        let rows: Vec<String> = sqlx::query_scalar(&format!("SELECT {row} FROM \"{table}\""))
            .fetch_all(&ctx.pool)
            .await
            .expect("dump a table");
        for text in rows {
            ids.extend(id_shaped(&text));
        }
    }
    ids
}

/// `answer` with every id it carries that `known` does not hold, which is an
/// id the call itself minted, replaced by `minted-<n>` in order of first
/// appearance. Nothing else is touched: two doors that differ anywhere but in
/// what they minted still differ after this.
pub fn normalize_minted(answer: &Value, known: &BTreeSet<String>) -> Value {
    let mut text = answer.to_string();
    let mut minted: BTreeMap<String, String> = BTreeMap::new();
    for id in id_shaped(&text) {
        if !known.contains(&id) && !minted.contains_key(&id) {
            let label = format!("minted-{}", minted.len() + 1);
            minted.insert(id, label);
        }
    }
    for (id, label) in &minted {
        text = text.replace(id.as_str(), label);
    }
    serde_json::from_str(&text).expect("ids normalize inside strings")
}

/// The four rows that answer a secret minted by the call, and where the
/// secret sits in the answer: two transports can never agree on one.
const MINTED_SECRETS: [(&str, &str); 4] = [
    ("create_personal_access_token", "token"),
    ("pair_own_runner", "token"),
    ("create_pairing_code", "code"),
    ("create_invitation", "token"),
];

/// `answer` with the secret `case` minted replaced by `<minted secret>`, after
/// checking it has its shape: an `rmp_`, `rmr_` or `rmi_` token, or a pairing
/// code.
/// Any other case's answer is returned unchanged, and so is a refusal, which
/// carries no secret. Checking first means normalizing cannot hide a missing
/// secret (task 047 Scope 12).
pub fn normalize_minted_secret(case: &str, answer: &Value) -> std::result::Result<Value, String> {
    let Some((_, field)) = MINTED_SECRETS.iter().find(|(name, _)| *name == case) else {
        return Ok(answer.clone());
    };
    if answer.get("code").is_some() && answer.get("message").is_some() {
        return Ok(answer.clone());
    }
    let minted = answer
        .get(*field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{case}: the answer has no `{field}`: {answer}"))?;
    let shaped = match case {
        "create_personal_access_token" => secret::is_token_of(TokenKind::Personal, minted),
        "pair_own_runner" => secret::is_token_of(TokenKind::Runner, minted),
        "create_invitation" => secret::is_invitation_token(minted),
        _ => secret::is_pairing_code(minted),
    };
    if !shaped {
        return Err(format!(
            "{case}: `{field}` does not have a minted secret's shape"
        ));
    }
    let mut normalized = answer.clone();
    normalized[*field] = Value::String("<minted secret>".to_string());
    Ok(normalized)
}

/// Every UUID-shaped run of `text`, in order: `8-4-4-4-12` hex digits, the
/// shape `db::new_id` mints.
fn id_shaped(text: &str) -> Vec<String> {
    const SHAPE: usize = 36;
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while at + SHAPE <= bytes.len() {
        let candidate = &bytes[at..at + SHAPE];
        let fits = candidate.iter().enumerate().all(|(i, byte)| match i {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        });
        let bounded = at == 0 || !bytes[at - 1].is_ascii_hexdigit();
        if fits && bounded {
            found.push(text[at..at + SHAPE].to_string());
            at += SHAPE;
        } else {
            at += 1;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_minted_id_is_named_by_order_and_a_known_one_is_left_alone() {
        let known: BTreeSet<String> = ["3f2b1c00-0000-4000-8000-0000000000a1".to_string()].into();
        let answer = json!({
            "id": "7d0f8a8e-1111-4222-8333-444455556666",
            "repositoryId": "3f2b1c00-0000-4000-8000-0000000000a1",
            "links": [{ "id": "9a9a9a9a-1111-4222-8333-444455556666",
                        "taskId": "7d0f8a8e-1111-4222-8333-444455556666" }],
        });

        assert_eq!(
            normalize_minted(&answer, &known),
            json!({
                "id": "minted-1",
                "repositoryId": "3f2b1c00-0000-4000-8000-0000000000a1",
                "links": [{ "id": "minted-2", "taskId": "minted-1" }],
            })
        );
    }

    #[tokio::test]
    async fn fixed_caller_answers_its_tokens_and_refuses_any_other() {
        let teams = TwoTeams::new().await;
        let auth = FixedCaller::two_teams(&teams);

        let a = auth
            .authenticate(Credential::Bearer(TEAM_A_TOKEN))
            .await
            .expect("team A's token");
        assert_eq!(a, as_desktop(&teams.team_a));

        let refused = auth
            .authenticate(Credential::Bearer("rmd_nobody"))
            .await
            .expect_err("an unknown token");
        assert_eq!(refused.code(), crate::ErrorCode::Unauthenticated);
    }
}
