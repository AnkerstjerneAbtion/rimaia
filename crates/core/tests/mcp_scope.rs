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

use rimaia_core::db::{BoardColumn, MutationSource, ScheduleMode};
use rimaia_core::mcp::requests::{
    CreateTaskRequest, GetStrategyDefaultsRequest, GetTaskRequest, ListTasksRequest,
    MoveTaskRequest, ScheduleConfigRequest, ScheduleRequest, SetMaxConcurrencyRequest,
    SetRepositoryMaxConcurrencyRequest, SetScheduleEnabledRequest, SetScheduleModeRequest,
    SetStrategyApprovalRequest, SetStrategyCatalogueRequest, SetStrategyDefaultsRequest,
    SetTaskDependenciesRequest, SetTaskStrategyRequest, TaskStrategyRequest, UpdateScheduleRequest,
    UpdateTaskRequest,
};
use rimaia_core::mcp::responses::{
    PreflightView, ScheduleDeletedView, ScheduleListView, ScheduleView, StrategyApprovalView,
    TaskListView, TaskView, TimezoneListView,
};
use rimaia_core::mcp::{
    self, McpHandle, RimaiaServer, RunAccess, RunGrant, RunHandles, RunScope, Tool,
};
use rimaia_core::repo;
use rimaia_core::schedule::{self, ScheduleInput};
use rimaia_core::scheduler::{capacity, CONCURRENCY_CEILING, DEFAULT_MAX_CONCURRENCY};
use rimaia_core::strategy::{
    self, Catalogue, CatalogueEntry, StrategyApproval, StrategyDefaults, DEFAULT_CATALOGUE_JSON,
};
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::{self, TestContext};
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
            Tool::CreateTask
            | Tool::ListTasks
            | Tool::MoveTask
            | Tool::SetTaskDependencies
            // ADR-0021's permanent refusal: everything that reconfigures the
            // installation, plus accepting a proposal, which speaks for a human.
            | Tool::AcceptTaskStrategy
            | Tool::ClearTaskStrategy
            | Tool::GetStrategyApproval
            | Tool::GetStrategyCatalogue
            | Tool::GetStrategyDefaults
            | Tool::SetStrategyApproval
            | Tool::SetStrategyCatalogue
            | Tool::SetStrategyDefaults
            // Task 012's four, one layer out: how many runs this installation
            // starts at once, and how many of them one repository holds, are
            // properties of the run configuration (ADR-0010) — which is what
            // that refusal names.
            | Tool::GetRunCapacity
            | Tool::SetScheduleMode
            | Tool::SetMaxConcurrency
            | Tool::SetRepositoryMaxConcurrency
            // Task 014's, and it is the first of ADR-0021 point 4's two
            // refusals rather than the second: ending a retry loop is a
            // statement about whether the work will be attempted at all, and a
            // run abandoning the task it was started for would be marking its
            // own homework in the other direction.
            | Tool::GiveUpOnTask
            // Task 018. The same clause read one step wider: these describe or
            // configure the *installation*, not any task. `run_doctor` in
            // particular is a reconnaissance surface — which binaries are on
            // the operator's PATH, whether they are signed in, where every
            // registered repository sits on disk — and every remediation it
            // returns is something only a human at the machine can do.
            | Tool::RunDoctor
            | Tool::DismissOnboarding
            // Task 013's seven, and these are *both* of ADR-0021 point 4's
            // permanent refusals at once rather than one of them: a schedule
            // spawns runs — it is the thing that starts the queue at 22:00 —
            // and it reconfigures the installation, because an open window
            // overrides the mode and concurrency the whole queue runs under.
            // `list_timezones` reads nothing and is refused anyway: it exists
            // only to fill in a field of the tools above it, so a run that may
            // not use those has no use for it.
            | Tool::ListSchedules
            | Tool::CreateSchedule
            | Tool::UpdateSchedule
            | Tool::SetScheduleEnabled
            | Tool::DeleteSchedule
            | Tool::PreviewSchedulePreflight
            | Tool::ListTimezones => RunAccess::Refused,
            // Task 016. The setting reconfigures the installation, which is the
            // same clause; the two reads are refused on `list_tasks`'s ground
            // instead — an inventory is by construction an enumeration of every
            // task's directory, and a run's entitlement is its own.
            | Tool::ListWorktrees
            | Tool::GetWorktreeAutoCleanup
            | Tool::SetWorktreeAutoCleanup => RunAccess::Refused,
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
// ADR-0021's eight, through their real handlers
//
// `the_operator_endpoint_keeps_every_tool_it_had_before_task_020` pins the
// *table*; these pin the handlers. The difference is the whole failure mode: a
// tool that forgot its `authorize` line would satisfy the table and still be
// callable by a run, because nothing else on the path checks. So every one of
// them is called on a scoped server here, and the refusal is asserted by
// sentence rather than by "it errored".
// ---------------------------------------------------------------------------

#[tokio::test]
async fn nothing_adr_0021_added_is_reachable_from_a_run() {
    // Seven of these reconfigure the installation — the model catalogue, the
    // defaults every card inherits, whether a proposal waits for a human — and
    // a run rewriting any of them changes what every *other* task in the queue
    // runs as. `accept_task_strategy` is refused for a different reason: it
    // speaks for a human, and a planner accepting its own proposal is marking
    // its own homework.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;
    let run = scoped(&h, &mine.id);

    assert_refusal(
        &as_result(run.get_strategy_catalogue().await),
        &not_available("get_strategy_catalogue", &mine.id),
    );
    assert_refusal(
        &as_result(
            run.set_strategy_catalogue(Parameters(request::<SetStrategyCatalogueRequest>(
                json!({ "catalogue": DEFAULT_CATALOGUE_JSON }),
            )))
            .await,
        ),
        &not_available("set_strategy_catalogue", &mine.id),
    );
    assert_refusal(
        &as_result(
            run.get_strategy_defaults(Parameters(request::<GetStrategyDefaultsRequest>(json!({}))))
                .await,
        ),
        &not_available("get_strategy_defaults", &mine.id),
    );
    assert_refusal(
        &as_result(
            run.set_strategy_defaults(Parameters(request::<SetStrategyDefaultsRequest>(json!({
                "mode": "manual",
                "model": "opus",
            }))))
            .await,
        ),
        &not_available("set_strategy_defaults", &mine.id),
    );
    assert_refusal(
        &as_result(run.get_strategy_approval().await),
        &not_available("get_strategy_approval", &mine.id),
    );
    assert_refusal(
        &as_result(
            run.set_strategy_approval(Parameters(request::<SetStrategyApprovalRequest>(
                json!({ "approval": "manual" }),
            )))
            .await,
        ),
        &not_available("set_strategy_approval", &mine.id),
    );

    // Both of these name a task, and both are refused for the run's *own* card:
    // they are off its table entirely, not merely narrowed to its own task.
    assert_refusal(
        &as_result(
            run.accept_task_strategy(Parameters(request::<TaskStrategyRequest>(
                json!({ "task_id": mine.id }),
            )))
            .await,
        ),
        &not_available("accept_task_strategy", &mine.id),
    );
    assert_refusal(
        &as_result(
            run.clear_task_strategy(Parameters(request::<TaskStrategyRequest>(
                json!({ "task_id": mine.id }),
            )))
            .await,
        ),
        &not_available("clear_task_strategy", &mine.id),
    );

    // And none of them wrote anything on the way to being refused.
    assert_eq!(
        strategy::settings::approval(&h.context.pool)
            .await
            .expect("read the approval setting"),
        StrategyApproval::Automatic,
    );
    assert_eq!(
        strategy::settings::global_default(&h.context.pool)
            .await
            .expect("read the global defaults"),
        StrategyDefaults::default(),
    );
}

#[tokio::test]
async fn nothing_task_012_added_is_reachable_from_a_run_either() {
    // The same permanent refusal one layer out (ADR-0021 point 4, ADR-0010).
    // How many runs this installation starts at once decides what the night
    // costs, and a repository's own cap is the thing keeping a second agent out
    // of this run's ports and test databases — a run raising it would be
    // removing its own protection. The *read* is refused with the writes
    // because a run cannot act on the answer.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;
    let run = scoped(&h, &mine.id);

    assert_refusal(
        &as_result(run.get_run_capacity().await),
        &not_available("get_run_capacity", &mine.id),
    );
    assert_refusal(
        &as_result(
            run.set_schedule_mode(Parameters(request::<SetScheduleModeRequest>(
                json!({ "mode": "parallel" }),
            )))
            .await,
        ),
        &not_available("set_schedule_mode", &mine.id),
    );
    assert_refusal(
        &as_result(
            run.set_max_concurrency(Parameters(request::<SetMaxConcurrencyRequest>(
                json!({ "max_concurrency": 8 }),
            )))
            .await,
        ),
        &not_available("set_max_concurrency", &mine.id),
    );
    assert_refusal(
        &as_result(
            run.set_repository_max_concurrency(Parameters(request::<
                SetRepositoryMaxConcurrencyRequest,
            >(json!({
                "repository_id": repository_id,
                "max_concurrency": 4,
            }))))
            .await,
        ),
        &not_available("set_repository_max_concurrency", &mine.id),
    );

    // And none of them wrote anything on the way to being refused.
    let capacity = capacity::configured(&h.context.pool)
        .await
        .expect("read the capacity back");
    assert_eq!(capacity.mode, ScheduleMode::Sequential);
    assert_eq!(capacity.max_concurrency, DEFAULT_MAX_CONCURRENCY);
    assert_eq!(
        repo::get(&h.context, &repository_id)
            .await
            .expect("read the repository back")
            .max_concurrency,
        1,
    );
}

#[tokio::test]
async fn nothing_task_013_added_is_reachable_from_a_run_either() {
    // Both of ADR-0021 point 4's permanent refusals at once. A schedule spawns
    // runs — it is the thing that starts the queue at 22:00 — *and* it
    // reconfigures the installation, because an open window overrides the mode
    // and concurrency the whole queue runs under. A run that could write one
    // could arrange to be run again, on its own terms, tomorrow night.
    //
    // Called through the handlers rather than only checked against the table,
    // because a tool that forgot its `authorize` line would satisfy the table
    // and still be callable by a run.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;
    let existing = schedule::create(&h.context, nightly())
        .await
        .expect("a schedule the operator made");
    let run = scoped(&h, &mine.id);

    let config = json!({
        "name": "Mine, nightly",
        "mode": "parallel",
        "max_concurrency": 4,
        "timezone": "Europe/Copenhagen",
        "cron": "0 22 * * *",
        "stop_at": "06:00",
        "enabled": true,
    });

    assert_refusal(
        &as_result(run.list_schedules().await),
        &not_available("list_schedules", &mine.id),
    );
    assert_refusal(
        &as_result(run.list_timezones().await),
        &not_available("list_timezones", &mine.id),
    );
    assert_refusal(
        &as_result(
            run.create_schedule(Parameters(request::<ScheduleConfigRequest>(config.clone())))
                .await,
        ),
        &not_available("create_schedule", &mine.id),
    );
    let mut update = config.clone();
    update["schedule_id"] = json!(existing.id);
    assert_refusal(
        &as_result(
            run.update_schedule(Parameters(request::<UpdateScheduleRequest>(update)))
                .await,
        ),
        &not_available("update_schedule", &mine.id),
    );
    assert_refusal(
        &as_result(
            run.set_schedule_enabled(Parameters(request::<SetScheduleEnabledRequest>(
                json!({ "schedule_id": existing.id, "enabled": false }),
            )))
            .await,
        ),
        &not_available("set_schedule_enabled", &mine.id),
    );
    assert_refusal(
        &as_result(
            run.delete_schedule(Parameters(request::<ScheduleRequest>(
                json!({ "schedule_id": existing.id }),
            )))
            .await,
        ),
        &not_available("delete_schedule", &mine.id),
    );
    assert_refusal(
        &as_result(
            run.preview_schedule_preflight(Parameters(request::<ScheduleRequest>(
                json!({ "schedule_id": existing.id }),
            )))
            .await,
        ),
        &not_available("preview_schedule_preflight", &mine.id),
    );

    // And none of them wrote anything on the way to being refused: the one
    // schedule that existed is still there, still enabled, still unedited.
    let after = schedule::list(&h.context).await.expect("read them back");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].schedule.id, existing.id);
    assert_eq!(after[0].schedule.name, "Nightly");
    assert!(after[0].schedule.enabled);
    assert_eq!(after[0].schedule.mode, ScheduleMode::Sequential);
}

#[tokio::test]
async fn the_operator_reads_and_writes_schedules_over_mcp() {
    // Each write round-tripped through the reader rather than merely called: a
    // setter that stored nothing would pass a smoke test.
    let h = TestContext::new().await;
    let operator = RimaiaServer::new(h.context.clone(), testing::doctor::environment());

    let created = json_of::<ScheduleView>(
        operator
            .create_schedule(Parameters(request::<ScheduleConfigRequest>(json!({
                "name": "Nightly",
                "mode": "parallel",
                "max_concurrency": 3,
                "timezone": "Europe/Copenhagen",
                "cron": "0 22 * * *",
                "stop_at": "06:00",
                "enabled": true,
            }))))
            .await,
    );
    assert_eq!(created.name, "Nightly");
    assert_eq!(created.timezone.as_deref(), Some("Europe/Copenhagen"));

    let listed = json_of::<ScheduleListView>(operator.list_schedules().await);
    assert_eq!(listed.schedules.len(), 1);
    assert_eq!(
        listed.schedules[0].next_fire_at,
        Some(
            "2026-08-20T20:00:00Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .expect("a literal timestamp"),
        ),
        "the list is the one place the next fire time is computed",
    );

    let disabled = json_of::<ScheduleView>(
        operator
            .set_schedule_enabled(Parameters(request::<SetScheduleEnabledRequest>(json!({
                "schedule_id": created.id,
                "enabled": false,
            }))))
            .await,
    );
    assert!(!disabled.enabled);
    assert!(
        disabled.cron.is_some(),
        "disabling keeps the configuration — that is the whole difference from deleting",
    );

    let renamed = json_of::<ScheduleView>(
        operator
            .update_schedule(Parameters(request::<UpdateScheduleRequest>(json!({
                "schedule_id": created.id,
                "name": "Weeknights",
                "mode": "sequential",
                "max_concurrency": 2,
                "timezone": "Europe/Copenhagen",
                "cron": "0 22 * * 1-5",
                "stop_at": "06:00",
                "enabled": true,
            }))))
            .await,
    );
    assert_eq!(renamed.name, "Weeknights");
    assert_eq!(renamed.cron.as_deref(), Some("0 22 * * 1-5"));

    let preview = json_of::<PreflightView>(
        operator
            .preview_schedule_preflight(Parameters(request::<ScheduleRequest>(json!({
                "schedule_id": created.id,
            }))))
            .await,
    );
    assert_eq!(preview.schedule_name, "Weeknights");
    assert_eq!(preview.will_start, 0, "an empty board starts nothing");

    let timezones = json_of::<TimezoneListView>(operator.list_timezones().await);
    assert!(timezones
        .timezones
        .iter()
        .any(|name| name == "Europe/Copenhagen"));

    let deleted = json_of::<ScheduleDeletedView>(
        operator
            .delete_schedule(Parameters(request::<ScheduleRequest>(json!({
                "schedule_id": created.id,
            }))))
            .await,
    );
    assert!(deleted.deleted);
    assert_eq!(
        json_of::<ScheduleListView>(operator.list_schedules().await)
            .schedules
            .len(),
        0,
    );
}

#[tokio::test]
async fn a_schedule_the_operator_configures_badly_is_refused_with_the_reason() {
    // The refusals are the service's, not the adapter's (ADR-0006), so the
    // sentence a tool caller reads is the sentence the panel reads.
    let h = TestContext::new().await;
    let operator = RimaiaServer::new(h.context.clone(), testing::doctor::environment());

    let refused = as_result(
        operator
            .create_schedule(Parameters(request::<ScheduleConfigRequest>(json!({
                "name": "Nightly",
                "mode": "sequential",
                "max_concurrency": 2,
                "timezone": "CEST",
                "cron": "0 22 * * *",
                "enabled": true,
            }))))
            .await,
    );

    assert_eq!(refused.is_error, Some(true));
    assert!(
        message(&refused).contains("IANA"),
        "an abbreviation is not a zone: {}",
        message(&refused),
    );
}

/// The schedule every refusal test above leaves untouched.
fn nightly() -> ScheduleInput {
    ScheduleInput {
        name: "Nightly".to_string(),
        mode: ScheduleMode::Sequential,
        max_concurrency: 2,
        timezone: "Europe/Copenhagen".to_string(),
        cron: Some("0 22 * * *".to_string()),
        start_at: None,
        stop_at: Some("06:00".to_string()),
        enabled: true,
    }
}

#[tokio::test]
async fn the_operator_reads_and_writes_the_run_capacity_over_mcp() {
    // ADR-0021's premise applied to task 012's own surface: each setter is
    // round-tripped through the reader rather than merely called, because a
    // setter that stored nothing would pass a smoke test.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let operator = RimaiaServer::new(h.context.clone(), testing::doctor::environment());

    let after_mode = operator
        .set_schedule_mode(Parameters(request::<SetScheduleModeRequest>(
            json!({ "mode": "parallel" }),
        )))
        .await
        .expect("the operator may reconfigure the queue")
        .0;
    assert_eq!(after_mode.mode, ScheduleMode::Parallel);

    let after_limit = operator
        .set_max_concurrency(Parameters(request::<SetMaxConcurrencyRequest>(
            json!({ "max_concurrency": 3 }),
        )))
        .await
        .expect("set the limit")
        .0;
    assert_eq!(after_limit.max_concurrency, 3);

    let read_back = operator
        .get_run_capacity()
        .await
        .expect("read it back through the other tool")
        .0;
    assert_eq!(read_back, after_limit);
    assert_eq!(
        read_back.ceiling, CONCURRENCY_CEILING,
        "the ceiling is reported so a caller can bound its own input",
    );

    let repository = operator
        .set_repository_max_concurrency(Parameters(request::<SetRepositoryMaxConcurrencyRequest>(
            json!({
                "repository_id": repository_id,
                "max_concurrency": 2,
            }),
        )))
        .await
        .expect("raise one repository's cap")
        .0;
    assert_eq!(repository.max_concurrency, 2);

    // A value no form would send is refused with a sentence rather than
    // clamped — the write side of the read-tolerant/write-strict asymmetry.
    let refused = as_result(
        operator
            .set_max_concurrency(Parameters(request::<SetMaxConcurrencyRequest>(
                json!({ "max_concurrency": 99 }),
            )))
            .await,
    );
    assert_eq!(refused.is_error, Some(true), "above the ceiling");
    assert!(
        message(&refused).contains("pause the queue"),
        "the refusal names what the caller probably wanted: {}",
        message(&refused),
    );
    assert_eq!(
        capacity::configured(&h.context.pool)
            .await
            .expect("read it back")
            .max_concurrency,
        3,
        "a refused write leaves the stored limit alone",
    );
}

#[tokio::test]
async fn the_operator_reads_and_writes_the_strategy_configuration_over_mcp() {
    // ADR-0021's premise: every one of these had a Tauri command and no tool,
    // and an agent could not do what the window could. So each is round-tripped
    // — set, then read back through the *other* tool — rather than merely
    // called, because a setter that stored nothing would pass a smoke test.
    let h = TestContext::new().await;
    let operator = RimaiaServer::new(h.context.clone(), testing::doctor::environment());

    let stored: StrategyApprovalView = json_of(
        operator
            .set_strategy_approval(Parameters(request::<SetStrategyApprovalRequest>(
                json!({ "approval": "manual" }),
            )))
            .await,
    );
    assert_eq!(stored.approval, StrategyApproval::Manual);
    assert_eq!(
        json_of::<StrategyApprovalView>(operator.get_strategy_approval().await).approval,
        StrategyApproval::Manual,
    );

    let catalogue: Catalogue = json_of(
        operator
            .set_strategy_catalogue(Parameters(request::<SetStrategyCatalogueRequest>(json!({
                // ADR-0016's "a new model does not require a release", as the
                // only thing that could prove it: a model this build has never
                // heard of, stored and read back verbatim.
                "catalogue": r#"{"models":[{"id":"sonnet-9","label":"Sonnet 9"}],
                    "efforts":[{"id":"low","label":"Low"}],
                    "planner":{"model":"sonnet-9","effort":"low","max_turns":4}}"#,
            }))))
            .await,
    );
    assert_eq!(
        catalogue.models,
        vec![CatalogueEntry {
            id: "sonnet-9".to_string(),
            label: "Sonnet 9".to_string(),
        }],
    );
    assert_eq!(
        json_of::<Catalogue>(operator.get_strategy_catalogue().await),
        catalogue,
    );
}

#[tokio::test]
async fn strategy_defaults_are_read_and_written_per_repository_or_globally_by_one_pair_of_tools() {
    // The optional `repository_id` is the whole reason there is one tool per
    // direction rather than four (ADR-0021's warning about a surface that is
    // large and badly described), so both spellings are exercised — and the
    // repository's own row must not answer for the global one or the reverse.
    let h = TestContext::new().await;
    let operator = RimaiaServer::new(h.context.clone(), testing::doctor::environment());
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;

    json_of::<StrategyDefaults>(
        operator
            .set_strategy_defaults(Parameters(request::<SetStrategyDefaultsRequest>(json!({
                "mode": "planned",
                "effort": "low",
            }))))
            .await,
    );
    json_of::<StrategyDefaults>(
        operator
            .set_strategy_defaults(Parameters(request::<SetStrategyDefaultsRequest>(json!({
                "repository_id": repository_id,
                "mode": "manual",
                "model": "opus",
                "effort": "high",
            }))))
            .await,
    );

    // Omitted means global — the level beneath the repositories, not a default
    // spelling of "the only repository there is".
    assert_eq!(
        json_of::<StrategyDefaults>(
            operator
                .get_strategy_defaults(Parameters(request::<GetStrategyDefaultsRequest>(json!({}))))
                .await
        ),
        StrategyDefaults {
            mode: rimaia_core::db::StrategyMode::Planned,
            model: None,
            effort: Some("low".to_string()),
        },
    );
    assert_eq!(
        json_of::<StrategyDefaults>(
            operator
                .get_strategy_defaults(Parameters(request::<GetStrategyDefaultsRequest>(
                    json!({ "repository_id": repository_id }),
                )))
                .await
        ),
        StrategyDefaults {
            mode: rimaia_core::db::StrategyMode::Manual,
            model: Some("opus".to_string()),
            effort: Some("high".to_string()),
        },
    );

    // A repository nobody has configured reads as nothing configured, rather
    // than inheriting the global row through this tool — the precedence chain
    // is `strategy::resolve`'s job, and a getter that pre-resolved it would
    // make "clear this repository's override" impossible to express.
    let other = seed_repository(&h.context.pool, "other", "/tmp/other").await;
    assert_eq!(
        json_of::<StrategyDefaults>(
            operator
                .get_strategy_defaults(Parameters(request::<GetStrategyDefaultsRequest>(
                    json!({ "repository_id": other }),
                )))
                .await
        ),
        StrategyDefaults::default(),
    );
}

#[tokio::test]
async fn a_proposal_is_accepted_and_cleared_over_mcp_exactly_as_the_panel_does_it() {
    // The two task-shaped tools ADR-0021 added, round-tripped against the same
    // service the panel calls: accepting takes authorship of a planner's
    // proposal, and clearing is the only thing that lifts D17.8's re-plan guard.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let task = create_task(&h, &repository_id, "Mine").await;
    tasks::update_task(
        &h.context,
        &task.id,
        rimaia_core::tasks::TaskPatch {
            strategy_mode: Some(rimaia_core::db::StrategyMode::Planned),
            ..Default::default()
        },
    )
    .await
    .expect("a planned task");
    tasks::strategy::set_task_strategy(
        &h.context,
        &task.id,
        rimaia_core::tasks::StrategyPlan::proposed(Some("sonnet".to_string()), None),
        rimaia_core::db::StrategySource::Planner,
    )
    .await
    .expect("a planner's proposal");

    let operator = RimaiaServer::new(h.context.clone(), testing::doctor::environment());

    let accepted = ok(operator
        .accept_task_strategy(Parameters(request::<TaskStrategyRequest>(
            json!({ "task_id": task.id }),
        )))
        .await);
    assert_eq!(
        accepted.strategy_source,
        Some(rimaia_core::db::StrategySource::User),
        "accepting is a claim of authorship, and there is no separate `accepted` column",
    );
    assert!(
        accepted.strategy_plan.is_some(),
        "the proposal itself is untouched, so the card keeps rendering the rationale",
    );

    let cleared = ok(operator
        .clear_task_strategy(Parameters(request::<TaskStrategyRequest>(
            json!({ "task_id": task.id }),
        )))
        .await);
    assert_eq!(cleared.strategy_plan, None);
    assert_eq!(cleared.strategy_source, None);
    assert_eq!(
        cleared.model.as_deref(),
        Some("sonnet"),
        "model and effort stay: they are still what this task runs on until the next planner",
    );
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
    RimaiaServer::scoped(
        h.context.with_source(MutationSource::Mcp),
        testing::doctor::environment(),
        task_id,
    )
}

/// A bound server on an OS-chosen port, already spawned, sharing `handles` with
/// the caller the way the shell shares them with the runner.
async fn serving(
    h: &TestContext,
    handles: &RunHandles,
) -> (McpHandle, tokio::task::JoinHandle<()>) {
    let (handle, task) = mcp::build(
        h.context.clone(),
        0,
        handles.clone(),
        testing::doctor::environment(),
    )
    .await;
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

/// The value a tool served, for a call that had no business being refused.
///
/// Generic where [`ok`] is not, because ADR-0021's tools answer with four
/// different shapes and a helper per shape would be four ways of writing
/// `panic!`.
fn json_of<T>(result: Result<Json<T>, rimaia_core::mcp::ToolError>) -> T {
    match result {
        Ok(Json(value)) => value,
        Err(error) => panic!("the operator's own door must serve this: {:?}", error.0),
    }
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
    let listed: TaskListView =
        match RimaiaServer::new(h.context.clone(), testing::doctor::environment())
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
