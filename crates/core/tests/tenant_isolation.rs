//! One team cannot see another's board through any door (task 039, ADR-0029
//! point 5).
//!
//! # The command registry
//!
//! Task 046 keys the commands' half on `api::registry::COMMANDS`, and parses no
//! Rust source to find them. Every board row has one case in
//! `testing::api::BOARD_CASES`, the table `crates/server/tests/commands.rs`
//! runs over HTTP too, and here each runs through `api::dispatch` with team A's
//! owner as the caller: nothing of team B's in the answer, B's rows unchanged,
//! every event naming team A, a foreign id answered `not_found`, and a foreign
//! filter answered exactly as a never-issued id is.
//!
//! The local rows (task 049) have no board handler in core: each is run by
//! calling the core functions its shell handler calls, under team A's context
//! and `TwoTeams::machine`, in `LOCAL_CASES`.
//! `every_board_command_has_a_cross_team_case` holds both tables equal to the
//! registry.
//!
//! # The MCP tools
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
use rimaia_core::api::{self, Effect as ApiEffect, COMMANDS};
use rimaia_core::mcp::{self, Grant, GrantKind, McpHandle, RunAccess, Tool};
use rimaia_core::runner::provider::ProviderProfile;
use rimaia_core::schedule::{self, ScheduleInput};
use rimaia_core::testing::api::{
    self as testing_api, with_never_issued_token, Foreign, BOARD_CASES,
};
use rimaia_core::testing::teams::{TwoTeams, SENTINEL};

use rimaia_core::{db::ScheduleMode, Audience, ChangeEvent, ErrorCode, ServiceContext};
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
        // One machine holds both teams' clones here; team A is told only of
        // its own (task 066).
        case("list_checkouts", Reads, |_| Value::Null),
        case("get_worktree_auto_cleanup", Reads, |_| Value::Null),
        case(
            "set_worktree_auto_cleanup",
            Writes,
            |_| json!({ "setting": "off" }),
        ),
        // This machine's strategy ceiling: a runner setting, which no team
        // owns and none of team B's rows can reach (task 072).
        case("get_strategy_ceiling", Reads, |_| Value::Null),
        case(
            "set_strategy_ceiling",
            Writes,
            |_| json!({ "models": ["sonnet"], "max_effort": "medium" }),
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
        // Task 045. Team A's own task, so the membership check is what has
        // to refuse team B's owner as an assignee.
        case(
            "assign_task",
            Writes,
            |b| json!({ "task_id": b.teams.team_a.ready, "assignee_id": b.teams.team_b.owner_id }),
        ),
        case("accept_content", Writes, |b| {
            json!({
                "team_id": b.teams.team_b.team_id,
                "task_id": b.teams.team_b.ready,
                "kind": "plan",
                "revision": "1",
            })
        }),
        case(
            "set_repository_unattended_ceiling",
            Writes,
            |b| json!({ "repository_id": b.teams.team_b.repository.id, "allowed": true }),
        ),
        case(
            "get_task_consent",
            Reads,
            |b| json!({ "task_id": b.teams.team_b.ready, "runner_id": b.teams.team_a.runner_id }),
        ),
        // Task 051. Each team's guest is the member an owner acts on; the
        // token tools present B's expired token, and the loop tries the
        // revoked and accepted ones too.
        case("create_team", Writes, |_| json!({ "name": "Acme" })),
        case(
            "list_team_members",
            Reads,
            |b| json!({ "team_id": b.teams.team_b.team_id }),
        ),
        case(
            "set_member_role",
            Writes,
            |b| json!({ "team_id": b.teams.team_b.team_id, "user_id": b.teams.team_b.guest_id, "role": "owner" }),
        ),
        case(
            "remove_member",
            Writes,
            |b| json!({ "team_id": b.teams.team_b.team_id, "user_id": b.teams.team_b.guest_id }),
        ),
        case(
            "leave_team",
            Writes,
            |b| json!({ "team_id": b.teams.team_b.team_id }),
        ),
        case(
            "create_invitation",
            Writes,
            |b| json!({ "team_id": b.teams.team_b.team_id, "role": "owner" }),
        ),
        case(
            "list_invitations",
            Reads,
            |b| json!({ "team_id": b.teams.team_b.team_id }),
        ),
        case(
            "revoke_invitation",
            Writes,
            |b| json!({ "invitation_id": b.teams.team_b.invitations.pending_id }),
        ),
        case(
            "preview_invitation",
            Reads,
            |b| json!({ "token": b.teams.team_b.invitations.expired.expose() }),
        ),
        case(
            "accept_invitation",
            Writes,
            |b| json!({ "token": b.teams.team_b.invitations.expired.expose() }),
        ),
        case("list_copy_targets", Reads, |_| Value::Null),
        case(
            "copy_task_to_team",
            Writes,
            |b| json!({ "task_id": b.teams.team_b.ready, "repository_id": b.teams.team_a.repository.id }),
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
// The command registry (task 046)
// ---------------------------------------------------------------------------

#[test]
fn every_board_command_has_a_cross_team_case() {
    // `(name, writes)`, so a case whose effect differs from its row's is a
    // row with no case and a case with no row.
    let board_rows: BTreeSet<(&str, bool)> = COMMANDS
        .iter()
        .filter_map(|command| {
            command
                .effect()
                .map(|effect| (command.name, effect == ApiEffect::Write))
        })
        .collect();
    let cased: BTreeSet<(&str, bool)> = BOARD_CASES
        .iter()
        .map(|case| (case.name, case.effect == ApiEffect::Write))
        .collect();

    assert_eq!(
        board_rows.difference(&cased).collect::<Vec<_>>(),
        Vec::<&(&str, bool)>::new(),
        "a board row with no case, or with a case of another effect: add or fix it in \
         crates/core/src/testing/api.rs's BOARD_CASES"
    );
    assert_eq!(
        cased.difference(&board_rows).collect::<Vec<_>>(),
        Vec::<&(&str, bool)>::new(),
        "a case for no board row"
    );
    assert_eq!(BOARD_CASES.len(), cased.len(), "a board row with two cases");

    // The local rows' half (task 049's first commit, task 046's size cut): the
    // same two-way comparison, so a row of either kind without a case fails
    // here, and a case without a row does too.
    let local_rows: BTreeSet<&str> = COMMANDS
        .iter()
        .filter(|command| !command.is_board())
        .map(|command| command.name)
        .collect();
    let local_cased: Vec<&str> = LOCAL_CASES.iter().map(|case| case.name).collect();
    let local_unique: BTreeSet<&str> = local_cased.iter().copied().collect();
    assert_eq!(
        local_rows.difference(&local_unique).collect::<Vec<_>>(),
        Vec::<&&str>::new(),
        "a local row with no case: add one to LOCAL_CASES in \
         crates/core/tests/tenant_isolation.rs"
    );
    assert_eq!(
        local_unique.difference(&local_rows).collect::<Vec<_>>(),
        Vec::<&&str>::new(),
        "a local case for no local row"
    );
    assert_eq!(
        local_cased.len(),
        local_unique.len(),
        "a local row with two cases"
    );
    let unexplained: Vec<&str> = LOCAL_CASES
        .iter()
        .filter(|case| matches!(case.kind, Local::NoCoreCall("")))
        .map(|case| case.name)
        .collect();
    assert_eq!(
        unexplained,
        Vec::<&str>::new(),
        "a local case that calls no core function must say why"
    );
}

#[tokio::test]
async fn a_team_cannot_see_another_teams_ids_through_any_board_command() {
    let mut teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let team_a = teams.team_a.team_id.clone();
    let caller = testing_api::as_desktop(&teams.team_a);

    let mut failures: Vec<String> = Vec::new();
    for case in BOARD_CASES {
        let arg_sets = match &case.foreign {
            Foreign::EntityLess => vec![(case.own)(&teams)],
            foreign => foreign.arg_sets(&teams),
        };
        for args in arg_sets {
            let before = teams.snapshot_b().await;
            let teams_before = team_ids(&teams.a).await;
            teams.drain_changes();
            let answer = api::dispatch(&host, caller.clone(), case.name, args.clone()).await;
            let events = teams.drain_changes();
            let text = answer_text(&answer);

            // 1. Nothing of B's leaks.
            for leaked in testing_api::leaks(&teams.team_b, &args, &text) {
                failures.push(format!(
                    "{}: the answer carries B's {leaked}: {text}",
                    case.name
                ));
            }
            // 3. B's rows are unchanged.
            let after = teams.snapshot_b().await;
            if after != before {
                failures.push(format!(
                    "{}: team B's rows changed\nbefore:\n{before}\nafter:\n{after}",
                    case.name
                ));
            }
            // 4. Every event is for team A, for the acting user alone, or for
            //    a team the call itself created.
            for event in events.iter().filter(|event| {
                !for_team_or_actor(event, &team_a, &teams.team_a.owner_id)
                    && !for_a_new_team(event, &teams_before)
            }) {
                failures.push(format!(
                    "{}: an event is for another team or user: {event:?}",
                    case.name
                ));
            }
            // 2. A foreign id reads as a missing one.
            match &case.foreign {
                Foreign::Ids(_) => {
                    if answer.as_ref().err().map(|error| error.code()) != Some(ErrorCode::NotFound)
                    {
                        failures.push(format!("{}: B's ids are not not_found: {text}", case.name));
                    }
                }
                Foreign::Filter(_) => {
                    let (fresh_args, substitutions) =
                        testing_api::with_never_issued_ids(&teams.team_b, &args);
                    let fresh = api::dispatch(&host, caller.clone(), case.name, fresh_args).await;
                    let restored = testing_api::restore_ids(&answer_text(&fresh), &substitutions);
                    if restored != text {
                        failures.push(format!(
                            "{}: a foreign filter is answered differently from a missing one\n\
                             foreign: {text}\n missing: {restored}",
                            case.name
                        ));
                    }
                }
                // A token of B's nobody can use reads as one never issued.
                Foreign::Capability(_) => {
                    let fresh = api::dispatch(
                        &host,
                        caller.clone(),
                        case.name,
                        testing_api::with_never_issued_token(&args),
                    )
                    .await;
                    if answer_text(&fresh) != text {
                        failures.push(format!(
                            "{}: a spent token of B's is answered differently from a never-issued \
                             one\n   spent: {text}\n missing: {}",
                            case.name,
                            answer_text(&fresh)
                        ));
                    }
                }
                Foreign::EntityLess => {}
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
        teams.cli.started(),
        Vec::<String>::new(),
        "no case spawned anything"
    );
}

/// Every team on the board, before a call: an event for a team outside it
/// is for a team the call created (`create_team`).
async fn team_ids(ctx: &ServiceContext) -> BTreeSet<String> {
    sqlx::query_scalar("SELECT id FROM teams")
        .fetch_all(&ctx.pool)
        .await
        .expect("list the teams")
        .into_iter()
        .collect()
}

/// Whether `event` is for a team that did not exist before the call.
fn for_a_new_team(event: &ChangeEvent, before: &BTreeSet<String>) -> bool {
    matches!(&event.audience, Audience::Team(team_id) if !before.contains(team_id))
}

/// An answer as the text a check scans: the value, or the error's payload.
fn answer_text(answer: &rimaia_core::Result<Value>) -> String {
    match answer {
        Ok(value) => value.to_string(),
        Err(error) => serde_json::to_string(error).expect("an error serializes"),
    }
}

// ---------------------------------------------------------------------------
// The local rows of the registry (task 049)
// ---------------------------------------------------------------------------
//
// A local handler lives in the desktop shell, which core cannot call. Each case
// below runs the core functions that row's handler calls, under team A's
// context and the fixture's machine, and puts them through 039's checks:
//
// 1. nothing of team B's in the answer;
// 2. a foreign id reads as a missing one, for a row that takes an id;
// 3. B's rows are unchanged, on the board and on this machine;
// 4. every event the call published names team A.
//
// The shell's own half of a handler, its state, its dialogs and its spawns,
// is not run: none of it reads the board, and CI runs no tests in `src-tauri`.

/// Which of a team's ids a case sends.
#[derive(Debug, Clone, Copy)]
enum Take {
    Task,
    Run,
    TaskAndRun,
    Repository,
}

enum Local {
    /// Takes an id of team B's, and answers `not_found` for it.
    Ids(Take),
    /// Takes an id of team B's and reports a refusal inside its answer, so the
    /// answer has to read exactly as a never-issued id's.
    Filter(Take),
    /// Names no entity: a machine setting, an aggregate of the caller's own.
    EntityLess,
    /// The handler calls nothing in `rimaia-core` that reads a team's board
    /// or this machine's records for one: it opens a native window, reads the
    /// shell's own state, or drives the queue, which `rimaia-runner` owns.
    NoCoreCall(&'static str),
}

struct LocalCase {
    name: &'static str,
    kind: Local,
}

const fn local_case(name: &'static str, kind: Local) -> LocalCase {
    LocalCase { name, kind }
}

/// One per `local` row of `api::registry::COMMANDS`.
/// `every_board_command_has_a_cross_team_case` holds this equal to the rows.
static LOCAL_CASES: &[LocalCase] = &[
    // app
    local_case("get_app_info", Local::EntityLess),
    local_case(
        "reveal_app_data_dir",
        Local::NoCoreCall("opens the data directory with the opener plugin"),
    ),
    local_case(
        "debug_provoke_error",
        Local::NoCoreCall("returns a fixed error"),
    ),
    local_case("get_client_capabilities", Local::EntityLess),
    local_case(
        "choose_folder",
        Local::NoCoreCall("opens a native folder picker"),
    ),
    // repositories
    local_case("list_checkouts", Local::EntityLess),
    local_case("register_repository", Local::EntityLess),
    local_case("set_repository_worktree_root", Local::Ids(Take::Repository)),
    local_case(
        "set_repository_unattended_runs",
        Local::Ids(Take::Repository),
    ),
    local_case("set_repository_on_archive", Local::Ids(Take::Repository)),
    local_case(
        "set_repository_max_concurrency",
        Local::Ids(Take::Repository),
    ),
    local_case("get_repository_remote_info", Local::Ids(Take::Repository)),
    local_case(
        "get_repository_credential_status",
        Local::Ids(Take::Repository),
    ),
    local_case("set_repository_credential", Local::Ids(Take::Repository)),
    local_case("remove_repository_credential", Local::Ids(Take::Repository)),
    // settings
    local_case("get_run_environment", Local::EntityLess),
    local_case("set_run_environment", Local::EntityLess),
    local_case("get_strategy_ceiling", Local::EntityLess),
    local_case("set_strategy_ceiling", Local::EntityLess),
    local_case("preview_composed_prompt", Local::Ids(Take::Task)),
    // strategy
    local_case("plan_task_strategy", Local::Ids(Take::Task)),
    local_case("plan_tasks_strategy", Local::Filter(Take::Task)),
    local_case(
        "cancel_plan_pass",
        Local::NoCoreCall("trips the shell's own cancel signal"),
    ),
    // worktree
    local_case("get_worktree_status", Local::Ids(Take::Task)),
    local_case("get_diff_summary", Local::Ids(Take::Task)),
    local_case("reveal_task_worktree", Local::Ids(Take::Task)),
    local_case("list_open_in_targets", Local::EntityLess),
    local_case("open_task_worktree_in", Local::Ids(Take::Task)),
    local_case("get_worktree_inventory", Local::EntityLess),
    local_case("remove_task_worktree", Local::Ids(Take::Task)),
    local_case("cleanup_done_worktrees", Local::EntityLess),
    local_case("cleanup_merged_worktrees", Local::EntityLess),
    local_case("get_worktree_auto_cleanup", Local::EntityLess),
    local_case("set_worktree_auto_cleanup", Local::EntityLess),
    local_case("list_local_worktrees", Local::EntityLess),
    // runs
    local_case("cancel_task_run", Local::Ids(Take::Task)),
    local_case("read_run_transcript_page", Local::Ids(Take::Run)),
    local_case("search_run_transcript", Local::Ids(Take::Run)),
    local_case("summarize_run_transcript", Local::Ids(Take::Run)),
    local_case("reveal_run_log", Local::Ids(Take::Run)),
    local_case("get_run_log_path", Local::Ids(Take::TaskAndRun)),
    local_case("get_run_log_size", Local::EntityLess),
    local_case("prune_run_logs", Local::Filter(Take::Task)),
    // queue
    local_case(
        "start_queue",
        Local::NoCoreCall("drives the queue, which rimaia-runner owns"),
    ),
    local_case(
        "pause_queue",
        Local::NoCoreCall("drives the queue, which rimaia-runner owns"),
    ),
    local_case(
        "resume_queue",
        Local::NoCoreCall("drives the queue, which rimaia-runner owns"),
    ),
    local_case(
        "stop_queue",
        Local::NoCoreCall("drives the queue, which rimaia-runner owns"),
    ),
    local_case(
        "get_queue_status",
        Local::NoCoreCall("reads the queue, which rimaia-runner owns"),
    ),
    local_case("get_run_capacity", Local::EntityLess),
    local_case("set_schedule_mode", Local::EntityLess),
    local_case("set_max_concurrency", Local::EntityLess),
    // schedules
    local_case("list_schedules", Local::EntityLess),
    local_case("create_schedule", Local::EntityLess),
    local_case("update_schedule", Local::EntityLess),
    local_case("set_schedule_enabled", Local::EntityLess),
    local_case("delete_schedule", Local::EntityLess),
    local_case("preview_schedule_preflight", Local::EntityLess),
    local_case("list_timezones", Local::EntityLess),
    // mcp
    local_case(
        "get_mcp_status",
        Local::NoCoreCall("reads the shell's own MCP handle"),
    ),
    local_case("set_mcp_port", Local::EntityLess),
    local_case(
        "test_mcp_connection",
        Local::NoCoreCall("probes a loopback address and reads no row"),
    ),
    // doctor
    local_case("run_doctor", Local::EntityLess),
    local_case("dismiss_onboarding", Local::EntityLess),
    local_case("dismiss_doctor_warning", Local::EntityLess),
    local_case("restore_doctor_warning", Local::EntityLess),
];

/// The ids a local case sends: one team's, or ones nothing issued.
#[derive(Clone)]
struct Subject {
    task: String,
    run: String,
    repository: String,
}

impl Subject {
    fn of(team: &rimaia_core::testing::teams::TeamBoard) -> Self {
        Self {
            task: team.in_review.clone(),
            run: team.implementation_run.clone(),
            repository: team.repository.id.clone(),
        }
    }

    /// `self` with the ids `take` names replaced by fresh ones, and the
    /// replacements, as `(foreign, fresh)`.
    fn never_issued(&self, take: Take) -> (Self, Vec<(String, String)>) {
        let mut fresh = self.clone();
        let mut substitutions = Vec::new();
        let mut replace = |slot: &mut String| {
            let id = TwoTeams::never_issued();
            substitutions.push((slot.clone(), id.clone()));
            *slot = id;
        };
        match take {
            Take::Task => replace(&mut fresh.task),
            Take::Run => replace(&mut fresh.run),
            Take::TaskAndRun => {
                replace(&mut fresh.task);
                replace(&mut fresh.run);
            }
            Take::Repository => replace(&mut fresh.repository),
        }
        (fresh, substitutions)
    }

    /// The arguments as a value, for the leak scan to skip what was sent.
    fn sent(&self, take: Take) -> Value {
        match take {
            Take::Task => json!({ "task": self.task }),
            Take::Run => json!({ "run": self.run }),
            Take::TaskAndRun => json!({ "task": self.task, "run": self.run }),
            Take::Repository => json!({ "repository": self.repository }),
        }
    }
}

fn value<T: serde::Serialize>(answer: rimaia_core::Result<T>) -> rimaia_core::Result<Value> {
    answer.map(|answer| serde_json::to_value(answer).expect("an answer serializes"))
}

fn debug<T: std::fmt::Debug>(answer: rimaia_core::Result<T>) -> rimaia_core::Result<Value> {
    answer.map(|answer| json!(format!("{answer:?}")))
}

fn schedule_input() -> schedule::ScheduleInput {
    serde_json::from_value(json!({
        "name": "Nightly",
        "mode": "parallel",
        "maxConcurrency": 2,
        "timezone": "UTC",
        "cron": "0 22 * * *",
        "enabled": false,
    }))
    .expect("a schedule input")
}

fn a_warning() -> rimaia_core::db::settings::Dismissal {
    serde_json::from_value(json!({ "check": "git", "repository": null, "detail": "a warning" }))
        .expect("a dismissal")
}

/// What the row's shell handler calls, as team A, with `subject`'s ids.
async fn run_local(name: &str, teams: &TwoTeams, subject: &Subject) -> rimaia_core::Result<Value> {
    use rimaia_core::db::settings;
    use rimaia_core::machine;
    use rimaia_core::runner::strategy as runner_strategy;
    use rimaia_core::runner::{CancelSignal, Starter};
    use rimaia_core::runs::{self, transcript};
    use rimaia_core::scheduler::{capacity, SlotOwner};
    use rimaia_core::testing::repo::TempRepo;
    use rimaia_core::{archive, openers, repo, worktree};

    let ctx = &teams.a;
    let machine = &teams.machine;
    let runner_id = &teams.team_a.runner_id;
    let board = teams.board(ctx);
    let id = subject.repository.as_str();
    let task_id = subject.task.as_str();
    let run_id = subject.run.as_str();

    match name {
        "get_app_info" => value(settings::onboarding_dismissed(machine).await),
        "get_client_capabilities" => Ok(serde_json::to_value(
            rimaia_core::api::capabilities::ClientCapabilities::desktop(runner_id.clone(), None),
        )
        .expect("capabilities serialize")),
        "list_checkouts" => value(repo::checkouts(ctx, machine).await),
        "register_repository" => {
            let source = TempRepo::init();
            value(
                repo::register(
                    ctx,
                    machine,
                    &teams.paths.worktrees_dir(),
                    repo::NewRepository {
                        path: source.path().to_string_lossy().into_owned(),
                        name: None,
                        worktree_root: None,
                    },
                )
                .await,
            )
        }
        "set_repository_worktree_root" => {
            let root = teams.paths.worktrees_dir().join("moved");
            value(
                repo::set_worktree_root(ctx, machine, id, root.to_string_lossy().into_owned())
                    .await,
            )
        }
        "set_repository_unattended_runs" => {
            value(repo::set_allow_unattended_runs(ctx, machine, id, true).await)
        }
        "set_repository_on_archive" => value(
            archive::set_repository_on_archive(
                ctx,
                machine,
                id,
                rimaia_core::db::OnArchive::None,
                None,
            )
            .await,
        ),
        "set_repository_max_concurrency" => {
            value(repo::set_max_concurrency(ctx, machine, id, 2).await)
        }
        "get_repository_remote_info" => {
            let repository = repo::get(ctx, id).await?;
            value(repo::remote_info(&machine::checkout_of(machine, &repository).await?).await)
        }
        "get_repository_credential_status" => {
            let repository = repo::get(ctx, id).await?;
            let checkout = machine::checkout_of(machine, &repository).await?;
            let remote = repo::remote_info(&checkout).await?;
            Ok(json!({
                "remote": remote,
                "store": format!("{:?}", teams.runner.credentials.status(&repository.id).await),
            }))
        }
        // The forge's verdict on a token, `provision::verify`, is a call to a
        // forge and reads nothing of a board, so the case stops short of it.
        "set_repository_credential" => {
            let repository = repo::get(ctx, id).await?;
            machine::checkout_of(machine, &repository).await?;
            let secret = rimaia_core::credentials::Secret::new("a-token".to_string())?;
            teams.runner.credentials.set(id, secret).await?;
            value(repo::set_credential_metadata(ctx, machine, id, None, Some("a label")).await)
        }
        "remove_repository_credential" => {
            repo::get(ctx, id).await?;
            teams.runner.credentials.delete(id).await?;
            value(repo::clear_credential_metadata(ctx, machine, id).await)
        }
        "get_run_environment" => value(settings::run_environment(machine).await),
        "set_run_environment" => value(
            settings::set_run_environment(machine, rimaia_core::db::RunEnvironment::Inherit).await,
        ),
        "get_strategy_ceiling" => {
            value(rimaia_core::consent::ceiling::strategy_ceiling(machine).await)
        }
        "set_strategy_ceiling" => {
            let ceiling =
                serde_json::from_value(json!({ "models": ["sonnet"], "maxEffort": "medium" }))
                    .expect("a ceiling");
            value(rimaia_core::consent::ceiling::set_strategy_ceiling(machine, &ceiling).await)
        }
        "preview_composed_prompt" => {
            use rimaia_core::runner::prompt;
            let base = settings::base_instructions_for_task(ctx, task_id).await?;
            let detail = rimaia_core::tasks::get_task(ctx, task_id).await?;
            let repository = repo::get(ctx, &detail.task.repository_id).await?;
            let guidance = prompt::StrategyGuidance::for_task(&detail);
            let team_kind = rimaia_core::tasks::team_kind_of_task(ctx, task_id).await?;
            Ok(json!(prompt::compose_prompt(
                &base,
                &detail,
                &repository,
                prompt::PromptTeam::from_kind(team_kind, None),
                guidance.as_ref(),
                teams.runner.provider.fanout_noun(),
            )))
        }
        // Up to the claim: the spawn after it is the shell's.
        "plan_task_strategy" => {
            let claim = runner_strategy::claim_for_planning(
                Starter::at_runner(ctx, runner_id),
                board.as_ref(),
                machine,
                &teams.in_flight,
                task_id,
                SlotOwner::Manual,
            )
            .await?;
            Ok(json!(claim.is_ok()))
        }
        "plan_tasks_strategy" => {
            let selection = runner_strategy::PlanSelection {
                column: None,
                repository_id: None,
                task_ids: vec![task_id.to_string()],
            };
            debug(
                runner_strategy::plan_all(
                    Starter::at_runner(ctx, runner_id),
                    board.as_ref(),
                    machine,
                    &teams.paths,
                    &teams.runner,
                    &teams.in_flight,
                    &selection,
                    &CancelSignal::new(),
                    &|_progress| {},
                )
                .await,
            )
        }
        "get_worktree_status" => value(worktree::status(ctx, machine, task_id).await),
        "get_diff_summary" => value(worktree::diff_summary(ctx, machine, task_id).await),
        "list_local_worktrees" => value(worktree::local_records(ctx, machine).await),
        "reveal_task_worktree" => value(worktree::local_path(ctx, machine, task_id).await),
        "list_open_in_targets" => value(Ok(openers::detect(
            &openers::Machine::host(),
            &openers::SystemProbe,
        ))),
        "open_task_worktree_in" => debug(
            openers::launch_for_task(
                ctx,
                machine,
                &openers::Machine::host(),
                &openers::SystemProbe,
                openers::Target::VsCode,
                task_id,
            )
            .await,
        ),
        "get_worktree_inventory" => value(worktree::inventory(ctx, machine).await),
        "remove_task_worktree" => value(
            worktree::remove_worktree(
                ctx,
                machine,
                task_id,
                worktree::RemovalAuthorization::default(),
            )
            .await,
        ),
        "cleanup_done_worktrees" => value(worktree::remove_done_worktrees(ctx, machine).await),
        "cleanup_merged_worktrees" => value(worktree::remove_merged_worktrees(ctx, machine).await),
        "get_worktree_auto_cleanup" => value(worktree::auto_cleanup(machine).await),
        "set_worktree_auto_cleanup" => {
            value(worktree::set_auto_cleanup(machine, worktree::AutoCleanup::Off).await)
        }
        // B's task is really running, so a handler that cancels by bare id
        // is caught by the run it stops and not only by the answer it gives.
        "cancel_task_run" => {
            let held = teams
                .in_flight
                .acquire_unbounded(task_id, &teams.team_b.repository.id, SlotOwner::Manual)
                .expect("B's task is not yet running");
            let answer = runs::cancel_in_flight(ctx, &teams.in_flight, task_id).await;
            if held.cancel_signal().is_cancelled() {
                return Err(rimaia_core::Error::internal(
                    "team B's run was cancelled by team A",
                ));
            }
            answer.map(|()| Value::Null)
        }
        "read_run_transcript_page" => {
            let path = runs::transcript_of(ctx, &teams.paths, run_id).await?;
            value(transcript::read_page(&path, 0, transcript::DEFAULT_PAGE_SIZE).await)
        }
        "summarize_run_transcript" => {
            let path = runs::transcript_of(ctx, &teams.paths, run_id).await?;
            value(transcript::summarize(&path).await)
        }
        "search_run_transcript" => {
            let path = runs::transcript_of(ctx, &teams.paths, run_id).await?;
            value(transcript::search(&path, "needle").await)
        }
        "reveal_run_log" => debug(runs::log_path_to_reveal(ctx, &teams.paths, run_id).await),
        "get_run_log_path" => debug(runs::log_path(ctx, &teams.paths, task_id, run_id).await),
        "get_run_log_size" => Ok(json!(runs::total_log_size(&teams.paths).await)),
        "prune_run_logs" => value(
            runs::prune_logs(
                ctx,
                &teams.paths,
                runs::PruneCriterion::Task(task_id.to_string()),
            )
            .await,
        ),
        "get_run_capacity" => value(capacity::configured(machine).await),
        "set_schedule_mode" => {
            capacity::set_schedule_mode(machine, rimaia_core::db::ScheduleMode::Parallel).await?;
            value(capacity::configured(machine).await)
        }
        "set_max_concurrency" => {
            capacity::set_max_concurrency(machine, 2).await?;
            value(capacity::configured(machine).await)
        }
        "list_schedules" => value(schedule::list(machine).await),
        "create_schedule" => value(schedule::create(machine, schedule_input()).await),
        "update_schedule" => {
            let made = schedule::create(machine, schedule_input()).await?;
            value(schedule::update(machine, &made.id, schedule_input()).await)
        }
        "set_schedule_enabled" => {
            let made = schedule::create(machine, schedule_input()).await?;
            value(schedule::set_enabled(machine, &made.id, true).await)
        }
        "delete_schedule" => {
            let made = schedule::create(machine, schedule_input()).await?;
            value(schedule::delete(machine, &made.id).await)
        }
        "preview_schedule_preflight" => {
            let made = schedule::create(machine, schedule_input()).await?;
            let (repositories, _) =
                rimaia_core::scheduler::view::for_runner(machine, &teams.in_flight).await?;
            let runner = rimaia_core::scheduler::RunnerView::new(
                runner_id.clone(),
                teams.runner.provider.id(),
                repositories,
            );
            value(schedule::preview(machine, ctx, &made.id, &runner).await)
        }
        "list_timezones" => Ok(json!(schedule::timezones())),
        "set_mcp_port" => value(mcp::set_configured_port(machine, 48_200).await),
        "run_doctor" => value(rimaia_core::doctor::run(machine, ctx, &teams.doctor()).await),
        "dismiss_onboarding" => value(settings::set_onboarding_dismissed(machine, true).await),
        "dismiss_doctor_warning" => value(rimaia_core::doctor::dismiss(machine, a_warning()).await),
        "restore_doctor_warning" => {
            value(rimaia_core::doctor::restore(machine, &a_warning()).await)
        }
        other => panic!("`{other}` has a case and no call: add it to run_local"),
    }
}

/// What this machine records for `team`: its checkout and its worktrees.
async fn machine_rows(teams: &TwoTeams, team: &rimaia_core::testing::teams::TeamBoard) -> String {
    let worktrees: Vec<_> = teams
        .machine
        .store
        .list_worktrees()
        .await
        .expect("list the worktree records")
        .into_iter()
        .filter(|record| record.repository_id == team.repository.id)
        .collect();
    let checkout = teams
        .machine
        .store
        .get_checkout(&team.repository.id)
        .await
        .expect("read the checkout");
    format!(
        "{}\n{}",
        serde_json::to_string(&worktrees).expect("worktrees serialize"),
        serde_json::to_string(&checkout).expect("a checkout serializes"),
    )
}

#[tokio::test]
async fn a_team_cannot_see_another_teams_ids_through_any_local_command() {
    let mut teams = TwoTeams::new().await;
    let team_a = teams.team_a.team_id.clone();
    let foreign = Subject::of(&teams.team_b);
    let own = Subject::of(&teams.team_a);

    let mut failures: Vec<String> = Vec::new();
    for case in LOCAL_CASES {
        let (take, subject) = match &case.kind {
            Local::NoCoreCall(_) => continue,
            Local::Ids(take) | Local::Filter(take) => (Some(*take), &foreign),
            Local::EntityLess => (None, &own),
        };
        let sent = take.map_or(Value::Null, |take| subject.sent(take));
        let before = teams.snapshot_b().await;
        let machine_before = machine_rows(&teams, &teams.team_b).await;
        teams.drain_changes();

        let answer = run_local(case.name, &teams, subject).await;
        let events = teams.drain_changes();
        let text = answer_text(&answer);

        // 1. Nothing of B's leaks.
        for leaked in testing_api::leaks(&teams.team_b, &sent, &text) {
            failures.push(format!(
                "{}: the answer carries B's {leaked}: {text}",
                case.name
            ));
        }
        // 3. B's rows are unchanged, on the board and on this machine.
        let after = teams.snapshot_b().await;
        if after != before {
            failures.push(format!(
                "{}: team B's rows changed\nbefore:\n{before}\nafter:\n{after}",
                case.name
            ));
        }
        let machine_after = machine_rows(&teams, &teams.team_b).await;
        if machine_after != machine_before {
            failures.push(format!(
                "{}: team B's records on this machine changed\nbefore:\n{machine_before}\nafter:\n{machine_after}",
                case.name
            ));
        }
        // 4. Every event is for team A, or for the acting user alone.
        for event in events
            .iter()
            .filter(|event| !for_team_or_actor(event, &team_a, &teams.team_a.owner_id))
        {
            failures.push(format!(
                "{}: an event is for another team or user: {event:?}",
                case.name
            ));
        }
        // 2. A foreign id reads as a missing one.
        match (&case.kind, take) {
            (Local::Ids(_), _) => {
                if answer.as_ref().err().map(|error| error.code()) != Some(ErrorCode::NotFound) {
                    failures.push(format!("{}: B's ids are not not_found: {text}", case.name));
                }
            }
            (Local::Filter(_), Some(take)) => {
                let (fresh_subject, substitutions) = foreign.never_issued(take);
                let fresh = run_local(case.name, &teams, &fresh_subject).await;
                let restored = testing_api::restore_ids(&answer_text(&fresh), &substitutions);
                if restored != text {
                    failures.push(format!(
                        "{}: a foreign id is answered differently from a missing one\n\
                         foreign: {text}\n missing: {restored}",
                        case.name
                    ));
                }
            }
            _ => {}
        }
    }

    assert!(
        failures.is_empty(),
        "{} failure(s):\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    assert_eq!(
        teams.cli.started(),
        Vec::<String>::new(),
        "no case spawned anything"
    );
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
        let teams_before = team_ids(&board.teams.a).await;
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
        if let Some(before) = &before {
            let after = board.teams.snapshot_b().await;
            if after != *before {
                failures.push(format!(
                    "{}: point 3, team B's rows changed\nbefore:\n{before}\nafter:\n{after}",
                    case.tool
                ));
            }
        }

        // 4. Every event is for team A, for the acting user alone, or for a
        //    team the call itself created.
        for event in events.iter().filter(|event| {
            !for_team_or_actor(event, &team_a, &board.teams.team_a.owner_id)
                && !for_a_new_team(event, &teams_before)
        }) {
            failures.push(format!(
                "{}: point 4, an event is for another team or user: {event:?}",
                case.tool
            ));
        }

        // 2, for a token: every spent token of B's reads as one never issued,
        //    and none changes B's rows.
        if args.get("token").is_some() {
            let missing = invoke(&client, case.tool, &with_never_issued_token(&args)).await;
            for token in board.teams.team_b.invitations.unusable() {
                let spent = invoke(&client, case.tool, &json!({ "token": token.expose() })).await;
                if spent != missing {
                    failures.push(format!(
                        "{}: point 2, a spent token of B's is answered differently from a \
                         never-issued one\n   spent: {spent}\n missing: {missing}",
                        case.tool
                    ));
                }
            }
            if let Some(before) = &before {
                if board.teams.snapshot_b().await != *before {
                    failures.push(format!(
                        "{}: point 3, a spent token changed B's rows",
                        case.tool
                    ));
                }
            }
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
const STORE_HANDLE_EXCEPTIONS: [(&str, &str); 56] = [
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
        "tasks::run_state::transition",
        "a claim takes its run-state edges inside the lease's transaction (043): the one \
         conditional write of run_state, with the expected state in its WHERE",
    ),
    (
        "tasks::run_state::set_within",
        "a finish lands its task inside the transaction that deletes its lease and sets its pin \
         (043); set_run_state is it run in a transaction of its own",
    ),
    (
        "tasks::strategy::write_strategy",
        "record_strategy writes the planner's strategy inside the transaction its fence read the \
         lease in (043); it filters by that transaction's scope",
    ),
    (
        "review::findings::record_within",
        "record_review_findings writes inside the transaction its fence read the lease in (043); \
         it filters by that transaction's scope",
    ),
    (
        "runner::outcome::insert_run_within",
        "start_run opens the row and moves its lease onto it in one fenced transaction (043)",
    ),
    (
        "runner::outcome::close_within",
        "finish_run closes the row inside the transaction its fence read the lease in (043)",
    ),
    (
        "runner::outcome::land_within",
        "finish_run lands the task, deletes or keeps its lease and sets its pin in one \
         transaction (043)",
    ),
    (
        "board::lease::eligible",
        "the claim, selection's plan and a Continue each ask it inside the transaction or on the \
         connection that acts on the answer (043); it reads one task's pin by id",
    ),
    (
        "board::lease::current",
        "the fence reads the live lease inside the transaction of the first write it guards \
         (043); it filters by the lease's team and that transaction's scope",
    ),
    (
        "board::lease::open_run",
        "start_run moves the lease onto its run inside the transaction that opened the row (043)",
    ),
    (
        "board::lease::land",
        "finish_run's landing, inside the transaction that lands the task (043)",
    ),
    (
        "tasks::service::team_of_task",
        "every write that names a task resolves the task's team inside its own transaction, for \
         its event; it filters by that transaction's scope",
    ),
    (
        "tasks::service::write_plan",
        "the plan helper: update_task and a review note record a plan revision inside the \
         transaction that read the task (045)",
    ),
    (
        "tasks::service::record_plan_revision",
        "the plan helper's other half: a retitle, a link edit and a strategy write record a plan \
         revision inside the transaction that changed the content (045); it writes one task \
         already read in that transaction's scope",
    ),
    (
        "board::lease::consent_refusal",
        "run_context re-checks consent inside its fence's transaction and ends the lease there \
         (045); it reads one task by id",
    ),
    (
        "board::lease::end_within",
        "release and run_context end a fenced lease inside the transaction that fenced it (045)",
    ),
    (
        "consent::inputs",
        "eligible reads the revisions a run would execute inside the claim's transaction (045); \
         it reads one task by id, which the caller has already scoped",
    ),
    (
        "consent::context_is_current",
        "run_context compares the context it read with the database inside its fence's \
         transaction, so consent judges the text it returns (045); it reads one task by id, \
         which the caller has already scoped, and that task's own team's settings",
    ),
    (
        "consent::missing",
        "eligible reads the owner's acceptances and trust list inside the claim's transaction \
         (045); it filters by the task's team",
    ),
    (
        "consent::task_row",
        "eligible reads the task's assignee, team and ceiling inside the claim's transaction \
         (045); it reads one task by id, which the caller has already scoped",
    ),
    (
        "consent::runner_row",
        "eligible reads the claiming runner's owner and policy inside the claim's transaction \
         (045); it reads the runner the adapter serves",
    ),
    (
        "consent::refusal_for",
        "eligible names a missing piece's author inside the claim's transaction (045)",
    ),
    (
        "consent::ensure_member",
        "assign_task and set_trust check membership inside the transaction that writes (045); \
         it reads one membership of a team the caller has already scoped",
    ),
    (
        "runs::commit_authors",
        "eligible credits a dependency's commit to its runners' owners inside the claim's \
         transaction (045); the dependency was resolved under the caller's scope",
    ),
    (
        "identity::sign_in::upsert_user",
        "signing in runs before there is a caller to scope anything to; it finds or creates \
         the user a scope is then read for (047)",
    ),
    (
        "identity::sessions::create",
        "a session is made at the end of sign-in, before any caller exists, and belongs to a \
         user, not a team (047)",
    ),
    (
        "identity::tokens::create_desktop",
        "the desktop's token is made at the end of its browser sign-in, before any caller \
         exists, and belongs to a user, not a team (047)",
    ),
    (
        "identity::tokens::create_runner",
        "it writes inside the transaction that writes its runners row, for redeem and \
         pair_own_runner (047)",
    ),
    (
        "identity::tokens::unpair_runner",
        "it writes inside the caller's transaction, so a token's revocation and its runner's \
         unpairing are one act; revoke_api_token scopes it to the caller's own runner (047)",
    ),
    (
        "identity::pairing::redeem",
        "a headless runner redeems a code with no credential at all; the code names the user \
         the runner is paired to (047)",
    ),
    (
        "identity::insert_team",
        "a team comes into being inside its creator's transaction: create_personal_team's for a \
         sign-up, create_team's for a shared team (051); it writes only the new team's rows",
    ),
    (
        "identity::teams::listing_of",
        "create_team and accept_invitation answer the caller's listing of the team they just \
         wrote, inside that transaction (051); it reads the caller's own membership",
    ),
    (
        "teams::require_owner",
        "the one role check reads the actor's membership inside the BEGIN IMMEDIATE transaction \
         that then writes, so a demotion committed since the request began wins (051); it checks \
         the context's scope first",
    ),
    (
        "teams::members::remove_member_in",
        "remove_member and leave_team withdraw a member inside the transaction that checked who \
         asked (051), and the testing member helper's removal calls it on its own connection; \
         it writes one team's rows",
    ),
    (
        "teams::invitations::create_invitation_in",
        "create_invitation inserts inside the transaction its owner check read (051), and the \
         testing member helper calls it on the caller's transaction; it writes one team's row",
    ),
    (
        "teams::invitations::accept_invitation_in",
        "accept_invitation redeems inside its own transaction (051), and the testing member \
         helper calls it on the caller's transaction; the token names the team",
    ),
    (
        "teams::invitations::revoke_links_made_by",
        "a removal, a departure, a demotion and an account deletion revoke the person's links \
         inside the transaction that checked who asked (051, D41 point 6); it writes one team's \
         rows",
    ),
    (
        "identity::team_kind_in",
        "the one predicate for a team's kind (ADR-0032's 2026-10-11 amendment) is read inside the \
         transaction of the claim, the ceiling command and an acceptance, which the kind decides; \
         every caller passes the team of a row it has already read, and only a kind comes back",
    ),
    (
        "teams::delete::purge",
        "delete_team and delete_account delete a team's rows in D28's order inside the \
         transaction that checked who asked (051)",
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
        "identity::insert_team",
        "the seed row that comes into being with its team (D28 part 3), personal or shared \
         (051); no set_team can run before the team exists",
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
        ProviderProfile::of(teams.doctor().provider.as_ref()),
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

/// 039's check 4, as task 048 refines it: an event is for team A, or it is
/// the acting user's own setting (`set_subscription_cost`), which is
/// announced to that user alone.
fn for_team_or_actor(event: &ChangeEvent, team_a: &str, actor: &str) -> bool {
    match &event.audience {
        Audience::Team(team_id) => team_id == team_a,
        Audience::User(user_id) => user_id == actor,
    }
}
