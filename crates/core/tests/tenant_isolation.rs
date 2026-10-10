//! One team cannot see another's board through any door (task 039, ADR-0029
//! point 5).
//!
//! # The registry
//!
//! One case per MCP tool, keyed by tool name, each written as data: a name,
//! whether it writes, and a function that builds the argument object, naming
//! team B's ids wherever the tool takes an id. The invoker is the real MCP
//! client calling the tool by name against a bound loopback server, so a case
//! here is a case task 046 lifts into `testing/api.rs` and runs again through
//! `api::dispatch` without rewriting it. Every case gets four checks, run as
//! team A:
//!
//! 1. nothing of team B's leaks into the answer: none of B's ids the caller did
//!    not send, and never the sentinel;
//! 2. a foreign id reads as a missing one: the answer for B's id equals the
//!    answer for a never-issued id, once the id is substituted back;
//! 3. a write changes none of B's rows;
//! 4. no change event names team B.
//!
//! `every_mcp_tool_has_a_cross_team_case` fails for a tool with no case, the
//! way `every_registered_tool_has_a_run_scope_decision` fails for one with no
//! run-scope decision (ADR-0021 point 3).
//!
//! # Two more doors
//!
//! A run-scoped handle is served under its task's one team even when the
//! server's own context reaches both, so every tool a grant allows answers B's
//! ids exactly as it answers never-issued ones. The board port's half lives in
//! `testing/board_contract.rs`, where task 052's HTTP adapter runs it too.
//!
//! # And the structure that keeps it true
//!
//! `no_service_takes_a_pool_without_a_scope` scans every function signature in
//! `crates/core/src/`: a service that took a pool could start a transaction no
//! scope ever sees.
//! `each_split_settings_table_has_one_writer` scans the statements: a second
//! writer of `team_settings` would be a write the checks tasks 045 and 051
//! hang on `set_team` never see.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use rimaia_core::mcp::{self, Grant, GrantKind, McpHandle, RunAccess, Tool};
use rimaia_core::schedule::{self, ScheduleInput};
use rimaia_core::testing::teams::{TwoTeams, SENTINEL};
use rimaia_core::{db::ScheduleMode, ServiceContext};
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// The case table
// ---------------------------------------------------------------------------

/// What a case does to the board, which decides whether point 3 applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    Reads,
    Writes,
}

/// One tool's cross-team case.
struct Case {
    tool: &'static str,
    effect: Effect,
    /// The argument object, or `Value::Null` for a tool that takes none.
    args: fn(&Board) -> Value,
}

/// What a case's arguments can name: the fixture, and the two schedules the
/// schedule tools work on (schedules are machine state, not a team's).
struct Board {
    teams: TwoTeams,
    schedule: String,
    doomed_schedule: String,
}

const fn case(tool: &'static str, effect: Effect, args: fn(&Board) -> Value) -> Case {
    Case { tool, effect, args }
}

fn schedule_config() -> Value {
    json!({
        "name": "Nightly",
        "mode": "parallel",
        "max_concurrency": 2,
        "timezone": "UTC",
        "cron": "0 22 * * *",
        "enabled": false,
    })
}

/// Every MCP tool, as team A, aimed at team B's rows wherever the tool takes
/// an id.
fn cases() -> Vec<Case> {
    use Effect::{Reads, Writes};
    vec![
        case(
            "add_task_link",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.ready, "label": "a link", "url": "https://example.invalid/a" }),
        ),
        case(
            "create_task",
            Writes,
            |b| json!({ "repository_id": b.teams.team_b.repository.id, "title": "planted", "plan": "1. Plant it" }),
        ),
        case("get_base_instructions", Reads, |_| Value::Null),
        case(
            "get_task",
            Reads,
            |b| json!({ "task_id": b.teams.team_b.ready }),
        ),
        case("list_repositories", Reads, |_| Value::Null),
        case("list_tasks", Reads, |_| json!({})),
        case(
            "move_task",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.ready, "column": "not_ready" }),
        ),
        case(
            "remove_task_link",
            Writes,
            |b| json!({ "link_id": b.teams.team_b.link }),
        ),
        case(
            "set_task_dependencies",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.in_review, "depends_on": [b.teams.team_b.done] }),
        ),
        case(
            "set_task_strategy",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.ready, "model": "a-model", "phases": [] }),
        ),
        case(
            "update_task",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.ready, "title": "renamed" }),
        ),
        case(
            "accept_task_strategy",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.ready }),
        ),
        case(
            "clear_task_strategy",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.ready }),
        ),
        case("get_strategy_approval", Reads, |_| Value::Null),
        case("get_strategy_catalogue", Reads, |_| Value::Null),
        case(
            "get_strategy_defaults",
            Reads,
            |b| json!({ "repository_id": b.teams.team_b.repository.id }),
        ),
        case(
            "set_strategy_approval",
            Writes,
            |_| json!({ "approval": "manual" }),
        ),
        case(
            "set_strategy_catalogue",
            Writes,
            |_| json!({ "catalogue": r#"{"models":[]}"# }),
        ),
        case(
            "set_strategy_defaults",
            Writes,
            |b| json!({ "repository_id": b.teams.team_b.repository.id, "mode": "manual", "model": "a-model" }),
        ),
        case("get_run_capacity", Reads, |_| Value::Null),
        case(
            "set_schedule_mode",
            Writes,
            |_| json!({ "mode": "parallel" }),
        ),
        case(
            "set_max_concurrency",
            Writes,
            |_| json!({ "max_concurrency": 2 }),
        ),
        case(
            "set_repository_max_concurrency",
            Writes,
            |b| json!({ "repository_id": b.teams.team_b.repository.id, "max_concurrency": 2 }),
        ),
        case(
            "give_up_on_task",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.ready }),
        ),
        case("run_doctor", Reads, |_| Value::Null),
        case("dismiss_onboarding", Writes, |_| Value::Null),
        case(
            "dismiss_doctor_warning",
            Writes,
            |_| json!({ "check": "git", "repository": null, "detail": "a warning" }),
        ),
        case(
            "restore_doctor_warning",
            Writes,
            |_| json!({ "check": "git", "repository": null, "detail": "a warning" }),
        ),
        case(
            "plan_task_strategy",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.ready }),
        ),
        case(
            "plan_tasks_strategy",
            Writes,
            |b| json!({ "task_ids": [b.teams.team_b.ready] }),
        ),
        case(
            "get_repository_credential_status",
            Reads,
            |b| json!({ "repository_id": b.teams.team_b.repository.id }),
        ),
        case("get_analytics", Reads, |_| json!({})),
        case("get_subscription_cost", Reads, |_| Value::Null),
        case(
            "set_subscription_cost",
            Writes,
            |_| json!({ "monthly_usd": 25.0 }),
        ),
        case("list_schedules", Reads, |_| Value::Null),
        case("create_schedule", Writes, |_| schedule_config()),
        case("update_schedule", Writes, |b| {
            let mut args = schedule_config();
            args["schedule_id"] = json!(b.schedule);
            args
        }),
        case(
            "set_schedule_enabled",
            Writes,
            |b| json!({ "schedule_id": b.schedule, "enabled": false }),
        ),
        case(
            "delete_schedule",
            Writes,
            |b| json!({ "schedule_id": b.doomed_schedule }),
        ),
        case(
            "preview_schedule_preflight",
            Reads,
            |b| json!({ "schedule_id": b.schedule }),
        ),
        case("list_timezones", Reads, |_| Value::Null),
        case("list_worktrees", Reads, |_| Value::Null),
        case("get_worktree_auto_cleanup", Reads, |_| Value::Null),
        case(
            "set_worktree_auto_cleanup",
            Writes,
            |_| json!({ "setting": "off" }),
        ),
        case(
            "archive_task",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.ready }),
        ),
        case(
            "archive_tasks",
            Writes,
            |b| json!({ "task_ids": [b.teams.team_b.ready, b.teams.team_b.not_ready] }),
        ),
        case(
            "unarchive_task",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.archived }),
        ),
        case(
            "set_repository_on_archive",
            Writes,
            |b| json!({ "repository_id": b.teams.team_b.repository.id, "on_archive": "none" }),
        ),
        case(
            "approve_task",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.in_review }),
        ),
        case(
            "reject_task",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.in_review, "note": "Not this." }),
        ),
        case(
            "request_task_changes",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.in_review, "note": "More tests." }),
        ),
        case(
            "get_task_dependents",
            Reads,
            |b| json!({ "task_id": b.teams.team_b.done }),
        ),
        case("get_review_digest", Reads, |_| Value::Null),
        case(
            "mark_review_digest_seen",
            Writes,
            |_| json!({ "through": "2026-08-20T01:00:00Z" }),
        ),
        case(
            "record_review_findings",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.in_review, "findings": [] }),
        ),
        // Team A's own task, so a fix handle on it passes `authorize` and the
        // finding id is what the service has to refuse.
        case("resolve_review_finding", Writes, |b| {
            json!({
                "task_id": b.teams.team_a.in_review,
                "finding_id": b.teams.team_b.findings[0],
                "status": "fixed",
            })
        }),
        case(
            "list_review_findings",
            Reads,
            |b| json!({ "task_id": b.teams.team_b.in_review }),
        ),
        case("get_review_settings", Reads, |_| Value::Null),
        case(
            "set_review_settings",
            Writes,
            |_| json!({ "instructions": "Review it.", "config": null }),
        ),
        case(
            "set_repository_review_config",
            Writes,
            |b| json!({ "repository_id": b.teams.team_b.repository.id, "config": null }),
        ),
        case(
            "set_task_review",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.in_review, "config": null }),
        ),
        case(
            "get_review_history",
            Reads,
            |b| json!({ "task_id": b.teams.team_b.in_review }),
        ),
        case(
            "get_review_level",
            Reads,
            |b| json!({ "level": "task", "id": b.teams.team_b.in_review }),
        ),
    ]
}

#[test]
fn every_mcp_tool_has_a_cross_team_case() {
    let declared: BTreeSet<&str> = Tool::ALL.iter().map(|tool| tool.as_str()).collect();
    let cased: Vec<&str> = cases().iter().map(|case| case.tool).collect();
    let unique: BTreeSet<&str> = cased.iter().copied().collect();

    let missing: Vec<&str> = declared.difference(&unique).copied().collect();
    assert_eq!(
        missing,
        Vec::<&str>::new(),
        "an MCP tool with no cross-team case: add one to `cases()` in \
         crates/core/tests/tenant_isolation.rs"
    );
    let naming_nothing: Vec<&str> = unique.difference(&declared).copied().collect();
    assert_eq!(
        naming_nothing,
        Vec::<&str>::new(),
        "a case for a tool that does not exist"
    );
    assert_eq!(cased.len(), unique.len(), "a tool with two cases");
}

// ---------------------------------------------------------------------------
// The operator's door
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_team_cannot_see_another_teams_ids_through_any_tool() {
    let mut board = board().await;
    let (handle, server) = serving(&board.teams, &board.teams.a).await;
    let client = connect(&operator_url(&handle)).await;
    let team_a = board.teams.team_a.team_id.clone();
    let b_ids = board.teams.team_b.ids();

    let mut failures: Vec<String> = Vec::new();
    for case in cases() {
        let args = (case.args)(&board);
        let sent: Vec<String> = b_ids
            .iter()
            .filter(|id| mentions(&args, id))
            .cloned()
            .collect();

        let before = match case.effect {
            Effect::Writes => Some(board.teams.snapshot_b().await),
            Effect::Reads => None,
        };
        board.teams.drain_changes();
        let answer = invoke(&client, case.tool, &args).await;
        let events = board.teams.drain_changes();

        // 1. Nothing leaks: an id the caller sent may come back in a refusal;
        //    any other of B's ids, or the sentinel, is a leak.
        for id in b_ids.iter().filter(|id| !sent.contains(id)) {
            if answer.contains(id.as_str()) {
                failures.push(format!(
                    "{}: point 1, the answer names team B's {id}: {answer}",
                    case.tool
                ));
            }
        }
        if answer.contains(SENTINEL) {
            failures.push(format!(
                "{}: point 1, the answer carries the sentinel: {answer}",
                case.tool
            ));
        }

        // 3. A write changed none of B's rows.
        if let Some(before) = before {
            let after = board.teams.snapshot_b().await;
            if after != before {
                failures.push(format!(
                    "{}: point 3, team B's rows changed\nbefore:\n{before}\nafter:\n{after}",
                    case.tool
                ));
            }
        }

        // 4. Every event names team A.
        for event in events.iter().filter(|event| event.team_id != team_a) {
            failures.push(format!(
                "{}: point 4, an event names another team: {event:?}",
                case.tool
            ));
        }

        // 2. A foreign id reads as a missing one.
        if !sent.is_empty() {
            let substitutions: Vec<(String, String)> = sent
                .iter()
                .map(|id| (id.clone(), TwoTeams::never_issued()))
                .collect();
            let missing_args = substitute(&args, &substitutions);
            let missing = invoke(&client, case.tool, &missing_args).await;
            let restored = substitutions
                .iter()
                .fold(missing, |text, (foreign, fresh)| {
                    text.replace(fresh, foreign)
                });
            if restored != answer {
                failures.push(format!(
                    "{}: point 2, a foreign id is answered differently from a missing one\n\
                     foreign: {answer}\n missing: {restored}",
                    case.tool
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} failure(s):\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    assert_eq!(
        board.teams.cli.started(),
        Vec::<String>::new(),
        "no case spawned anything"
    );
    board.teams.cli.assert_nothing_fell_through();

    let _ = client.cancel().await;
    handle.shutdown();
    server.await.expect("the server task ends");
}

// ---------------------------------------------------------------------------
// The run-scoped handle (D30 point 5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_run_scoped_handle_cannot_reach_another_teams_ids() {
    let board = board().await;
    // The server's own context reaches both teams: a runner that serves
    // several. The handle must still see only its task's team.
    let (handle, server) = serving(&board.teams, &board.teams.both).await;
    let b_ids = board.teams.team_b.ids();
    let table = cases();

    let mut failures: Vec<String> = Vec::new();
    for kind in GrantKind::ALL {
        let grant = grant_for(&board.teams, kind);
        let client = connect(&board.teams.handles.endpoint_for(&grant).expect("bound")).await;

        for tool in Tool::ALL {
            if tool.run_access(kind) == RunAccess::Refused {
                continue;
            }
            let case = table
                .iter()
                .find(|case| case.tool == tool.as_str())
                .expect("every tool has a case");
            let args = (case.args)(&board);
            let sent: Vec<String> = b_ids
                .iter()
                .filter(|id| mentions(&args, id))
                .cloned()
                .collect();
            if sent.is_empty() {
                // An id-less tool: `a_run_scoped_handle_lists_only_its_own_team`.
                continue;
            }

            let answer = invoke(&client, tool.as_str(), &args).await;
            let substitutions: Vec<(String, String)> = sent
                .iter()
                .map(|id| (id.clone(), TwoTeams::never_issued()))
                .collect();
            let missing = invoke(&client, tool.as_str(), &substitute(&args, &substitutions)).await;
            let restored = substitutions
                .iter()
                .fold(missing, |text, (foreign, fresh)| {
                    text.replace(fresh, foreign)
                });

            if restored != answer {
                failures.push(format!(
                    "{} under a {kind:?} grant: a foreign id is answered differently\n\
                     foreign: {answer}\n missing: {restored}",
                    tool.as_str()
                ));
            }
            if answer.contains(SENTINEL) {
                failures.push(format!(
                    "{} under a {kind:?} grant: the answer carries the sentinel: {answer}",
                    tool.as_str()
                ));
            }
        }
        let _ = client.cancel().await;
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert_eq!(board.teams.cli.started(), Vec::<String>::new());
    handle.shutdown();
    server.await.expect("the server task ends");
}

#[tokio::test]
async fn a_run_scoped_handle_lists_only_its_own_team() {
    let board = board().await;
    let (handle, server) = serving(&board.teams, &board.teams.both).await;
    let team_a = &board.teams.team_a;

    for kind in GrantKind::ALL {
        let grant = grant_for(&board.teams, kind);
        let client = connect(&board.teams.handles.endpoint_for(&grant).expect("bound")).await;

        let repositories = invoke(&client, "list_repositories", &Value::Null).await;
        assert!(
            repositories.contains(&team_a.repository.id),
            "{kind:?}: team A's repository is listed: {repositories}"
        );
        let instructions = invoke(&client, "get_base_instructions", &Value::Null).await;
        assert!(
            instructions.contains(team_a.setting(rimaia_core::db::settings::BASE_INSTRUCTIONS)),
            "{kind:?}: team A's own instructions: {instructions}"
        );

        for answer in [&repositories, &instructions] {
            assert!(!answer.contains(SENTINEL), "{kind:?}: {answer}");
            for id in board.teams.team_b.ids() {
                assert!(!answer.contains(&id), "{kind:?}: team B's {id}: {answer}");
            }
        }
        let _ = client.cancel().await;
    }

    handle.shutdown();
    server.await.expect("the server task ends");
}

// ---------------------------------------------------------------------------
// The structure: no service reaches the store without a scope
// ---------------------------------------------------------------------------

/// The functions allowed a store handle in their signature, each with why. A
/// later task that needs one appends an entry, with its reason, in the same
/// commit.
///
/// A `ScopedTx` counts as a transaction here. It carries the context's scope,
/// which is why the helpers below may take one, but each of them is still a
/// door into someone else's transaction, so each is named.
const STORE_HANDLE_EXCEPTIONS: [(&str, &str); 18] = [
    ("db::connect", "it makes the pool; no context can exist yet"),
    ("db::migrate", "it runs before the context is built"),
    (
        "db::apply_migrations",
        "it runs before the context is built",
    ),
    (
        "identity::ensure_solo",
        "it returns the scope the context is built from",
    ),
    (
        "identity::create_personal_team",
        "it writes inside the caller's transaction, for ensure_solo and 047's sign-up (038)",
    ),
    (
        "context::ServiceContext::new",
        "it takes the pool the context wraps",
    ),
    (
        "db::settings::get_user_in",
        "it reads the actor's row inside a review action's or mark_seen's transaction; it takes \
         the context for the actor and checks the key's placement",
    ),
    (
        "db::settings::set_user_in",
        "it writes the actor's row inside a review action's or mark_seen's transaction; it takes \
         the context for the actor and checks the key's placement",
    ),
    (
        "db::settings::set_team_in",
        "it writes a team's row inside a save that writes two keys as one (the review loop's \
         instructions and configuration) and deletes one inside a repository's removal; it takes \
         the context for the scope and checks the key's placement",
    ),
    (
        "context::ServiceContext::begin",
        "it opens the transaction a service shares with its helpers, carrying the context's scope",
    ),
    (
        "context::ServiceContext::begin_immediate",
        "it opens that transaction as BEGIN IMMEDIATE, for a read the write after it depends on",
    ),
    (
        "repo::team_of_repository",
        "a task's create and repository move resolve the repository's team inside their own \
         transaction; it filters by that transaction's scope",
    ),
    (
        "review::digest::advance_marker",
        "a verdict advances the actor's marker inside its own transaction, so the column move and \
         the marker commit together (034); it is set_user_in's caller",
    ),
    (
        "tasks::dependencies::dependents_in",
        "deleting a task and a review action read its dependents inside the transaction that read \
         the task; it filters by that transaction's scope",
    ),
    (
        "tasks::position::rebalance_column",
        "a move renumbers its column inside its own transaction, or a failure part-way reorders \
         the column (its doc comment); the caller has already scoped the repository",
    ),
    (
        "tasks::service::move_within",
        "a review action writes its note and moves the card in one transaction (034)",
    ),
    (
        "tasks::service::fetch_task_row",
        "every task write reads the row it changes inside its own transaction; it filters by that \
         transaction's scope and answers a foreign id as a missing one",
    ),
    (
        "tasks::service::team_of_task",
        "every write that names a task resolves the task's team inside its own transaction, for \
         its event; it filters by that transaction's scope",
    ),
];

#[test]
fn no_service_takes_a_pool_without_a_scope() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();
    let mut excepted = BTreeSet::new();

    for file in rust_files(&root) {
        let relative = file.strip_prefix(&root).expect("under src");
        if relative.starts_with("testing") {
            continue;
        }
        let module = module_path(relative);
        let source = std::fs::read_to_string(&file).expect("read a source file");
        for signature in signatures(&without_test_modules(&blank(&source))) {
            let path = match &signature.owner {
                Some(owner) => format!("{module}::{owner}::{}", signature.name),
                None => format!("{module}::{}", signature.name),
            };
            let path = path.trim_start_matches("::").to_string();

            let takes_a_pool = has_token(&signature.text, &["SqlitePool", "Pool<"]);
            let takes_a_connection = has_token(
                &signature.text,
                &[
                    "SqliteConnection",
                    "PoolConnection",
                    "Transaction",
                    "ScopedTx",
                    "Executor",
                    "SqliteExecutor",
                    "Acquire",
                ],
            );
            let offends = takes_a_pool || (signature.visible && takes_a_connection);
            if !offends {
                continue;
            }
            if STORE_HANDLE_EXCEPTIONS
                .iter()
                .any(|(excepted_path, _)| *excepted_path == path)
            {
                excepted.insert(path);
                continue;
            }
            violations.push(format!(
                "{}: {path} takes {}",
                relative.display(),
                if takes_a_pool {
                    "a pool"
                } else {
                    "a bare connection, and is visible outside its module"
                }
            ));
        }
    }

    assert_eq!(
        violations,
        Vec::<String>::new(),
        "take `&ServiceContext`, or keep a helper that shares a transaction private to its \
         module; an exception needs an entry in STORE_HANDLE_EXCEPTIONS with its reason"
    );
    let stale: Vec<&str> = STORE_HANDLE_EXCEPTIONS
        .iter()
        .map(|(path, _)| *path)
        .filter(|path| !excepted.contains(*path))
        .collect();
    assert_eq!(
        stale,
        Vec::<&str>::new(),
        "an exception that matches nothing any more"
    );
}

/// The functions allowed a statement that writes a split settings table, each
/// with why. Task 039 makes `set_team_in` and `set_user_in` the one writer of
/// their table, so tasks 045 and 051 can hang their checks on it and a key
/// added later inherits them; a second writer would be a write those checks
/// never see.
const SETTINGS_WRITERS: [(&str, &str, &str); 3] = [
    (
        "team_settings",
        "db::settings::set_team_in",
        "the one writer: it sets a team key and, with `None`, removes it; set_team is it run \
         over the pool",
    ),
    (
        "team_settings",
        "identity::create_personal_team",
        "the seed row that comes into being with its team (D28 part 3); no set_team can run \
         before the team exists",
    ),
    (
        "user_settings",
        "db::settings::set_user_in",
        "the one writer; set_user is it run over the pool",
    ),
];

#[test]
fn each_split_settings_table_has_one_writer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut writers = BTreeSet::new();

    for file in rust_files(&root) {
        let relative = file.strip_prefix(&root).expect("under src");
        if relative.starts_with("testing") {
            continue;
        }
        let module = module_path(relative);
        let raw: Vec<char> = std::fs::read_to_string(&file)
            .expect("read a source file")
            .chars()
            .collect();
        let blanked: Vec<char> = blank(&raw.iter().collect::<String>()).chars().collect();
        let live: Vec<char> = without_test_modules(&blanked.iter().collect::<String>())
            .chars()
            .collect();
        assert_eq!(
            raw.len(),
            live.len(),
            "blanking moved {}",
            relative.display()
        );
        let lowered: String = raw.iter().collect::<String>().to_lowercase();
        let lowered: Vec<char> = lowered.chars().collect();
        assert_eq!(
            raw.len(),
            lowered.len(),
            "lowercasing moved {}",
            relative.display()
        );

        for at in 0..raw.len() {
            let Some(table) = writes_settings_table(&lowered, at) else {
                continue;
            };
            // A literal's opening quote survives blanking, where a comment
            // leaves nothing, and is blanked again only in a test module.
            let Some(quote) = (0..at).rev().find(|&i| !blanked[i].is_whitespace()) else {
                continue;
            };
            let in_a_literal = matches!(blanked[quote], '"' | 'r');
            if !in_a_literal || live[quote] != blanked[quote] {
                continue;
            }
            let function = enclosing_fn(&live, quote).unwrap_or_default();
            let path = format!("{module}::{function}");
            writers.insert((table, path.trim_start_matches("::").to_string()));
        }
    }

    let expected: BTreeSet<(&str, String)> = SETTINGS_WRITERS
        .iter()
        .map(|(table, path, _)| (*table, path.to_string()))
        .collect();
    assert_eq!(
        writers, expected,
        "write team_settings through db::settings::set_team_in and user_settings through \
         set_user_in; an exception needs an entry in SETTINGS_WRITERS with its reason"
    );
}

/// The settings table a write statement starting at `at` names, if one does:
/// `insert into`, `replace into`, `update` or `delete from`, then the table.
fn writes_settings_table(lowered: &[char], at: usize) -> Option<&'static str> {
    let starts_word = at == 0 || !(lowered[at - 1].is_alphanumeric() || lowered[at - 1] == '_');
    if !starts_word {
        return None;
    }
    let rest: String = lowered[at..lowered.len().min(at + 80)].iter().collect();
    let words: Vec<&str> = rest.split_whitespace().collect();
    let table = match words.as_slice() {
        ["insert" | "replace", "into", table, ..]
        | ["insert", "or", _, "into", table, ..]
        | ["delete", "from", table, ..]
        | ["update", table, ..] => *table,
        _ => return None,
    };
    let table = table.trim_end_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
    ["team_settings", "user_settings"]
        .into_iter()
        .find(|known| *known == table)
}

/// The name of the last `fn` declared before `position` in a blanked source:
/// the function a statement sits in, since no writer nests one inside another.
fn enclosing_fn(chars: &[char], position: usize) -> Option<String> {
    (0..position.saturating_sub(2)).rev().find_map(|i| {
        let is_fn = chars[i] == 'f'
            && chars[i + 1] == 'n'
            && chars[i + 2].is_whitespace()
            && (i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_'));
        is_fn.then(|| {
            chars[i + 3..]
                .iter()
                .skip_while(|c| c.is_whitespace())
                .take_while(|c| c.is_alphanumeric() || **c == '_')
                .collect()
        })
    })
}

/// One `fn` signature, through its opening brace or semicolon.
struct Signature {
    name: String,
    /// The `impl` it sits directly in, by type name.
    owner: Option<String>,
    /// `pub`, `pub(crate)`, `pub(super)` or `pub(in …)`.
    visible: bool,
    text: String,
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).expect("read a source directory") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    files.sort();
    files
}

/// `db/settings.rs` is `db::settings`, `db/mod.rs` is `db`, `lib.rs` is the
/// crate root.
fn module_path(relative: &Path) -> String {
    let mut segments: Vec<String> = relative
        .with_extension("")
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    if matches!(segments.last().map(String::as_str), Some("mod" | "lib")) {
        segments.pop();
    }
    segments.join("::")
}

/// The source with every comment, string and character literal's contents
/// replaced by spaces, so braces and keywords inside them count for nothing.
/// Newlines survive, so nothing moves.
fn blank(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    let keep_newlines = |c: char| if c == '\n' { '\n' } else { ' ' };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    out.push_str("  ");
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    out.push_str("  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(keep_newlines(chars[i]));
                    i += 1;
                }
            }
        } else if c == 'r'
            && (next == Some('"') || next == Some('#'))
            && !chars
                .get(i.wrapping_sub(1))
                .is_some_and(|before| before.is_alphanumeric() || *before == '_')
        {
            // A raw string: r"…", r#"…"#, with as many hashes as it opened.
            let mut j = i + 1;
            let mut hashes = 0;
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j) != Some(&'"') {
                out.push(c);
                i += 1;
                continue;
            }
            out.push('r');
            out.extend(std::iter::repeat_n(' ', j - i));
            i = j + 1;
            loop {
                if i >= chars.len() {
                    break;
                }
                if chars[i] == '"' && (0..hashes).all(|k| chars.get(i + 1 + k) == Some(&'#')) {
                    out.extend(std::iter::repeat_n(' ', 1 + hashes));
                    i += 1 + hashes;
                    break;
                }
                out.push(keep_newlines(chars[i]));
                i += 1;
            }
        } else if c == '"' {
            out.push('"');
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' {
                    out.push(' ');
                    i += 1;
                }
                if i < chars.len() {
                    out.push(keep_newlines(chars[i]));
                    i += 1;
                }
            }
            out.push('"');
            i += 1;
        } else if c == '\'' {
            // A character literal, or a lifetime, which has no closing quote.
            if next == Some('\\') {
                out.push('\'');
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    let width = if chars[i] == '\\' { 2 } else { 1 };
                    out.extend(std::iter::repeat_n(' ', width));
                    i += width;
                }
                out.push('\'');
                i += 1;
            } else if chars.get(i + 2) == Some(&'\'') {
                out.push_str("' '");
                i += 3;
            } else {
                out.push(c);
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// The blanked source with every `#[cfg(test)]` module and function emptied.
fn without_test_modules(blanked: &str) -> String {
    let mut text: Vec<char> = blanked.chars().collect();
    let marker: Vec<char> = "#[cfg(test)]".chars().collect();
    let mut i = 0;
    while i + marker.len() <= text.len() {
        if text[i..i + marker.len()] != marker[..] {
            i += 1;
            continue;
        }
        // The item the attribute is on runs to its body's closing brace.
        let Some(open) = (i..text.len()).find(|&j| text[j] == '{' || text[j] == ';') else {
            break;
        };
        if text[open] == ';' {
            i = open + 1;
            continue;
        }
        let mut depth = 0;
        let mut end = open;
        for (j, c) in text.iter().enumerate().skip(open) {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = j;
                        break;
                    }
                }
                _ => {}
            }
        }
        for c in &mut text[i..=end] {
            if *c != '\n' {
                *c = ' ';
            }
        }
        i = end + 1;
    }
    text.into_iter().collect()
}

/// Every `fn` signature in a blanked source.
fn signatures(blanked: &str) -> Vec<Signature> {
    let chars: Vec<char> = blanked.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i + 3 <= chars.len() {
        let is_fn = chars[i] == 'f'
            && chars[i + 1] == 'n'
            && chars[i + 2].is_whitespace()
            && (i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_'));
        if !is_fn {
            i += 1;
            continue;
        }

        let name: String = chars[i + 3..]
            .iter()
            .skip_while(|c| c.is_whitespace())
            .take_while(|c| c.is_alphanumeric() || **c == '_')
            .collect();
        // Through the opening brace or the semicolon, at bracket depth zero:
        // `[u8; 4]` in a parameter has a semicolon of its own.
        let mut depth = 0i32;
        let mut end = i;
        for (j, c) in chars.iter().enumerate().skip(i) {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                '{' | ';' if depth == 0 => {
                    end = j;
                    break;
                }
                _ => {}
            }
        }
        let text: String = chars[i..=end].iter().collect();

        let before: String = chars[i.saturating_sub(80)..i].iter().collect();
        let prefix = before
            .rsplit(['\n', ';', '{', '}', ']'])
            .next()
            .unwrap_or_default()
            .trim();
        let visible = prefix.starts_with("pub");

        found.push(Signature {
            name,
            owner: enclosing_impl(&chars, i),
            visible,
            text,
        });
        i = end.max(i + 1);
    }
    found
}

/// The type of the `impl` block a position sits directly inside, if any.
fn enclosing_impl(chars: &[char], position: usize) -> Option<String> {
    let mut depth = 0i32;
    let mut open = None;
    for j in (0..position).rev() {
        match chars[j] {
            '}' => depth += 1,
            '{' => {
                if depth == 0 {
                    open = Some(j);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    let open = open?;
    let start = (0..open)
        .rev()
        .find(|&j| matches!(chars[j], ';' | '}' | '{'))
        .map_or(0, |j| j + 1);
    let header: String = chars[start..open].iter().collect();
    let header = header
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("#["))
        .collect::<Vec<_>>()
        .join(" ");
    let header = header.trim();
    if !header.starts_with("impl") {
        return None;
    }
    let target = header.rsplit(" for ").next().unwrap_or(header);
    let target = target.trim_start_matches("impl").trim();
    // Past the impl's own generics, to the type's last path segment.
    let target = if target.starts_with('<') {
        let mut depth = 0;
        let mut cut = 0;
        for (k, c) in target.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        cut = k + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        target[cut..].trim()
    } else {
        target
    };
    let target = target.split(['<', ' ']).next().unwrap_or(target);
    Some(target.rsplit("::").next().unwrap_or(target).to_string())
}

/// Whether `text` holds any of `tokens` as a word (or, for one ending in `<`,
/// as that prefix).
fn has_token(text: &str, tokens: &[&str]) -> bool {
    tokens.iter().any(|token| {
        text.match_indices(token).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + token.len()..].chars().next();
            let word_start = !before.is_some_and(|c| c.is_alphanumeric() || c == '_');
            let word_end =
                token.ends_with('<') || !after.is_some_and(|c| c.is_alphanumeric() || c == '_');
            word_start && word_end
        })
    })
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// The fixture, plus two schedules for the schedule tools.
async fn board() -> Board {
    let teams = TwoTeams::new().await;
    let nightly = |name: &str| ScheduleInput {
        name: name.to_string(),
        mode: ScheduleMode::Sequential,
        max_concurrency: 2,
        timezone: "UTC".to_string(),
        cron: Some("0 22 * * *".to_string()),
        start_at: None,
        stop_at: Some("06:00".to_string()),
        enabled: false,
    };
    let schedule = schedule::create(&teams.machine, nightly("Nightly"))
        .await
        .expect("a schedule")
        .id;
    let doomed_schedule = schedule::create(&teams.machine, nightly("Doomed"))
        .await
        .expect("a schedule to delete")
        .id;
    Board {
        teams,
        schedule,
        doomed_schedule,
    }
}

/// A bound MCP server over `ctx`, on an OS-chosen port, already spawned.
async fn serving(
    teams: &TwoTeams,
    ctx: &ServiceContext,
) -> (McpHandle, tokio::task::JoinHandle<()>) {
    let (handle, task) = mcp::build(
        ctx.clone(),
        0,
        teams.handles.clone(),
        teams.doctor().provider,
        Some(teams.local(ctx)),
    )
    .await;
    (handle, tokio::spawn(task.run()))
}

fn operator_url(handle: &McpHandle) -> String {
    handle.url().expect("the server is listening")
}

async fn connect(url: &str) -> RunningService<RoleClient, ()> {
    ().serve(StreamableHttpClientTransport::with_client(
        reqwest::Client::default(),
        StreamableHttpClientTransportConfig::with_uri(url.to_string()),
    ))
    .await
    .expect("the server answers `initialize`")
}

/// A handle for team A's run of the grant's kind.
fn grant_for(teams: &TwoTeams, kind: GrantKind) -> mcp::RunGrant {
    let team_a = &teams.team_a;
    let (task_id, grant) = match kind {
        GrantKind::Strategy => (&team_a.ready, Grant::Strategy),
        GrantKind::Review => (
            &team_a.in_review,
            Grant::Review {
                run_id: team_a.review_run.clone(),
            },
        ),
        GrantKind::Fix => (
            &team_a.in_review,
            Grant::Fix {
                run_id: team_a.fix_run.clone(),
            },
        ),
    };
    teams.handles.grant(task_id, &team_a.team_id, grant)
}

/// One tool call by name, answered as the serialized result: a refusal and a
/// success are both an answer, and both are compared as text.
async fn invoke(
    client: &RunningService<RoleClient, ()>,
    tool: &'static str,
    args: &Value,
) -> String {
    let mut params = CallToolRequestParams::new(tool);
    if let Value::Object(arguments) = args {
        params = params.with_arguments(arguments.clone());
    }
    let result = client
        .call_tool(params)
        .await
        .unwrap_or_else(|error| panic!("{tool}: the call itself completes: {error}"));
    serde_json::to_string(&result).expect("a result serializes")
}

/// Whether any string in `args` is `id`.
fn mentions(args: &Value, id: &str) -> bool {
    match args {
        Value::String(text) => text == id,
        Value::Array(items) => items.iter().any(|item| mentions(item, id)),
        Value::Object(fields) => fields.values().any(|value| mentions(value, id)),
        _ => false,
    }
}

/// `args` with every string equal to a foreign id replaced by its stand-in.
fn substitute(args: &Value, substitutions: &[(String, String)]) -> Value {
    match args {
        Value::String(text) => Value::String(
            substitutions
                .iter()
                .find(|(foreign, _)| foreign == text)
                .map_or_else(|| text.clone(), |(_, fresh)| fresh.clone()),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| substitute(item, substitutions))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), substitute(value, substitutions)))
                .collect(),
        ),
        other => other.clone(),
    }
}
