//! The run-scope boundary (ADR-0006's 2026-08-28 amendment, seam-contract
//! D17.4).
//!
//! Two levels, deliberately. The allow table is exercised by calling the real
//! handlers on a `RimaiaServer::scoped(..)`, which is where the refusals are
//! decided and where their payloads are worth pinning. Routing is exercised
//! against a **real bound server over real HTTP**, because "an unknown token is
//! a bare 404" is a statement about the router and nothing below it can
//! establish it.
//!
//! `every_registered_tool_has_a_run_scope_decision` is the one that earns its
//! keep over time: an eleventh, twelfth or thirteenth tool cannot reach the
//! wire without someone having said what a run may do with it.

use rimaia_core::db::{BoardColumn, MutationSource};
use rimaia_core::mcp::requests::{
    CreateTaskRequest, GetTaskRequest, ListTasksRequest, MoveTaskRequest,
    SetTaskDependenciesRequest, SetTaskStrategyRequest, UpdateTaskRequest,
};
use rimaia_core::mcp::responses::{TaskListView, TaskView};
use rimaia_core::mcp::{
    self, McpHandle, RimaiaServer, RunAccess, RunGrant, RunHandles, RunScope, Tool,
};
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::TestContext;
use rimaia_core::Error;

use pretty_assertions::assert_eq;
use reqwest::StatusCode;
use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};
use sqlx::SqlitePool;

// ---------------------------------------------------------------------------
// The table, and the thing that stops it drifting
// ---------------------------------------------------------------------------

#[test]
fn every_registered_tool_has_a_run_scope_decision() {
    // The anti-drift test. A tool added to `server.rs` with no entry in `Tool`
    // would otherwise reach a run with whatever the fall-through happened to
    // be; here it reddens the build until someone puts it in ADR-0006's table.
    let undecided: Vec<String> = RimaiaServer::tool_router()
        .list_all()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .filter(|name| Tool::from_name(name).is_none())
        .collect();

    assert_eq!(
        undecided,
        Vec::<String>::new(),
        "a registered tool with no run-scope decision: add it to `mcp::scope::Tool` and to \
         ADR-0006's amendment table"
    );

    // And the other direction, which is now symmetric: `set_task_strategy` had
    // a decision one commit before it had a handler — the decision is
    // ADR-0006's to make, not the handler's — and the handler has landed, so
    // every tool the table declares is a tool that exists. A name left here
    // after its handler is removed would advertise a refusal for something
    // nobody can call.
    let registered: Vec<String> = RimaiaServer::tool_router()
        .list_all()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    let unregistered: Vec<Tool> = Tool::ALL
        .into_iter()
        .filter(|tool| !registered.iter().any(|name| name == tool.as_str()))
        .collect();

    assert!(
        unregistered.is_empty(),
        "declared but never registered: {unregistered:?}"
    );
}

#[test]
fn the_operator_endpoint_keeps_every_tool_it_had_before_task_020() {
    // Task 020 adds a narrower door; it takes nothing away from the wide one.
    // Asserted over the whole table rather than over the four a run may not
    // call, so a future `RunAccess` variant cannot quietly start refusing the
    // operator too.
    for tool in Tool::ALL {
        RunScope::Operator
            .authorize(tool, None)
            .unwrap_or_else(|error| panic!("{} refused for the operator: {error}", tool.as_str()));
        RunScope::Operator
            .authorize(tool, Some("any-task-at-all"))
            .unwrap_or_else(|error| panic!("{} refused for the operator: {error}", tool.as_str()));
    }

    // And the table itself, so the three groups ADR-0006's amendment names are
    // legible in one place rather than only in a match arm.
    for tool in Tool::ALL {
        let expected = match tool {
            Tool::AddTaskLink
            | Tool::GetTask
            | Tool::RemoveTaskLink
            | Tool::SetTaskStrategy
            | Tool::UpdateTask => RunAccess::OwnTaskOnly,
            Tool::GetBaseInstructions | Tool::ListRepositories => RunAccess::Unscoped,
            Tool::CreateTask | Tool::ListTasks | Tool::MoveTask | Tool::SetTaskDependencies => {
                RunAccess::Refused
            }
        };
        assert_eq!(tool.run_access(), expected, "{}", tool.as_str());
    }
}

// ---------------------------------------------------------------------------
// What a run may do
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_run_scoped_handle_updates_its_own_task() {
    // The control for every refusal below: without it they could all pass
    // because a scoped server refuses everything.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;

    let updated = ok(scoped(&h, &mine.id)
        .update_task(Parameters(request::<UpdateTaskRequest>(json!({
            "task_id": mine.id,
            "extra_instructions": "Skip the migration",
        }))))
        .await);

    assert_eq!(
        updated.extra_instructions.as_deref(),
        Some("Skip the migration")
    );

    // And reading it back, which is the other half of what a run is for.
    let read = ok(scoped(&h, &mine.id)
        .get_task(Parameters(request::<GetTaskRequest>(
            json!({ "task_id": mine.id }),
        )))
        .await);
    assert_eq!(read.id, mine.id);
}

#[tokio::test]
async fn a_run_scoped_handle_reads_the_instructions_it_is_working_under() {
    // The `Unscoped` row of the table: neither tool takes a task, and a run has
    // a legitimate use for both.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;

    scoped(&h, &mine.id)
        .get_base_instructions()
        .await
        .map_err(|error| error.0)
        .expect("a run may read the standing instructions");
    scoped(&h, &mine.id)
        .list_repositories()
        .await
        .map_err(|error| error.0)
        .expect("a run may see the repositories it might be looking at");
}

// ---------------------------------------------------------------------------
// What a run may not do
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_run_scoped_handle_cannot_update_another_task() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;
    let theirs = create_task(&h, &repository_id, "Someone else's").await;

    let refused = as_result(
        scoped(&h, &mine.id)
            .update_task(Parameters(request::<UpdateTaskRequest>(json!({
                "task_id": theirs.id,
                "title": "Hijacked",
            }))))
            .await,
    );

    assert_refusal(
        &refused,
        &format!(
            "this handle is scoped to task {mine}, so update_task cannot be called against task \
             {theirs}.",
            mine = mine.id,
            theirs = theirs.id,
        ),
    );

    // And nothing was written on the way to being refused.
    let untouched = tasks::get_task(&h.context, &theirs.id)
        .await
        .expect("the other task is still there");
    assert_eq!(untouched.task.title, "Someone else's");
}

#[tokio::test]
async fn a_run_scoped_handle_cannot_record_a_strategy_for_another_task() {
    // The eleventh tool through the narrow door, and the one case where the
    // *order* of the two checks shows: `set_task_strategy` refuses a task that
    // is not in `planned` mode as well, and the other card is not, so a handler
    // that called the service before authorizing would refuse this — with the
    // wrong sentence, and after having read a card it was never allowed to name.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;
    let theirs = create_task(&h, &repository_id, "Someone else's").await;

    let refused = as_result(
        scoped(&h, &mine.id)
            .set_task_strategy(Parameters(request::<SetTaskStrategyRequest>(json!({
                "task_id": theirs.id,
                "model": "opus",
            }))))
            .await,
    );

    assert_refusal(
        &refused,
        &format!(
            "this handle is scoped to task {mine}, so set_task_strategy cannot be called against \
             task {theirs}.",
            mine = mine.id,
            theirs = theirs.id,
        ),
    );
}

#[tokio::test]
async fn a_run_scoped_handle_cannot_move_another_task() {
    // Task 020's acceptance criterion 4. `move_task` is the sharpest case: the
    // runner owns where a card lands when a run finishes, so a run moving a
    // card — anyone's — would be marking its own homework.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;
    let theirs = create_task(&h, &repository_id, "Someone else's").await;

    let refused = as_result(
        scoped(&h, &mine.id)
            .move_task(Parameters(request::<MoveTaskRequest>(json!({
                "task_id": theirs.id,
                "column": "done",
            }))))
            .await,
    );

    assert_refusal(&refused, &not_available("move_task", &mine.id));

    // Its own card is refused too, and by the same sentence: `move_task` is off
    // the run's table entirely, not merely narrowed to its own task.
    let own = as_result(
        scoped(&h, &mine.id)
            .move_task(Parameters(request::<MoveTaskRequest>(json!({
                "task_id": mine.id,
                "column": "done",
            }))))
            .await,
    );
    assert_refusal(&own, &not_available("move_task", &mine.id));

    assert_eq!(
        tasks::get_task(&h.context, &theirs.id)
            .await
            .expect("read it back")
            .task
            .column,
        BoardColumn::NotReady,
        "nothing moved"
    );
}

#[tokio::test]
async fn a_run_scoped_handle_cannot_create_a_task() {
    // A run spawning work is orchestration, which ADR-0016 declines to build.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;

    let refused = as_result(
        scoped(&h, &mine.id)
            .create_task(Parameters(request::<CreateTaskRequest>(json!({
                "repository_id": repository_id,
                "title": "Spawned by a run",
                "plan": "a plan",
            }))))
            .await,
    );

    assert_refusal(&refused, &not_available("create_task", &mine.id));
    assert_eq!(
        board(&h, &repository_id).await.len(),
        1,
        "the board still holds only the task the run was started for"
    );
}

#[tokio::test]
async fn a_run_scoped_handle_cannot_reorder_the_work_it_depends_on() {
    // The other half of the orchestration refusal, and the one that would be
    // easy to read as harmless: a run that could declare dependencies could
    // decide what runs next.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;
    let other = create_task(&h, &repository_id, "The API").await;

    let refused = as_result(
        scoped(&h, &mine.id)
            .set_task_dependencies(Parameters(request::<SetTaskDependenciesRequest>(json!({
                "task_id": mine.id,
                "depends_on": [other.id],
            }))))
            .await,
    );

    assert_refusal(&refused, &not_available("set_task_dependencies", &mine.id));
}

#[tokio::test]
async fn a_run_scoped_handle_cannot_list_the_board() {
    // Not a write, and refused anyway: a run has no business enumerating
    // someone's board, and every card it could read carries a title the
    // operator did not hand it.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;
    create_task(&h, &repository_id, "Someone else's").await;

    let refused = as_result(
        scoped(&h, &mine.id)
            .list_tasks(Parameters(request::<ListTasksRequest>(json!({}))))
            .await,
    );

    assert_refusal(&refused, &not_available("list_tasks", &mine.id));
}

#[tokio::test]
async fn a_scope_refusal_carries_the_same_payload_as_every_other_refusal() {
    // The assertion `tests/mcp_tools.rs` makes about every shared invariant,
    // applied to the one refusal that has no counterpart on the other door: a
    // scope check that invented its own shape would be the single refusal an
    // agent could not handle like the rest.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;
    let theirs = create_task(&h, &repository_id, "Someone else's").await;

    let refused = as_result(
        scoped(&h, &mine.id)
            .get_task(Parameters(request::<GetTaskRequest>(
                json!({ "task_id": theirs.id }),
            )))
            .await,
    );

    let same_shape = Error::invalid(format!(
        "this handle is scoped to task {mine}, so get_task cannot be called against task {theirs}.",
        mine = mine.id,
        theirs = theirs.id,
    ));

    assert_eq!(refused.is_error, Some(true));
    assert_eq!(
        refused.structured_content,
        Some(serde_json::to_value(&same_shape).expect("the tauri boundary's payload")),
        "a scope refusal is `{{ code, message }}` like every other one"
    );
    assert_eq!(message(&refused), same_shape.to_string());
}

// ---------------------------------------------------------------------------
// Routing, against a real bound server
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unknown_token_is_not_routed_at_all() {
    // Not "the tools all refuse" — the request never reaches a tool. A bare 404
    // with no body, so this route cannot be used to find out which runs exist.
    let h = TestContext::new().await;
    let handles = RunHandles::default();
    let (handle, server) = serving(&h, &handles).await;
    let address = handle.status().bound_address.expect("a bound address");

    let answer = post_tools_list(&format!("http://{address}/mcp/run/not-a-real-token")).await;

    assert_eq!(answer.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        answer.text().await.expect("a body, even an empty one"),
        "",
        "an empty body is the whole answer: no message names the token, and none \
         distinguishes 'never existed' from 'revoked'"
    );

    // And the operator's own door is untouched by any of this.
    assert_eq!(
        post_tools_list(&format!("http://{address}/mcp"))
            .await
            .status(),
        StatusCode::OK,
        "ADR-0006 fixes /mcp, and task 020 does not move it"
    );

    handle.shutdown();
    server.await.expect("the server task ends");
}

#[tokio::test]
async fn a_token_stops_working_when_its_run_ends() {
    // The RAII half. A cancelled or panicking run unwinds through `RunGrant`'s
    // `Drop`, so there is no path that leaves a live handle to a task behind.
    let h = TestContext::new().await;
    let handles = RunHandles::default();
    let (handle, server) = serving(&h, &handles).await;
    let address = handle.status().bound_address.expect("a bound address");
    assert_eq!(
        handles.endpoint(),
        Some(format!("http://{address}")),
        "`build` tells the handles where it landed, on every bind"
    );

    let url = {
        let grant = handles.grant("task-1");
        let url = scoped_url(&handles, &grant);

        assert_eq!(
            post_tools_list(&url).await.status(),
            StatusCode::OK,
            "while the run holds its grant"
        );

        url
        // The grant drops here, which is what "the run ended" means.
    };

    assert_eq!(post_tools_list(&url).await.status(), StatusCode::NOT_FOUND);

    handle.shutdown();
    server.await.expect("the server task ends");
}

#[tokio::test]
async fn a_real_client_at_a_scoped_url_is_refused_a_task_that_is_not_its_own() {
    // The direct-call tests above establish the table; this establishes that a
    // run can actually reach it, and that the refusal survives the wire with
    // its message intact.
    //
    // It is here rather than left to task 020's runner test because `dispatch`
    // builds a fresh `StreamableHttpService` per request: `initialize` and the
    // `tools/call` that follows it are two separate POSTs with nothing carried
    // between them, and if that ever stopped working the symptom would surface
    // in the runner, several layers from the cause.
    let h = TestContext::new().await;
    let handles = RunHandles::default();
    let (handle, server) = serving(&h, &handles).await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;
    let theirs = create_task(&h, &repository_id, "Someone else's").await;

    let grant = handles.grant(&mine.id);
    let url = scoped_url(&handles, &grant);
    let client = ()
        .serve(StreamableHttpClientTransport::with_client(
            reqwest::Client::default(),
            StreamableHttpClientTransportConfig::with_uri(url.clone()),
        ))
        .await
        .expect("a run's own handle answers `initialize`");

    let own = call_get_task(&client, &mine.id).await;
    assert_eq!(own.is_error, Some(false), "its own card is served");
    assert_eq!(
        own.structured_content
            .as_ref()
            .and_then(|view| view["id"].as_str()),
        Some(mine.id.as_str())
    );

    let other = call_get_task(&client, &theirs.id).await;
    assert_refusal(
        &other,
        &format!(
            "this handle is scoped to task {mine}, so get_task cannot be called against task \
             {theirs}.",
            mine = mine.id,
            theirs = theirs.id,
        ),
    );

    let _ = client.cancel().await;
    handle.shutdown();
    server.await.expect("the server task ends");
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const NOW: &str = "2026-08-20T02:00:00+00:00";

/// A server reached the way a run reaches it, on a context re-sourced the way
/// `mcp::build` does it.
fn scoped(h: &TestContext, task_id: &str) -> RimaiaServer {
    RimaiaServer::scoped(h.context.with_source(MutationSource::Mcp), task_id)
}

/// A bound server on an OS-chosen port, already spawned, sharing `handles` with
/// the caller the way the shell shares them with the runner.
async fn serving(
    h: &TestContext,
    handles: &RunHandles,
) -> (McpHandle, tokio::task::JoinHandle<()>) {
    let (handle, task) = mcp::build(h.context.clone(), 0, handles.clone()).await;
    (handle, tokio::spawn(task.run()))
}

/// The URL a run is actually handed, read back out of its `--mcp-config`.
///
/// Deliberately not one a test formats: what the runner puts in argv has to be
/// what the router serves, and that is exactly the seam a hand-written URL
/// would hide.
fn scoped_url(handles: &RunHandles, grant: &RunGrant) -> String {
    let config: Value = serde_json::from_str(
        &handles
            .mcp_config_json(grant)
            .expect("an endpoint is bound"),
    )
    .expect("the config is JSON");

    config["mcpServers"]["rimaia"]["url"]
        .as_str()
        .expect("the config names a url")
        .to_string()
}

/// One `get_task`, called the way an agent calls it.
///
/// A refusal comes back as a `CallToolResult` with `is_error`, never as a
/// transport error, which is what lets the caller assert on the same shape the
/// direct-call tests assert on.
async fn call_get_task(client: &RunningService<RoleClient, ()>, task_id: &str) -> CallToolResult {
    client
        .call_tool(
            CallToolRequestParams::new("get_task").with_arguments(
                json!({ "task_id": task_id })
                    .as_object()
                    .cloned()
                    .expect("an object"),
            ),
        )
        .await
        .expect("the call itself completes")
}

/// One JSON-RPC `tools/list`, posted with the headers the streamable-HTTP
/// transport sends.
///
/// Raw `reqwest` rather than rmcp's client because what these two tests assert
/// is the HTTP status: "a bare 404" is the whole answer for a token that does
/// not resolve, and an rmcp error would say only that something went wrong.
async fn post_tools_list(url: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(url)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .send()
        .await
        .expect("the server answers")
}

/// The sentence a run gets for a tool that is off its table entirely.
fn not_available(tool: &str, task_id: &str) -> String {
    format!(
        "{tool} is not available to a run: this handle is scoped to task {task_id}, and a run may \
         only read and amend its own task."
    )
}

fn assert_refusal(result: &CallToolResult, expected: &str) {
    assert_eq!(result.is_error, Some(true), "it must refuse");
    assert_eq!(message(result), expected);
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

/// Read through the *operator's* door, so a test about what a run cannot see
/// does not depend on the thing it is asserting about.
async fn board(h: &TestContext, repository_id: &str) -> Vec<String> {
    let listed: TaskListView = match RimaiaServer::new(h.context.clone())
        .list_tasks(Parameters(request::<ListTasksRequest>(
            json!({ "repository_id": repository_id }),
        )))
        .await
    {
        Ok(Json(listed)) => listed,
        Err(error) => panic!("the operator may always list: {:?}", error.0),
    };

    listed.tasks.into_iter().map(|task| task.id).collect()
}
