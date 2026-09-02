//! The MCP tool handlers, called directly, against a real migrated database
//! (ADR-0006, task 010's acceptance criteria).
//!
//! The centrepiece is the pair of tests named `..._identically_by_the_ui_path_
//! and_by_mcp`. Task 010's Notes call the "same invariants, both paths" test
//! the point of this task's design, and the assertion is deliberately stronger
//! than "both failed": both must fail with the **same payload**, so neither
//! path can grow a message of its own without this file going red.
//!
//! Requests are built by deserializing the JSON an agent would actually send,
//! rather than by constructing the structs, so the schema and the handler are
//! exercised together.

use rimaia_core::db::{settings, BoardColumn, MutationSource, StrategyMode, StrategySource};
use rimaia_core::mcp::requests::{
    GetTaskRequest, ListTasksRequest, MoveTaskRequest, RemoveTaskLinkRequest,
    SetTaskDependenciesRequest, UpdateTaskRequest,
};
use rimaia_core::mcp::responses::{TaskListView, TaskView};
use rimaia_core::mcp::RimaiaServer;
use rimaia_core::strategy::{settings as strategy_settings, StrategyDefaults};
use rimaia_core::tasks::{self, NewTask, StrategyPlan, TaskPatch};
use rimaia_core::testing::{self, TestContext};
use rimaia_core::{ChangeEvent, Error};

use pretty_assertions::assert_eq;
use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolResponse, CallToolResult};
use serde_json::{json, Value};
use sqlx::SqlitePool;

// ---------------------------------------------------------------------------
// The same invariant, through both doors, with the same payload
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ready_without_a_plan_is_refused_identically_by_the_ui_path_and_by_mcp() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;

    // The UI path: exactly what `commands::tasks::create_task` calls.
    let ui = tasks::create_task(
        &h.context,
        NewTask {
            repository_id: repository_id.clone(),
            title: "Queue me".to_string(),
            plan: None,
            extra_instructions: None,
            column: Some(BoardColumn::Ready),
            links: vec![],
        },
    )
    .await
    .expect_err("ready with no plan is refused");

    // The same case, through the tool handler.
    let mcp = server(&h)
        .create_task(Parameters(request(json!({
            "repository_id": repository_id,
            "title": "Queue me",
            "column": "ready",
        }))))
        .await;

    assert_same_refusal(&ui, mcp);
}

#[tokio::test]
async fn a_blank_title_is_refused_identically_by_the_ui_path_and_by_mcp() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;

    let ui = tasks::create_task(
        &h.context,
        NewTask {
            repository_id: repository_id.clone(),
            title: "   ".to_string(),
            plan: Some("a plan".to_string()),
            extra_instructions: None,
            column: None,
            links: vec![],
        },
    )
    .await
    .expect_err("a blank title is refused");

    let mcp = server(&h)
        .create_task(Parameters(request(json!({
            "repository_id": repository_id,
            "title": "   ",
            "plan": "a plan",
        }))))
        .await;

    assert_same_refusal(&ui, mcp);
}

#[tokio::test]
async fn an_unknown_repository_is_refused_identically_by_the_ui_path_and_by_mcp() {
    let h = TestContext::new().await;

    let ui = tasks::create_task(
        &h.context,
        NewTask {
            repository_id: "nope".to_string(),
            title: "Orphan".to_string(),
            plan: Some("a plan".to_string()),
            extra_instructions: None,
            column: None,
            links: vec![],
        },
    )
    .await
    .expect_err("a repository that does not exist is refused");

    let mcp = server(&h)
        .create_task(Parameters(request(json!({
            "repository_id": "nope",
            "title": "Orphan",
            "plan": "a plan",
        }))))
        .await;

    assert_same_refusal(&ui, mcp);
}

#[tokio::test]
async fn reassigning_a_task_that_has_a_worktree_is_refused_identically_by_the_ui_path_and_by_mcp() {
    // Seam-contract D13's guard, which that entry says explicitly must not be
    // a UI-only courtesy — task 010 exposes `update_task` too.
    let h = TestContext::new().await;
    let here = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let elsewhere = seed_repository(&h.context.pool, "other", "/tmp/other").await;
    let task = create_task(&h, &here, "Filed in the wrong place").await;
    give_it_a_worktree(&h.context.pool, &task.id).await;

    let ui = tasks::update_task(
        &h.context,
        &task.id,
        TaskPatch {
            repository_id: Some(elsewhere.clone()),
            ..TaskPatch::default()
        },
    )
    .await
    .expect_err("a task with a worktree cannot change repository");

    let mcp = server(&h)
        .update_task(Parameters(request(json!({
            "task_id": task.id,
            "repository_id": elsewhere,
        }))))
        .await;

    assert_same_refusal(&ui, mcp);
}

#[tokio::test]
async fn a_planner_write_back_is_refused_when_the_task_is_not_in_planned_mode() {
    // Task 020's acceptance criterion 6. Two repositories rather than two rows
    // written by hand, because the guard reads the **resolved** mode
    // (seam-contract D17.6): a repository that defaults to `planned` plans every
    // untouched card in it, and one that says nothing leaves its cards in
    // `default`, where the strategy is the user's and a planner has no business
    // writing one.
    let h = TestContext::new().await;
    let planning = seed_repository(&h.context.pool, "planning", "/tmp/planning").await;
    let hand_picked = seed_repository(&h.context.pool, "hand-picked", "/tmp/hand-picked").await;
    strategy_settings::set_repository_default(
        &h.context,
        &planning,
        &StrategyDefaults {
            mode: StrategyMode::Planned,
            ..StrategyDefaults::default()
        },
    )
    .await
    .expect("store the repository default");

    let planned = create_task(&h, &planning, "Let the planner choose").await;
    let ours = create_task(&h, &hand_picked, "The user chose this one").await;

    // The control: the same call, on a task whose mode does ask for a planner.
    let recorded = ok(server(&h)
        .set_task_strategy(Parameters(request(json!({
            "task_id": planned.id,
            "model": "sonnet",
            "effort": "high",
            "rationale": "Three services and a migration.",
        }))))
        .await);
    assert_eq!(recorded.model.as_deref(), Some("sonnet"));
    assert_eq!(recorded.effort.as_deref(), Some("high"));
    assert_eq!(recorded.strategy_source, Some(StrategySource::Planner));

    let refused = as_result(
        server(&h)
            .set_task_strategy(Parameters(request(json!({
                "task_id": ours.id,
                "model": "opus",
                "effort": "max",
            }))))
            .await,
    );

    assert_eq!(refused.is_error, Some(true));
    assert_eq!(
        message(&refused),
        "cannot record a planner's strategy for \"The user chose this one\": it is in default \
         mode, so its strategy is the user's"
    );

    // And nothing was written on the way to being refused — not the envelope,
    // and not the two columns a proposal also sets, which is what would silently
    // change what the next run spawns with.
    let untouched = tasks::get_task(&h.context, &ours.id)
        .await
        .expect("the task is still there")
        .task;
    assert_eq!(untouched.strategy_plan, None);
    assert_eq!(untouched.strategy_source, None);
    assert_eq!(untouched.model, None);
    assert_eq!(untouched.effort, None);
}

#[tokio::test]
async fn set_task_strategy_is_refused_identically_by_the_ui_path_and_by_mcp() {
    // The eleventh tool, held to this file's standard: the mode guard lives in
    // `tasks::set_task_strategy` and nowhere else, so the runner recording a
    // planner failure and a planner writing its proposal back over MCP are
    // refused by the same sentence and the same payload. A copy of the guard in
    // the handler would pass "both refuse" and fail this.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let task = create_task(&h, &repository_id, "Nobody asked for a planner").await;

    // The other caller of the service: `runner::strategy`, annotating a task
    // whose planner failed.
    let ui = tasks::set_task_strategy(
        &h.context,
        &task.id,
        StrategyPlan::proposed(Some("sonnet".to_string()), Some("high".to_string())),
        StrategySource::Planner,
    )
    .await
    .expect_err("a task in default mode is not the planner's to write");

    let mcp = server(&h)
        .set_task_strategy(Parameters(request(json!({
            "task_id": task.id,
            "model": "sonnet",
            "effort": "high",
        }))))
        .await;

    assert_same_refusal(&ui, mcp);
}

#[tokio::test]
async fn the_same_valid_task_is_created_by_either_path() {
    // The control. Without it the four tests above could all pass because both
    // paths are broken in the same way.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;

    let ui = tasks::create_task(
        &h.context,
        NewTask {
            repository_id: repository_id.clone(),
            title: "Queue me".to_string(),
            plan: Some("a plan".to_string()),
            extra_instructions: None,
            column: Some(BoardColumn::Ready),
            links: vec![],
        },
    )
    .await
    .expect("the ui path creates it");

    let mcp = ok(server(&h)
        .create_task(Parameters(request(json!({
            "repository_id": repository_id,
            "title": "Queue me",
            "plan": "a plan",
            "column": "ready",
        }))))
        .await);

    assert_eq!(ui.column, BoardColumn::Ready);
    assert_eq!(mcp.column, BoardColumn::Ready);
    assert_eq!(mcp.title, ui.title);
    assert_eq!(mcp.plan, ui.plan);
}

// ---------------------------------------------------------------------------
// The plan round trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_multi_thousand_word_plan_round_trips_through_create_task_and_get_task() {
    // Task 010's acceptance criterion, with the characters that actually break
    // a round trip: fenced code, backticks, em dashes, non-ASCII, CRLF-free
    // blank lines, and a trailing space nothing is allowed to trim.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let plan = long_plan();
    assert!(plan.split_whitespace().count() > 3_000, "a realistic plan");

    let created = ok(server(&h)
        .create_task(Parameters(request(json!({
            "repository_id": repository_id,
            "title": "A long plan",
            "plan": plan,
        }))))
        .await);

    let fetched = ok(server(&h)
        .get_task(Parameters(request::<GetTaskRequest>(
            json!({ "task_id": created.id }),
        )))
        .await);

    assert_eq!(fetched.plan.as_deref(), Some(plan.as_str()));

    // And through the JSON the agent actually receives, not only the struct.
    let wire = serde_json::to_value(&fetched).expect("a DTO must always serialize");
    assert_eq!(wire["plan"], json!(plan));

    // And against the row, so a lossy column would not pass either.
    let stored: String = sqlx::query_scalar("SELECT plan FROM tasks WHERE id = ?1")
        .bind(&created.id)
        .fetch_one(&h.context.pool)
        .await
        .expect("read the plan back");
    assert_eq!(stored, plan);
}

// ---------------------------------------------------------------------------
// Attribution and live updates
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_task_created_over_mcp_records_source_mcp() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;

    let created = ok(server(&h)
        .create_task(Parameters(request(json!({
            "repository_id": repository_id,
            "title": "From a session",
        }))))
        .await);

    assert_eq!(created.source, MutationSource::Mcp);
}

#[tokio::test]
async fn a_task_created_over_the_ui_path_records_source_ui() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;

    let created = create_task(&h, &repository_id, "From the board").await;

    assert_eq!(created.source, MutationSource::Ui);
}

#[tokio::test]
async fn updating_a_task_over_mcp_leaves_the_source_it_was_created_with() {
    // ADR-0019's decision, from the door that would break it first.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let task = create_task(&h, &repository_id, "From the board").await;

    let updated = ok(server(&h)
        .update_task(Parameters(request(json!({
            "task_id": task.id,
            "title": "Amended by an agent",
        }))))
        .await);

    assert_eq!(updated.title, "Amended by an agent");
    assert_eq!(updated.source, MutationSource::Ui);
}

#[tokio::test]
async fn a_task_created_over_mcp_publishes_tasks_changed_on_the_original_subscriber() {
    // Task 010's "it appears on the board within a second", in test form: the
    // board subscribed to the context the shell built, and the MCP server
    // publishes on a *clone* of it (ADR-0018, ADR-0019).
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;

    let created = ok(server(&h)
        .create_task(Parameters(request(json!({
            "repository_id": repository_id,
            "title": "From a session",
        }))))
        .await);

    assert_eq!(
        h.changes.try_recv().expect("a publication"),
        ChangeEvent::tasks([created.id])
    );
}

// ---------------------------------------------------------------------------
// move_task's synthesised neighbour
// ---------------------------------------------------------------------------

#[tokio::test]
async fn moving_a_task_into_a_populated_column_lands_it_at_the_bottom() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let first = create_ready(&h, &repository_id, "First in the queue").await;
    let second = create_ready(&h, &repository_id, "Second in the queue").await;
    let latecomer = create_task(&h, &repository_id, "Latecomer").await;
    tasks::update_task(
        &h.context,
        &latecomer.id,
        TaskPatch {
            plan: rimaia_core::tasks::Patch::Set("a plan".to_string()),
            ..TaskPatch::default()
        },
    )
    .await
    .expect("give it a plan so it may enter ready");

    ok(server(&h)
        .move_task(Parameters(request::<MoveTaskRequest>(json!({
            "task_id": latecomer.id,
            "column": "ready",
        }))))
        .await);

    let queue = ready_column(&h, &repository_id).await;
    assert_eq!(
        queue,
        vec![first.id, second.id, latecomer.id],
        "no neighbour named means the back of the queue"
    );
}

#[tokio::test]
async fn moving_a_task_that_is_already_alone_in_the_destination_column_does_not_use_itself_as_a_neighbour(
) {
    // Without the exclusion the adapter would hand `move_task` the moving
    // task's own id and earn "a task cannot be moved next to itself" — a
    // refusal for a no-op.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let alone = create_ready(&h, &repository_id, "The only ready task").await;

    let moved = ok(server(&h)
        .move_task(Parameters(request::<MoveTaskRequest>(json!({
            "task_id": alone.id,
            "column": "ready",
        }))))
        .await);

    assert_eq!(moved.column, BoardColumn::Ready);
}

// ---------------------------------------------------------------------------
// The rest of the surface
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cycle_declared_over_mcp_is_refused_naming_the_path() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let a = create_task(&h, &repository_id, "A").await;
    let b = create_task(&h, &repository_id, "B").await;
    ok(server(&h)
        .set_task_dependencies(Parameters(request::<SetTaskDependenciesRequest>(json!({
            "task_id": b.id,
            "depends_on": [a.id],
        }))))
        .await);

    let refused = as_result(
        server(&h)
            .set_task_dependencies(Parameters(request::<SetTaskDependenciesRequest>(json!({
                "task_id": a.id,
                "depends_on": [b.id],
            }))))
            .await,
    );

    assert_eq!(refused.is_error, Some(true));
    assert_eq!(
        message(&refused),
        "cannot save these dependencies: they would create a cycle — \
         \"A\" depends on \"B\" depends on \"A\""
    );
}

#[tokio::test]
async fn a_dependency_declared_over_mcp_comes_back_on_the_task() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let api = create_task(&h, &repository_id, "Add the API").await;
    let caller = create_task(&h, &repository_id, "Call the API").await;

    let updated = ok(server(&h)
        .set_task_dependencies(Parameters(request::<SetTaskDependenciesRequest>(json!({
            "task_id": caller.id,
            "depends_on": [api.id],
        }))))
        .await);

    assert_eq!(updated.depends_on, vec![api.id]);
}

#[tokio::test]
async fn clearing_a_field_over_mcp_erases_it_while_an_omitted_one_is_left_alone() {
    // Seam-contract D16's asymmetry, end to end.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let task = create_task(&h, &repository_id, "Configured").await;
    ok(server(&h)
        .update_task(Parameters(request::<UpdateTaskRequest>(json!({
            "task_id": task.id,
            "model": "opus",
            "extra_instructions": "Skip the migration",
        }))))
        .await);

    let updated = ok(server(&h)
        .update_task(Parameters(request::<UpdateTaskRequest>(json!({
            "task_id": task.id,
            "clear": ["model"],
        }))))
        .await);

    assert_eq!(updated.model, None, "named in `clear`");
    assert_eq!(
        updated.extra_instructions.as_deref(),
        Some("Skip the migration"),
        "omitted, so untouched — the whole point of not using null"
    );
    assert_eq!(updated.plan.as_deref(), Some("a plan"), "never at risk");
}

#[tokio::test]
async fn a_link_added_and_removed_over_mcp_comes_back_on_the_task_each_time() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let task = create_task(&h, &repository_id, "Needs a reference").await;

    let with_link = ok(server(&h)
        .add_task_link(Parameters(request(json!({
            "task_id": task.id,
            "label": "ADR-0006",
            "url": "https://example.com/adr-0006",
        }))))
        .await);
    assert_eq!(with_link.links.len(), 1);
    assert_eq!(with_link.links[0].label, "ADR-0006");

    let without = ok(server(&h)
        .remove_task_link(Parameters(request::<RemoveTaskLinkRequest>(json!({
            "link_id": with_link.links[0].id,
        }))))
        .await);

    assert!(
        without.links.is_empty(),
        "the tool answers with the task, so the agent sees what is left"
    );
}

#[tokio::test]
async fn list_tasks_over_mcp_omits_plan_text_and_filters_by_column() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    create_ready(&h, &repository_id, "Queued").await;
    create_task(&h, &repository_id, "Drafting").await;

    let listed = ok_list(
        server(&h)
            .list_tasks(Parameters(request::<ListTasksRequest>(json!({
                "column": "ready",
            }))))
            .await,
    );

    assert_eq!(listed.tasks.len(), 1);
    assert_eq!(listed.tasks[0].title, "Queued");
    assert!(listed.tasks[0].has_plan);
    let wire = serde_json::to_value(&listed).expect("a DTO must always serialize");
    assert!(
        wire["tasks"][0].get("plan").is_none(),
        "fifty plans in one response is a context bomb (D16)"
    );
}

#[tokio::test]
async fn get_base_instructions_returns_the_template_unexpanded() {
    let h = TestContext::new().await;
    settings::set_base_instructions(&h.context, "Work on {{task.title}} in {{repo.name}}.")
        .await
        .expect("store base instructions");

    let view = match server(&h).get_base_instructions().await {
        Ok(Json(view)) => view,
        Err(error) => panic!("the tool must succeed: {:?}", error.0),
    };

    assert_eq!(
        view.base_instructions, "Work on {{task.title}} in {{repo.name}}.",
        "verbatim: composing needs a task, and the caller has none yet"
    );
    assert_eq!(
        view.template_variables,
        vec![
            "task.title",
            "task.branch",
            "task.links",
            "repo.name",
            "repo.default_branch",
        ]
    );
}

#[tokio::test]
async fn an_unknown_task_id_reaches_the_agent_as_readable_content() {
    // Task 010's "a specific, actionable error to the calling agent": not an
    // opaque protocol failure, and not "invalid input".
    let h = TestContext::new().await;

    let refused = as_result(
        server(&h)
            .get_task(Parameters(request::<GetTaskRequest>(
                json!({ "task_id": "nope" }),
            )))
            .await,
    );

    assert_eq!(refused.is_error, Some(true));
    assert_eq!(message(&refused), "no task with id nope");
    assert_eq!(
        refused.structured_content,
        Some(json!({ "code": "not_found", "message": "no task with id nope" }))
    );
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const NOW: &str = "2026-08-20T02:00:00+00:00";

/// A server on the same context the harness subscribed to, re-sourced the way
/// `mcp::build` does it.
fn server(h: &TestContext) -> RimaiaServer {
    RimaiaServer::new(h.context.with_source(MutationSource::Mcp), testing::doctor::environment())
}

/// The request an agent would send, deserialized through the real schema.
fn request<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("a well-formed request deserializes")
}

fn ok(result: Result<Json<TaskView>, rimaia_core::mcp::ToolError>) -> TaskView {
    match result {
        Ok(Json(view)) => view,
        Err(error) => panic!("the tool must succeed: {:?}", error.0),
    }
}

fn ok_list(result: Result<Json<TaskListView>, rimaia_core::mcp::ToolError>) -> TaskListView {
    match result {
        Ok(Json(listed)) => listed,
        Err(error) => panic!("the tool must succeed: {:?}", error.0),
    }
}

fn as_result<T>(result: Result<Json<T>, rimaia_core::mcp::ToolError>) -> CallToolResult
where
    T: serde::Serialize + schemars::JsonSchema + 'static,
{
    match result
        .into_call_tool_result()
        .expect("a tool error is never a protocol error")
    {
        CallToolResponse::Complete(result) => result,
        other => panic!("expected a completed result, got {other:?}"),
    }
}

fn message(result: &CallToolResult) -> String {
    result
        .content
        .first()
        .and_then(|block| block.as_text())
        .map(|text| text.text.clone())
        .expect("a tool error always carries its message as content")
}

/// The load-bearing assertion of this file: not "both failed", but "both
/// failed with the same payload", so neither door can grow a message of its
/// own.
fn assert_same_refusal<T>(ui: &Error, mcp: Result<Json<T>, rimaia_core::mcp::ToolError>)
where
    T: serde::Serialize + schemars::JsonSchema + 'static,
{
    let mcp = as_result(mcp);

    assert_eq!(mcp.is_error, Some(true), "the mcp path must refuse too");
    assert_eq!(
        mcp.structured_content,
        Some(serde_json::to_value(ui).expect("the ui path's payload")),
    );
    assert_eq!(message(&mcp), ui.to_string());
}

async fn seed_repository(pool: &SqlitePool, name: &str, path: &str) -> String {
    let id = rimaia_core::db::new_id();
    sqlx::query(
        "INSERT INTO repositories (id, name, path, default_branch, worktree_root, allow_unattended_runs, created_at)
         VALUES (?1, ?2, ?3, 'main', '/tmp/rimaia-worktrees', 0, ?4)",
    )
    .bind(&id)
    .bind(name)
    .bind(path)
    .bind(NOW)
    .execute(pool)
    .await
    .expect("seed a repository");
    id
}

async fn create_task(h: &TestContext, repository_id: &str, title: &str) -> rimaia_core::db::Task {
    tasks::create_task(
        &h.context,
        NewTask {
            repository_id: repository_id.to_string(),
            title: title.to_string(),
            plan: Some("a plan".to_string()),
            extra_instructions: None,
            column: None,
            links: vec![],
        },
    )
    .await
    .expect("create a task fixture")
}

async fn create_ready(h: &TestContext, repository_id: &str, title: &str) -> rimaia_core::db::Task {
    tasks::create_task(
        &h.context,
        NewTask {
            repository_id: repository_id.to_string(),
            title: title.to_string(),
            plan: Some("a plan".to_string()),
            extra_instructions: None,
            column: Some(BoardColumn::Ready),
            links: vec![],
        },
    )
    .await
    .expect("create a ready task fixture")
}

/// Seam-contract D13's blocker, written straight onto the row: task 007's
/// worktree service is not what this test is about.
async fn give_it_a_worktree(pool: &SqlitePool, task_id: &str) {
    sqlx::query("UPDATE tasks SET worktree_path = ?1, branch = ?2 WHERE id = ?3")
        .bind("/tmp/rimaia-worktrees/filed-wrong")
        .bind("rimaia/filed-wrong")
        .bind(task_id)
        .execute(pool)
        .await
        .expect("record a worktree");
}

async fn ready_column(h: &TestContext, repository_id: &str) -> Vec<String> {
    tasks::list_tasks(
        &h.context,
        rimaia_core::tasks::TaskFilter {
            repository_id: Some(repository_id.to_string()),
            column: Some(BoardColumn::Ready),
            run_state: None,
        },
    )
    .await
    .expect("read the ready column")
    .into_iter()
    .map(|summary| summary.task.id)
    .collect()
}

/// A plan of the size task 010's acceptance criterion names, carrying every
/// character class that has ever eaten a round trip.
fn long_plan() -> String {
    let paragraph = "Read `crates/core/src/tasks/service.rs` first — the transaction \
boundary is what matters here, not the SQL. Note the em dash, the “curly quotes”, the naïve \
café, and the trailing space at the end of this line. \n\n\
```rust\nlet plan = task.plan.as_deref().unwrap_or_default();\nassert!(!plan.is_empty());\n```\n\n\
Then check that `position` is still fractional: board order *is* execution order.\n\n";

    let mut plan = String::from("# The plan\n\n");
    for step in 1..=60 {
        plan.push_str(&format!("## Step {step}\n\n"));
        plan.push_str(paragraph);
    }
    plan
}
