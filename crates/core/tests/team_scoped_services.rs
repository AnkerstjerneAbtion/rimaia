//! Every board service filters by its context's teams (task 039, ADR-0029
//! point 5), asserted at the service layer over the two-team fixture.
//!
//! `tenant_isolation.rs` proves the doors; this proves the rules behind them:
//! a foreign id answered as a missing one, the three cross-team references
//! refused at write time, the entity-less refusal, settings read from the
//! store D28 places them in, the digest marker on the actor's own row, and the
//! aggregates over the caller's teams alone.

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::analytics::{self, Period};
use rimaia_core::board::RunContext;
use rimaia_core::db::settings::{self, placement, Placement, ALL_KEYS};
use rimaia_core::db::{BoardColumn, RunState};
use rimaia_core::events::Change;
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::review::{self, findings, FindingResolution};
use rimaia_core::review_loop::config as review_config;
use rimaia_core::runner::outcome::observed_run_cost;
use rimaia_core::runner::process::{self, DISALLOWED_TOOLS, MAX_TURNS};
use rimaia_core::runner::prompt::{compose_prompt, StrategyGuidance};
use rimaia_core::runner::provider::{AgentProvider, ClaudeProvider};
use rimaia_core::runner::strategy::claim_for_planning;
use rimaia_core::runs::{self, transcript};
use rimaia_core::scheduler::LeaseOwner;
use rimaia_core::strategy::catalogue::{self, STRATEGY_CATALOGUE};
use rimaia_core::strategy::settings::{
    self as strategy_settings, repository_default_key, StrategyApproval, STRATEGY_APPROVAL,
    STRATEGY_DEFAULT,
};
use rimaia_core::tasks::{self, NewTask, TaskFilter, TaskLinkPatch, TaskPatch};
use rimaia_core::testing::teams::{TwoTeams, SENTINEL};
use rimaia_core::testing::{self, TempRepo};
use rimaia_core::{startup, Clock, ErrorCode, Result, ServiceContext};

// ---------------------------------------------------------------------------
// A foreign id is a missing one
// ---------------------------------------------------------------------------

/// An answer as text, with the id it was asked about written as `<id>`, so a
/// foreign id's answer and a never-issued one's compare equal exactly when
/// nothing but the id differs.
fn answer<T: std::fmt::Debug>(result: &Result<T>, id: &str) -> String {
    format!("{result:?}").replace(id, "<id>")
}

#[tokio::test]
async fn a_foreign_id_is_answered_exactly_as_a_missing_one() {
    let t = TwoTeams::new().await;
    let a = &t.a;
    let b = &t.team_b;
    let own = &t.team_a;
    let runs_before = t.run_count().await;
    let board = t.board(a);

    // Each probe is called with team B's id and with a never-issued one, under
    // team A's context. The two answers must be the same answer.
    let mut probes: Vec<(&str, String, String)> = Vec::new();
    macro_rules! probe {
        ($name:literal, $foreign:expr, |$id:ident| $call:expr) => {{
            let foreign: String = $foreign.clone();
            let missing = TwoTeams::never_issued();
            let for_foreign = {
                let $id: &str = &foreign;
                answer(&$call, $id)
            };
            let for_missing = {
                let $id: &str = &missing;
                answer(&$call, $id)
            };
            probes.push(($name, for_foreign, for_missing));
        }};
    }

    probe!("get_task", b.ready, |id| tasks::get_task(a, id).await);
    probe!("move_task", b.ready, |id| {
        tasks::move_task(a, id, BoardColumn::NotReady, None, None).await
    });
    probe!("set_task_dependencies (the task)", b.in_review, |id| {
        tasks::set_task_dependencies(a, id, &[]).await
    });
    probe!("set_task_dependencies (the dependency)", b.done, |id| {
        tasks::set_task_dependencies(a, &own.ready, &[id.to_string()]).await
    });
    probe!("update_task_link", b.link, |id| {
        tasks::update_task_link(
            a,
            id,
            TaskLinkPatch {
                label: Some("relabelled".to_string()),
                url: None,
            },
        )
        .await
    });
    probe!("get_run", b.implementation_run, |id| runs::get_run(a, id)
        .await);
    probe!("read_run_transcript_page", b.implementation_run, |id| {
        // The command's body: the scoped row read, then the page.
        match runs::get_run_row(a, id).await {
            Ok(run) => transcript::read_page(Path::new(&run.log_path), 0, 10)
                .await
                .map(|_| ()),
            Err(error) => Err(error),
        }
    });
    probe!("accept_task_strategy", b.ready, |id| {
        tasks::accept_task_strategy(a, id).await
    });
    probe!("plan_task_strategy", b.ready, |id| {
        claim_for_planning(board.as_ref(), &t.in_flight, id, LeaseOwner::Manual)
            .await
            .map(|claimed| claimed.is_ok())
    });
    probe!("approve", b.in_review, |id| review::approve(a, id).await);
    probe!("reject", b.in_review, |id| review::reject(
        a,
        id,
        "Not this."
    )
    .await);
    probe!("request_changes", b.in_review, |id| {
        review::request_changes(a, id, "More tests.").await
    });
    probe!("findings::list", b.in_review, |id| {
        findings::list(a, id, None).await
    });
    probe!("findings::record (the task)", b.in_review, |id| {
        findings::record(a, id, &own.review_run, vec![]).await
    });
    probe!("findings::record (the run)", b.review_run, |id| {
        findings::record(a, &own.in_review, id, vec![]).await
    });
    probe!("findings::resolve", b.findings[0], |id| {
        findings::resolve(
            a,
            &own.in_review,
            id,
            &own.fix_run,
            FindingResolution::Fixed { note: None },
        )
        .await
    });

    let mut differing = Vec::new();
    for (name, foreign, missing) in &probes {
        if foreign != missing {
            differing.push(format!("{name}\n foreign: {foreign}\n missing: {missing}"));
        }
        assert!(!foreign.contains(SENTINEL), "{name}: {foreign}");
    }
    assert!(differing.is_empty(), "{}", differing.join("\n\n"));

    // None of them created a worktree, a lease, a process or a `runs` row.
    assert_eq!(t.run_count().await, runs_before);
    assert!(t.in_flight.is_empty(), "no lease is held");
    assert_eq!(t.cli.started(), Vec::<String>::new());
    let worktrees: Vec<_> = std::fs::read_dir(t.paths.worktrees_dir())
        .expect("the worktrees directory")
        .collect();
    assert!(worktrees.is_empty(), "no worktree was created");
}

// ---------------------------------------------------------------------------
// References across teams are refused at write time
// ---------------------------------------------------------------------------

async fn every_edge(t: &TwoTeams) -> Vec<(String, String)> {
    sqlx::query_as("SELECT task_id, depends_on_task_id FROM task_dependencies ORDER BY 1, 2")
        .fetch_all(&t.a.pool)
        .await
        .expect("read the edges")
}

#[tokio::test]
async fn a_dependency_on_another_teams_task_is_refused_even_when_both_are_visible() {
    let t = TwoTeams::new().await;
    let edges = every_edge(&t).await;

    let error = tasks::set_task_dependencies(
        &t.both,
        &t.team_a.ready,
        std::slice::from_ref(&t.team_b.done),
    )
    .await
    .expect_err("a dependency across teams");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        format!(
            "cannot make \"team-a ready\" depend on \"{SENTINEL} done\": they are in different \
             repositories, and a dependent task branches from its dependency"
        )
    );
    assert_eq!(every_edge(&t).await, edges, "no edge was written");
}

#[tokio::test]
async fn a_cycle_through_another_teams_task_is_refused_without_naming_it() {
    let t = TwoTeams::new().await;
    // Two hand-written rows, the only way an edge can cross teams: A's ready
    // card depends on B's done card, which depends on A's not-ready card.
    for (task_id, depends_on) in [
        (&t.team_a.ready, &t.team_b.done),
        (&t.team_b.done, &t.team_a.not_ready),
    ] {
        sqlx::query("INSERT INTO task_dependencies (task_id, depends_on_task_id) VALUES (?1, ?2)")
            .bind(task_id)
            .bind(depends_on)
            .execute(&t.a.pool)
            .await
            .expect("a hand-written edge");
    }

    // Saving "not ready depends on ready" closes the loop through B's card.
    let error = tasks::set_task_dependencies(
        &t.a,
        &t.team_a.not_ready,
        std::slice::from_ref(&t.team_a.ready),
    )
    .await
    .expect_err("the cycle is real");

    assert_eq!(error.code(), ErrorCode::Invalid);
    let message = error.to_string();
    assert_eq!(
        message,
        "cannot save these dependencies: they would create a cycle — \"team-a not ready\" \
         depends on \"team-a ready\" depends on a task you cannot see depends on \
         \"team-a not ready\""
    );
    assert!(!message.contains(SENTINEL));
    for id in t.team_b.ids() {
        assert!(!message.contains(&id), "{id}: {message}");
    }
}

#[tokio::test]
async fn a_task_cannot_move_to_another_teams_repository() {
    let t = TwoTeams::new().await;
    let row = t.row_text("tasks", &t.team_a.ready).await;
    let to = |repository_id: &str| TaskPatch {
        repository_id: Some(repository_id.to_string()),
        ..TaskPatch::default()
    };

    let error = tasks::update_task(&t.both, &t.team_a.ready, to(&t.team_b.repository.id))
        .await
        .expect_err("a task does not move between teams");
    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        "a task cannot move to another team; copy it instead"
    );
    assert_eq!(
        t.row_text("tasks", &t.team_a.ready).await,
        row,
        "byte-identical"
    );

    // Under team A's own context, B's repository is as missing as one never
    // registered.
    let foreign = tasks::update_task(&t.a, &t.team_a.ready, to(&t.team_b.repository.id)).await;
    let missing_id = TwoTeams::never_issued();
    let missing = tasks::update_task(&t.a, &t.team_a.ready, to(&missing_id)).await;
    assert_eq!(
        answer(&foreign, &t.team_b.repository.id),
        answer(&missing, &missing_id)
    );
    assert_eq!(t.row_text("tasks", &t.team_a.ready).await, row);
}

// ---------------------------------------------------------------------------
// An entity-less call acts on one team
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_context_reaching_two_teams_is_refused_an_entity_less_call() {
    let t = TwoTeams::new().await;
    let before = (t.snapshot(&t.team_a).await, t.snapshot_b().await);
    let worktrees = tempfile::tempdir().expect("a worktrees directory");
    let directory = TempRepo::init();
    let both = &t.both;

    let refusals: Vec<(&str, rimaia_core::Error)> = vec![
        (
            "list_tasks",
            tasks::list_tasks(both, TaskFilter::default())
                .await
                .expect_err("list_tasks"),
        ),
        (
            "list_repositories",
            repo::list(both).await.expect_err("list_repositories"),
        ),
        (
            "register_repository",
            repo::register(
                both,
                worktrees.path(),
                NewRepository {
                    path: directory.path().to_string_lossy().into_owned(),
                    name: None,
                    worktree_root: None,
                },
            )
            .await
            .expect_err("register_repository"),
        ),
        (
            "set_base_instructions",
            settings::set_base_instructions(both, "Two teams' instructions.")
                .await
                .expect_err("set_base_instructions"),
        ),
        (
            "set_subscription_cost",
            settings::set_subscription_monthly_usd(both, Some(5.0))
                .await
                .expect_err("set_subscription_cost"),
        ),
        (
            "get_review_digest",
            review::digest(both).await.expect_err("get_review_digest"),
        ),
        (
            "mark_review_digest_seen",
            review::mark_seen(both, t.clock.now() - Duration::minutes(1))
                .await
                .expect_err("mark_review_digest_seen"),
        ),
    ];

    for (name, error) in refusals {
        assert_eq!(error.code(), ErrorCode::Invalid, "{name}");
        let message = error.to_string();
        assert!(message.contains(&t.team_a.team_id), "{name}: {message}");
        assert!(message.contains(&t.team_b.team_id), "{name}: {message}");
    }
    assert_eq!(
        (t.snapshot(&t.team_a).await, t.snapshot_b().await),
        before,
        "no refused call wrote anything"
    );

    // A user setting is the actor's under any scope, and an aggregate spans
    // the scope.
    assert_eq!(
        settings::subscription_monthly_usd(both)
            .await
            .expect("the actor's own figure"),
        Some(t.team_a.subscription_monthly_usd)
    );
    let page = analytics::analytics(both, Period::default())
        .await
        .expect("analytics over both teams");
    assert_eq!(
        page.implementation_spend_usd,
        t.team_a.run_costs[0] + t.team_b.run_costs[0]
    );
}

#[tokio::test]
async fn registering_a_directory_another_team_registered_reveals_nothing() {
    let t = TwoTeams::new().await;
    let worktrees = tempfile::tempdir().expect("a worktrees directory");

    let registered = repo::register(
        &t.a,
        worktrees.path(),
        NewRepository {
            path: t.team_b.repository.path.clone(),
            name: Some("Team A's copy".to_string()),
            worktree_root: None,
        },
    )
    .await
    .expect("one directory, registered by a second team");

    let listed: Vec<String> = repo::list(&t.a)
        .await
        .expect("team A's repositories")
        .into_iter()
        .map(|repository| repository.id)
        .collect();
    assert!(listed.contains(&registered.id));
    assert!(!listed.contains(&t.team_b.repository.id));
}

// ---------------------------------------------------------------------------
// Settings come from the team, the user or the runner, as D28 places them
// ---------------------------------------------------------------------------

/// What a run of team A's ready task is composed from, read through a board
/// whose scope reaches both teams.
async fn context_for_team_a(t: &TwoTeams) -> RunContext {
    t.board(&t.both)
        .preview(&t.team_a.ready)
        .await
        .expect("preview team A's task")
}

#[tokio::test]
async fn each_team_composes_its_own_base_instructions() {
    let t = TwoTeams::new().await;
    let context = context_for_team_a(&t).await;
    let guidance = StrategyGuidance::for_task(&context.task);
    let compose = |base: &str| {
        compose_prompt(
            base,
            &context.task,
            &context.repository,
            guidance.as_ref(),
            ClaudeProvider.fanout_noun(),
        )
    };

    let composed = compose(&context.base_instructions);

    assert_eq!(
        composed,
        compose(t.team_a.setting(settings::BASE_INSTRUCTIONS))
    );
    assert!(!composed.contains(SENTINEL), "{composed}");
}

#[tokio::test]
async fn a_run_is_forbidden_what_its_own_team_forbids() {
    let t = TwoTeams::new().await;
    let expected_turns = 41;
    let expected_tools = Some(vec![t.team_a.setting(DISALLOWED_TOOLS).to_string()]);

    let limits = context_for_team_a(&t).await.limits;
    assert_eq!(limits.max_turns, expected_turns);
    assert_eq!(limits.disallowed_tools, expected_tools);
    assert_eq!(
        process::max_turns(&t.both, &t.team_a.team_id)
            .await
            .expect("team A's budget"),
        expected_turns
    );

    // Team B tightening its own limits changes nothing about team A's runs.
    testing::settings::set_team(&t.b, &t.team_b.team_id, MAX_TURNS, "3")
        .await
        .expect("team B's new budget");
    testing::settings::set_team(&t.b, &t.team_b.team_id, DISALLOWED_TOOLS, "Bash(*)")
        .await
        .expect("team B's new blocklist");

    let limits = context_for_team_a(&t).await.limits;
    assert_eq!(limits.max_turns, expected_turns);
    assert_eq!(limits.disallowed_tools, expected_tools);
}

/// Writes `legacy`, `team` and `user` into the three stores for `key`: the
/// legacy row, team A's row and A's owner's row.
async fn plant(t: &TwoTeams, key: &str, legacy: &str, team: &str, user: &str) {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(legacy)
    .execute(&t.a.pool)
    .await
    .expect("plant the legacy row");
    sqlx::query(
        "INSERT INTO team_settings (team_id, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT (team_id, key) DO UPDATE SET value = excluded.value",
    )
    .bind(&t.team_a.team_id)
    .bind(key)
    .bind(team)
    .execute(&t.a.pool)
    .await
    .expect("plant the team row");
    sqlx::query(
        "INSERT INTO user_settings (user_id, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT (user_id, key) DO UPDATE SET value = excluded.value",
    )
    .bind(&t.team_a.owner_id)
    .bind(key)
    .bind(user)
    .execute(&t.a.pool)
    .await
    .expect("plant the user row");
}

fn instant(rfc3339: &str) -> DateTime<Utc> {
    rfc3339.parse().expect("a literal instant")
}

/// Which store a read came from: the one placement whose planted value, as
/// the typed reader would render it, is the one read back. Exactly one may
/// match, so a key whose values cannot all differ plants its odd one out in
/// the store it expects.
fn store_of(read: &str, legacy: &str, team: &str, user: &str) -> Placement {
    let matches: Vec<Placement> = [
        (legacy, Placement::Runner),
        (team, Placement::Team),
        (user, Placement::User),
    ]
    .into_iter()
    .filter(|(value, _)| *value == read)
    .map(|(_, store)| store)
    .collect();
    assert_eq!(
        matches.len(),
        1,
        "{read:?} must match exactly one planted value"
    );
    matches[0]
}

#[tokio::test]
async fn every_settings_key_reads_from_the_store_d28_places_it_in() {
    // The key list 038's placement test walks, plus team A's repository's own
    // strategy default, named here because no constant spells it. For every
    // key, a value in each of the three stores, and the typed reader has to
    // come back with the one from the store `placement` names. A key added to
    // `ALL_KEYS` without a row here fails by name. (An accessor handed a key of
    // another placement is `Error::internal`, which `db::settings`'s own tests
    // pin: the accessors are crate-private.)
    let t = TwoTeams::new().await;
    let a = &t.a;
    let now = t.clock.now();
    let per_repository = repository_default_key(&t.team_a.repository.id);
    let keys: Vec<&str> = ALL_KEYS
        .iter()
        .copied()
        .chain(std::iter::once(per_repository.as_str()))
        .collect();

    for key in &keys {
        let key = *key;
        // (planted legacy, team, user), and the reader's rendering of each.
        let (planted, rendered, read): ([String; 3], [String; 3], String) = match key {
            settings::BASE_INSTRUCTIONS | DISALLOWED_TOOLS | review_config::REVIEW_INSTRUCTIONS => {
                let values = ["legacy".to_string(), "team".to_string(), "user".to_string()];
                plant(&t, key, &values[0], &values[1], &values[2]).await;
                let read = match key {
                    settings::BASE_INSTRUCTIONS => settings::base_instructions(a).await,
                    DISALLOWED_TOOLS => process::disallowed_tools(a, &t.team_a.team_id)
                        .await
                        .map(|rules| rules.join("\n")),
                    _ => review_config::get_review_settings(a)
                        .await
                        .map(|settings| settings.instructions),
                }
                .expect("read");
                (values.clone(), values, read)
            }
            STRATEGY_CATALOGUE => {
                let values = [
                    r#"{"models":[]}"#.to_string(),
                    r#"{"efforts":[]}"#.to_string(),
                    r#"{"planner":{}}"#.to_string(),
                ];
                plant(&t, key, &values[0], &values[1], &values[2]).await;
                let read = catalogue::stored_text(a)
                    .await
                    .expect("read")
                    .expect("a stored catalogue");
                (values.clone(), values, read)
            }
            STRATEGY_DEFAULT => {
                let planted = ["legacy", "team", "user"].map(|v| format!(r#"{{"model":"{v}"}}"#));
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = strategy_settings::global_default(a)
                    .await
                    .expect("read")
                    .model
                    .expect("a model");
                (planted, ["legacy", "team", "user"].map(String::from), read)
            }
            key if key == per_repository => {
                let planted = ["legacy", "team", "user"].map(|v| format!(r#"{{"effort":"{v}"}}"#));
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = strategy_settings::repository_default(a, &t.team_a.repository.id)
                    .await
                    .expect("read")
                    .effort
                    .expect("an effort");
                (planted, ["legacy", "team", "user"].map(String::from), read)
            }
            STRATEGY_APPROVAL => {
                let planted = ["automatic", "manual", "automatic"].map(String::from);
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = match strategy_settings::approval(a).await.expect("read") {
                    StrategyApproval::Automatic => "automatic",
                    StrategyApproval::Manual => "manual",
                }
                .to_string();
                (planted.clone(), planted, read)
            }
            MAX_TURNS => {
                let planted = ["11", "22", "33"].map(String::from);
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = process::max_turns(a, &t.team_a.team_id)
                    .await
                    .expect("read")
                    .to_string();
                (planted.clone(), planted, read)
            }
            review_config::REVIEW_CONFIG => {
                let planted = [1, 2, 3].map(|n| format!(r#"{{"max_review_loops":{n}}}"#));
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = review_config::get_review_settings(a)
                    .await
                    .expect("read")
                    .config
                    .max_review_loops
                    .expect("a loop count")
                    .to_string();
                (planted, ["1", "2", "3"].map(String::from), read)
            }
            settings::SUBSCRIPTION_MONTHLY_USD => {
                let planted = ["1", "2", "3"].map(String::from);
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = settings::subscription_monthly_usd(a)
                    .await
                    .expect("read")
                    .expect("a figure")
                    .to_string();
                (planted.clone(), planted, read)
            }
            review::digest::REVIEW_DIGEST_SEEN_THROUGH => {
                let planted = [
                    "2026-08-01T00:00:00Z",
                    "2026-08-02T00:00:00Z",
                    "2026-08-03T00:00:00Z",
                ]
                .map(String::from);
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = review::digest::seen_through(a)
                    .await
                    .expect("read")
                    .expect("a marker");
                let rendered = planted.clone().map(|value| instant(&value).to_rfc3339());
                (planted, rendered, read.to_rfc3339())
            }
            settings::RUN_ENVIRONMENT => {
                let planted = ["strict_local", "inherit", "inherit"].map(String::from);
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = settings::run_environment(a).await.expect("read").as_str();
                (planted.clone(), planted, read.to_string())
            }
            rimaia_core::mcp::MCP_PORT => {
                let planted = ["4600", "4601", "4602"].map(String::from);
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = rimaia_core::mcp::configured_port(a)
                    .await
                    .expect("read")
                    .to_string();
                (planted.clone(), planted, read)
            }
            rimaia_core::scheduler::capacity::MAX_CONCURRENCY => {
                let planted = ["3", "4", "5"].map(String::from);
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = rimaia_core::scheduler::capacity::max_concurrency(a)
                    .await
                    .expect("read")
                    .to_string();
                (planted.clone(), planted, read)
            }
            rimaia_core::scheduler::capacity::SCHEDULE_MODE => {
                let planted = ["parallel", "sequential", "sequential"].map(String::from);
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = rimaia_core::scheduler::capacity::schedule_mode(a)
                    .await
                    .expect("read")
                    .as_str();
                (planted.clone(), planted, read.to_string())
            }
            rimaia_core::scheduler::QUEUE_STATE => {
                let planted = ["running", "paused", "paused"].map(String::from);
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = rimaia_core::scheduler::queue_state(a)
                    .await
                    .expect("read")
                    .as_str();
                (planted.clone(), planted, read.to_string())
            }
            rimaia_core::schedule::window::ACTIVE_RUN_WINDOW => {
                let names = ["legacy", "team", "user"];
                let planted = names.map(|name| {
                    format!(
                        r#"{{"scheduleId":"s","scheduleName":"{name}","openedAt":"2026-08-20T01:00:00Z","closesAt":null,"mode":"parallel","maxConcurrency":2}}"#
                    )
                });
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = rimaia_core::schedule::window::active(a)
                    .await
                    .expect("read")
                    .expect("an open window")
                    .schedule_name;
                (planted, names.map(String::from), read)
            }
            rimaia_core::scheduler::pause::USAGE_LIMIT_PAUSE_UNTIL => {
                let instants = [1, 2, 3].map(|hours| now + Duration::hours(hours));
                let planted = instants.map(|at| at.to_rfc3339());
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = rimaia_core::scheduler::pause::active_until(a, now)
                    .await
                    .expect("read")
                    .expect("a pause")
                    .to_rfc3339();
                (planted.clone(), planted, read)
            }
            rimaia_core::worktree::cleanup::AUTO_CLEANUP => {
                let planted = ["on_done_acknowledged", "off", "off"].map(String::from);
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = rimaia_core::worktree::cleanup::auto_cleanup(a)
                    .await
                    .expect("read")
                    .as_str();
                (planted.clone(), planted, read.to_string())
            }
            settings::DOCTOR_DISMISSALS => {
                let details = ["legacy", "team", "user"];
                let planted = details.map(|detail| {
                    format!(r#"[{{"check":"git","repository":null,"detail":"{detail}"}}]"#)
                });
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = settings::doctor_dismissals(a)
                    .await
                    .expect("read")
                    .into_iter()
                    .map(|dismissal| dismissal.detail)
                    .collect::<Vec<_>>()
                    .join(",");
                (planted, details.map(String::from), read)
            }
            settings::ONBOARDING_DISMISSED => {
                let planted = ["true", "false", "false"].map(String::from);
                plant(&t, key, &planted[0], &planted[1], &planted[2]).await;
                let read = settings::onboarding_dismissed(a).await.expect("read");
                (planted.clone(), planted, read.to_string())
            }
            other => panic!("no reader for the settings key {other}: add it here"),
        };
        let _ = planted;

        assert_eq!(
            store_of(&read, &rendered[0], &rendered[1], &rendered[2]),
            placement(key),
            "{key} read back {read:?}"
        );
    }
    assert_eq!(keys.len(), ALL_KEYS.len() + 1);
}

// ---------------------------------------------------------------------------
// The digest marker is the actor's, and a verdict counts its own team's queue
// ---------------------------------------------------------------------------

const MARKER: &str = review::digest::REVIEW_DIGEST_SEEN_THROUGH;

fn parsed(stored: Option<String>) -> Option<DateTime<Utc>> {
    stored.map(|value| instant(&value))
}

/// A further `in_review` task of `ctx`'s team, in `repository_id`.
async fn another_in_review(ctx: &ServiceContext, repository_id: &str, title: &str) -> String {
    tasks::create_task(
        ctx,
        NewTask {
            repository_id: repository_id.to_string(),
            title: title.to_string(),
            plan: Some("1. Waiting for review".to_string()),
            extra_instructions: None,
            column: Some(BoardColumn::InReview),
            links: vec![],
        },
    )
    .await
    .expect("an in_review task")
    .id
}

#[tokio::test]
async fn the_digest_marker_is_written_to_the_actors_user_settings_row() {
    let mut t = TwoTeams::new().await;
    // A stale legacy row, which nothing may read or write any more.
    let stale = "2030-01-01T00:00:00Z";
    sqlx::query("INSERT INTO settings (key, value) VALUES (?1, ?2)")
        .bind(MARKER)
        .bind(stale)
        .execute(&t.a.pool)
        .await
        .expect("plant a stale legacy marker");

    let earlier = t.clock.now() - Duration::hours(1);
    review::mark_seen(&t.a, earlier).await.expect("mark seen");
    assert_eq!(
        parsed(t.user_setting(&t.team_a.owner_id, MARKER).await),
        Some(earlier),
        "mark_seen writes A's owner's row"
    );
    assert_eq!(
        review::digest(&t.a).await.expect("the digest").since,
        earlier,
        "the window starts at the actor's marker, never at the stale legacy row"
    );

    // Team A's only `in_review` task, approved: the queue is empty, so the
    // marker moves to now, on the same row.
    t.drain_changes();
    review::approve(&t.a, &t.team_a.in_review)
        .await
        .expect("approve team A's task");
    assert_eq!(
        parsed(t.user_setting(&t.team_a.owner_id, MARKER).await),
        Some(t.clock.now())
    );
    let settings_events: Vec<_> = t
        .drain_changes()
        .into_iter()
        .filter(|event| event.change == Change::Settings)
        .collect();
    assert_eq!(settings_events.len(), 1);
    assert_eq!(settings_events[0].team_id, t.team_a.team_id);

    assert_eq!(t.legacy_setting(MARKER).await.as_deref(), Some(stale));
    assert_eq!(t.user_setting(&t.team_b.owner_id, MARKER).await, None);
}

#[tokio::test]
async fn the_review_that_empties_its_teams_queue_advances_the_marker_whatever_another_team_holds() {
    for scope in ["a", "both"] {
        let t = TwoTeams::new().await;
        for title in ["more B work", "even more B work"] {
            another_in_review(
                &t.b,
                &t.team_b.repository.id,
                &format!("{SENTINEL} {title}"),
            )
            .await;
        }
        let before_b = t.snapshot_b().await;
        let ctx = if scope == "a" { &t.a } else { &t.both };

        review::approve(ctx, &t.team_a.in_review)
            .await
            .expect("approve team A's last task in review");

        assert_eq!(
            parsed(t.user_setting(&t.team_a.owner_id, MARKER).await),
            Some(t.clock.now()),
            "{scope}: the marker advanced"
        );
        assert_eq!(t.snapshot_b().await, before_b, "{scope}: B is untouched");
        assert_eq!(t.user_setting(&t.team_b.owner_id, MARKER).await, None);
    }
}

#[tokio::test]
async fn another_teams_empty_queue_does_not_advance_the_marker() {
    for scope in ["a", "both"] {
        let t = TwoTeams::new().await;
        tasks::move_task_to_bottom(&t.b, &t.team_b.in_review, BoardColumn::Done)
            .await
            .expect("team B's queue empties");
        another_in_review(&t.a, &t.team_a.repository.id, "more A work").await;
        let ctx = if scope == "a" { &t.a } else { &t.both };

        review::approve(ctx, &t.team_a.in_review)
            .await
            .expect("approve one of team A's two");

        assert_eq!(
            t.user_setting(&t.team_a.owner_id, MARKER).await,
            None,
            "{scope}: team A's queue is not empty, whatever team B's holds"
        );
    }
}

#[tokio::test]
async fn a_refused_verdict_on_another_teams_task_leaves_every_marker_where_it_was() {
    let t = TwoTeams::new().await;
    let markers = || async {
        (
            t.user_setting(&t.team_a.owner_id, MARKER).await,
            t.user_setting(&t.team_b.owner_id, MARKER).await,
        )
    };
    let before = markers().await;

    let foreign = review::approve(&t.a, &t.team_b.in_review).await;
    let missing_id = TwoTeams::never_issued();
    let missing = review::approve(&t.a, &missing_id).await;

    assert_eq!(
        answer(&foreign, &t.team_b.in_review),
        answer(&missing, &missing_id)
    );
    assert_eq!(markers().await, before);
}

// ---------------------------------------------------------------------------
// Aggregates over the caller's teams, and nothing else
// ---------------------------------------------------------------------------

#[tokio::test]
async fn analytics_count_only_the_callers_teams_runs() {
    let t = TwoTeams::new().await;
    let [a_implementation, a_review, a_fix] = t.team_a.run_costs;
    let [b_implementation, b_review, b_fix] = t.team_b.run_costs;

    for (ctx, implementation, review_loop, label) in [
        (&t.a, a_implementation, a_review + a_fix, "team A"),
        (&t.b, b_implementation, b_review + b_fix, "team B"),
        (
            &t.both,
            a_implementation + b_implementation,
            a_review + a_fix + b_review + b_fix,
            "both",
        ),
    ] {
        let page = analytics::analytics(ctx, Period::default())
            .await
            .expect("the analytics page");
        assert_eq!(page.implementation_spend_usd, implementation, "{label}");
        assert_eq!(page.review_loop_spend_usd, review_loop, "{label}");
        // D29 point 7's kind breakdown: implementation outcomes apart from the
        // review loop's.
        let teams = if label == "both" { 2 } else { 1 };
        assert_eq!(page.outcomes.succeeded, teams, "{label}");
        assert_eq!(page.review_loop_outcomes.succeeded, 2 * teams, "{label}");
        assert_eq!(page.tasks_attempted, teams, "{label}");
    }
}

#[tokio::test]
async fn the_observed_run_cost_is_the_median_of_the_callers_teams_runs() {
    let t = TwoTeams::new().await;

    for (ctx, median, sample, label) in [
        (&t.a, 1.5, 3, "team A"),
        (&t.b, 20.0, 3, "team B"),
        (&t.both, 2.5, 6, "both"),
    ] {
        let summary = observed_run_cost(ctx, &ClaudeProvider)
            .await
            .expect("the run-cost summary");
        assert_eq!(summary.median_usd, Some(median), "{label}");
        assert_eq!(summary.sample_size, sample, "{label}");
    }
}

#[tokio::test]
async fn the_startup_survey_reports_only_its_scopes_tasks() {
    let t = TwoTeams::new().await;
    // One task per team left running by a crash; the fixture's worktree paths
    // and transcripts are already missing on disk for both teams.
    for task_id in [&t.team_a.ready, &t.team_b.ready] {
        sqlx::query("UPDATE tasks SET run_state = ?1 WHERE id = ?2")
            .bind(RunState::Running)
            .bind(task_id)
            .execute(&t.a.pool)
            .await
            .expect("a crash");
    }
    let sorted = |mut ids: Vec<String>| {
        ids.sort();
        ids
    };

    let report = startup::survey(&t.a).await.expect("survey");
    assert_eq!(report.tasks_left_running, vec![t.team_a.ready.clone()]);
    assert_eq!(report.missing_worktrees, vec![t.team_a.in_review.clone()]);
    assert_eq!(
        sorted(report.missing_run_logs),
        sorted(vec![
            t.team_a.implementation_run.clone(),
            t.team_a.review_run.clone(),
            t.team_a.fix_run.clone(),
        ])
    );

    let report = startup::survey(&t.both).await.expect("survey both");
    assert_eq!(
        sorted(report.tasks_left_running),
        sorted(vec![t.team_a.ready.clone(), t.team_b.ready.clone()])
    );
    assert_eq!(report.missing_run_logs.len(), 6);
}
