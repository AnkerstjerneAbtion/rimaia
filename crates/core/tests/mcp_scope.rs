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

use rimaia_core::db::{BoardColumn, MutationSource, RunKind, ScheduleMode};
use rimaia_core::mcp::requests::{
    AcceptContentRequest, ArchiveTaskRequest, AssignTaskRequest, CreateTaskRequest,
    DoctorDismissalRequest, GetReviewHistoryRequest, GetReviewLevelRequest,
    GetStrategyDefaultsRequest, GetTaskConsentRequest, GetTaskRequest, ListReviewFindingsRequest,
    ListTasksRequest, MarkReviewDigestSeenRequest, MoveTaskRequest, PlanSelectionRequest,
    RecordReviewFindingsRequest, ResolveReviewFindingRequest, ReviewNoteRequest,
    ScheduleConfigRequest, ScheduleRequest, SetMaxConcurrencyRequest,
    SetRepositoryMaxConcurrencyRequest, SetRepositoryReviewConfigRequest,
    SetRepositoryUnattendedCeilingRequest, SetReviewSettingsRequest, SetScheduleEnabledRequest,
    SetScheduleModeRequest, SetStrategyApprovalRequest, SetStrategyCatalogueRequest,
    SetStrategyDefaultsRequest, SetTaskDependenciesRequest, SetTaskReviewRequest,
    SetTaskStrategyRequest, TaskStrategyRequest, UpdateScheduleRequest, UpdateTaskRequest,
};
use rimaia_core::mcp::responses::{
    DoctorDismissalsView, DoctorReportView, PreflightView, ScheduleDeletedView, ScheduleListView,
    ScheduleView, StrategyApprovalView, TaskListView, TaskView, TimezoneListView,
};
use rimaia_core::mcp::{
    self, Grant, GrantKind, McpHandle, RimaiaServer, RunAccess, RunGrant, RunHandles, RunScope,
    Tool,
};
use rimaia_core::review;
use rimaia_core::runner::outcome::{start_run, NewRun};
use rimaia_core::schedule::{self, ScheduleInput};
use rimaia_core::scheduler::{capacity, CONCURRENCY_CEILING, DEFAULT_MAX_CONCURRENCY};
use rimaia_core::strategy::{self, Catalogue, CatalogueEntry, StrategyApproval, StrategyDefaults};
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::{self, TestContext};
use rimaia_core::{AppPaths, Error};

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

    // And a decision for every grant, read off D30 point 5's table as this file
    // spells it out. `expected_access` is an exhaustive match on the tool, so a
    // tool added without a row there does not compile, and a row that disagrees
    // with `Tool::run_access` for any grant fails here by name.
    for kind in GrantKind::ALL {
        for tool in Tool::ALL {
            assert_eq!(
                tool.run_access(kind),
                expected_access(tool, kind),
                "{} for a {kind:?} grant",
                tool.as_str()
            );
        }
    }
}

/// D30 point 5's table, one row per tool, written out where a reviewer can
/// read it whole rather than reconstruct it from match arms.
fn expected_access(tool: Tool, kind: GrantKind) -> RunAccess {
    let planner_only = match kind {
        GrantKind::Strategy => RunAccess::OwnTaskOnly,
        GrantKind::Review | GrantKind::Fix => RunAccess::Refused,
    };
    match tool {
        // Every run reads its own card.
        Tool::GetTask => RunAccess::OwnTaskOnly,
        Tool::GetBaseInstructions | Tool::ListRepositories => RunAccess::Unscoped,
        // The planner amends the card it plans (task 020). A reviewer or a
        // fixer that could would be marking its own homework.
        Tool::AddTaskLink | Tool::RemoveTaskLink | Tool::SetTaskStrategy | Tool::UpdateTask => {
            planner_only
        }
        // Each loop run writes only its own output.
        Tool::RecordReviewFindings => match kind {
            GrantKind::Review => RunAccess::OwnTaskOnly,
            GrantKind::Strategy | GrantKind::Fix => RunAccess::Refused,
        },
        Tool::ResolveReviewFinding => match kind {
            GrantKind::Fix => RunAccess::OwnTaskOnly,
            GrantKind::Strategy | GrantKind::Review => RunAccess::Refused,
        },
        // D30's "everything else" row: a fix run is handed its findings in its
        // prompt (task 021) and does not go looking for them.
        Tool::ListReviewFindings => RunAccess::Refused,
        Tool::GetReviewHistory => RunAccess::Refused,
        Tool::GetReviewLevel => RunAccess::Refused,

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
            // Task 066's: where every clone is, its cap and this runner's
            // consent, refused with the caps it reports.
            | Tool::ListCheckouts
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
            // Task 027's two, and the edge is sharper than the rest of that
            // clause: a run that could dismiss a doctor warning could silence
            // the report on the environment it is itself running in, and the
            // operator would read a clean panel about a machine that is not.
            // Restoring is refused with dismissing because the pair is one
            // feature.
            | Tool::DismissDoctorWarning
            | Tool::RestoreDoctorWarning
            // Task 023's two, and this is ADR-0021 point 4's *first* permanent
            // refusal rather than its second: both spawn a `claude` process.
            // `plan_task_strategy` was off the surface entirely until D19 made
            // the in-flight registry reachable from core; the decision was
            // never in doubt, only the wiring.
            | Tool::PlanTaskStrategy
            | Tool::PlanTasksStrategy
            // Task 024's three, and it is the same clause `run_doctor` is
            // refused under: an inventory of every task this machine has
            // attempted, with a price list attached, plus a fact about the
            // operator's own billing.
            | Tool::GetAnalytics
            | Tool::GetSubscriptionCost
            | Tool::SetSubscriptionCost
            // Task 022's one. The two that *write* have no tool at all — the
            // argument is a live forge token, and a loopback protocol into a
            // process's argv is not where one belongs (seam-contract D25).
            | Tool::GetRepositoryCredentialStatus
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
            // Task 030. These have tools at all because archiving is
            // *reversible*, which is the property ADR-0021 point 5's
            // `delete_task` exception is drawn along — but they are still
            // refused for a run. `set_repository_on_archive` is
            // "reconfigures the installation" verbatim, and it decides which
            // program Rimaia will execute. The three archive calls are refused
            // on the narrower ground that archiving is not a board edit: it
            // fires whatever the repository configured, so `OwnTaskOnly` would
            // be a run able to delete the worktree it is standing in.
            Tool::ArchiveTask
            | Tool::ArchiveTasks
            | Tool::UnarchiveTask
            | Tool::SetRepositoryOnArchive => RunAccess::Refused,
            // Task 034 (ADR-0021 point 3). See `review_tools_are_refused_to_a_run`.
            Tool::ApproveTask
            | Tool::RejectTask
            | Tool::RequestTaskChanges
            | Tool::GetTaskDependents
            | Tool::GetReviewDigest
            | Tool::MarkReviewDigestSeen => RunAccess::Refused,
            // Task 021. The loop's configuration reconfigures the installation
            // (ADR-0021 point 4): a run enabling its own loop would spend on its
            // own authority. See `the_review_configuration_is_refused_to_every_grant`.
            Tool::GetReviewSettings
            | Tool::SetReviewSettings
            | Tool::SetRepositoryReviewConfig
            | Tool::SetTaskReview => RunAccess::Refused,
            // Task 045. Each speaks for a person; see
            // `a_run_cannot_accept_through_its_handle`.
            Tool::AssignTask
            | Tool::AcceptContent
            | Tool::SetRepositoryUnattendedCeiling
            | Tool::GetTaskConsent => RunAccess::Refused,
    }
}

#[test]
fn the_operator_endpoint_keeps_every_tool_it_had_before_task_020() {
    // Task 020 adds a narrower door; it takes nothing away from the wide one.
    // Asserted over the whole table rather than over the four a run may not
    // call, so a future `RunAccess` variant cannot quietly start refusing the
    // operator too.
    //
    // The one exception is what only a run writes (D30 point 5), which
    // `the_operator_cannot_write_a_finding` pins; it is exactly two tools, so a
    // third cannot join it by accident.
    let run_outputs: Vec<Tool> = Tool::ALL
        .into_iter()
        .filter(|tool| tool.is_run_output())
        .collect();
    assert_eq!(
        run_outputs,
        vec![Tool::RecordReviewFindings, Tool::ResolveReviewFinding]
    );
    for tool in Tool::ALL.into_iter().filter(|tool| !tool.is_run_output()) {
        RunScope::Operator
            .authorize(tool, None)
            .unwrap_or_else(|error| panic!("{} refused for the operator: {error}", tool.as_str()));
        RunScope::Operator
            .authorize(tool, Some("any-task-at-all"))
            .unwrap_or_else(|error| panic!("{} refused for the operator: {error}", tool.as_str()));
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
                json!({ "catalogue": r#"{"models": [{"id": "opus", "label": "Opus"}]}"# }),
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
        strategy::settings::approval(&h.context)
            .await
            .expect("read the approval setting"),
        StrategyApproval::Automatic,
    );
    assert_eq!(
        strategy::settings::global_default(&h.context)
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
    seed_checkout(&h, &repository_id).await;
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
    let capacity = capacity::configured(h.machine())
        .await
        .expect("read the capacity back");
    assert_eq!(capacity.mode, ScheduleMode::Sequential);
    assert_eq!(capacity.max_concurrency, DEFAULT_MAX_CONCURRENCY);
    assert_eq!(
        h.machine()
            .store
            .get_checkout(&repository_id)
            .await
            .expect("read the checkout back")
            .expect("the repository has a checkout")
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
    let existing = schedule::create(h.machine(), nightly())
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
    let after = schedule::list(h.machine()).await.expect("read them back");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].schedule.id, existing.id);
    assert_eq!(after[0].schedule.name, "Nightly");
    assert!(after[0].schedule.enabled);
    assert_eq!(after[0].schedule.mode, ScheduleMode::Sequential);
}

#[tokio::test]
async fn a_run_cannot_spawn_planners_of_its_own() {
    // Task 023, and ADR-0021 point 4's *first* permanent refusal: both of these
    // spawn a `claude` process. A run that could spawn planners could spend the
    // night's budget on deciding rather than doing, and `plan_tasks_strategy`
    // could do it once per card in a single call.
    //
    // Refused before anything is read, let alone spawned — the assertions below
    // would take minutes rather than milliseconds if `authorize` were not the
    // first statement in each handler.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;
    let run = scoped(&h, &mine.id);

    assert_refusal(
        &as_result(
            run.plan_task_strategy(Parameters(request::<TaskStrategyRequest>(
                json!({ "task_id": mine.id }),
            )))
            .await,
        ),
        &not_available("plan_task_strategy", &mine.id),
    );
    assert_refusal(
        &as_result(
            run.plan_tasks_strategy(Parameters(request::<PlanSelectionRequest>(
                json!({ "column": "ready" }),
            )))
            .await,
        ),
        &not_available("plan_tasks_strategy", &mine.id),
    );
}

#[tokio::test]
async fn a_run_cannot_silence_the_doctor_about_the_machine_it_is_running_on() {
    // Task 027, and the sharpest edge on ADR-0021 point 4's second refusal: a
    // run that could dismiss a warning could dismiss the one describing its own
    // environment, and the operator would read a clean panel about a machine
    // that is not. Called through the handlers, not only checked against the
    // table, because a tool that forgot its `authorize` line satisfies the
    // table and is still callable.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;
    let run = scoped(&h, &mine.id);

    let warning = json!({
        "check": "mcp_port",
        "repository": null,
        "detail": "nothing is listening on 4517.",
    });

    assert_refusal(
        &as_result(
            run.dismiss_doctor_warning(Parameters(request::<DoctorDismissalRequest>(
                warning.clone(),
            )))
            .await,
        ),
        &not_available("dismiss_doctor_warning", &mine.id),
    );
    assert_refusal(
        &as_result(
            run.restore_doctor_warning(Parameters(request::<DoctorDismissalRequest>(warning)))
                .await,
        ),
        &not_available("restore_doctor_warning", &mine.id),
    );

    // And neither wrote on the way to being refused.
    assert_eq!(
        rimaia_core::db::settings::doctor_dismissals(h.machine())
            .await
            .expect("read the key"),
        Vec::new()
    );
}

#[tokio::test]
async fn the_operator_dismisses_and_restores_a_doctor_warning_over_mcp() {
    // Both writes round-tripped through the reader, and through the same
    // `run_doctor` view the window reads — a setter that stored nothing would
    // pass a smoke test.
    let h = TestContext::new().await;
    let operator = RimaiaServer::new(
        h.context.clone(),
        testing::doctor::provider(),
        Some(testing::doctor::local_tools(h.machine())),
    );

    let warning = json!({
        "check": "github_cli",
        "repository": "rimaia",
        "detail": "gh is not authenticated for github.com, used by rimaia.",
    });

    let after_dismiss = json_of::<DoctorDismissalsView>(
        operator
            .dismiss_doctor_warning(Parameters(request::<DoctorDismissalRequest>(
                warning.clone(),
            )))
            .await,
    );
    assert_eq!(after_dismiss.dismissals.len(), 1);
    assert_eq!(after_dismiss.dismissals[0].check, "github_cli");
    assert_eq!(
        after_dismiss.dismissals[0].repository.as_deref(),
        Some("rimaia")
    );

    // The report carries the same set, so an agent that only ran the doctor can
    // still see what has been put down and hand it back.
    let report = json_of::<DoctorReportView>(operator.run_doctor().await);
    assert_eq!(report.dismissals, after_dismiss.dismissals);

    let after_restore = json_of::<DoctorDismissalsView>(
        operator
            .restore_doctor_warning(Parameters(request::<DoctorDismissalRequest>(warning)))
            .await,
    );
    assert!(after_restore.dismissals.is_empty());
}

#[tokio::test]
async fn the_operator_reads_and_writes_schedules_over_mcp() {
    // Each write round-tripped through the reader rather than merely called: a
    // setter that stored nothing would pass a smoke test.
    let h = TestContext::new().await;
    let operator = RimaiaServer::new(
        h.context.clone(),
        testing::doctor::provider(),
        Some(testing::doctor::local_tools(h.machine())),
    );

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
    let operator = RimaiaServer::new(
        h.context.clone(),
        testing::doctor::provider(),
        Some(testing::doctor::local_tools(h.machine())),
    );

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
    seed_checkout(&h, &repository_id).await;
    let operator = RimaiaServer::new(
        h.context.clone(),
        testing::doctor::provider(),
        Some(testing::doctor::local_tools(h.machine())),
    );

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
    // This machine's checkout, in D16.1's snake case (task 066), rather than
    // the board's `RepositoryView`, which no longer carries the cap.
    assert_eq!(
        serde_json::to_value(&repository).expect("serialize"),
        json!({
            "repository_id": repository_id,
            "path": "/tmp/rimaia",
            "worktree_root": "/tmp/rimaia-worktrees",
            "max_concurrency": 2,
            "unattended_consent": false,
            "on_archive": "none",
            "on_archive_script": null,
        })
    );

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
        capacity::configured(h.machine())
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
    let operator = RimaiaServer::new(
        h.context.clone(),
        testing::doctor::provider(),
        Some(testing::doctor::local_tools(h.machine())),
    );

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
    let operator = RimaiaServer::new(
        h.context.clone(),
        testing::doctor::provider(),
        Some(testing::doctor::local_tools(h.machine())),
    );
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

    let operator = RimaiaServer::new(
        h.context.clone(),
        testing::doctor::provider(),
        Some(testing::doctor::local_tools(h.machine())),
    );

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
        let grant = handles.grant("task-1", &h.solo.team_id, Grant::Strategy);
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

    let grant = handles.grant(&mine.id, &h.solo.team_id, Grant::Strategy);
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
// What each grant may do (seam-contract D30 point 5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_run_scoped_handle_reaches_only_its_own_task() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;
    let theirs = create_task(&h, &repository_id, "Someone else's").await;
    let review_run = open_run(&h, &mine.id, RunKind::Review).await;
    let fix_run = open_run(&h, &mine.id, RunKind::Fix).await;

    let grants = [
        Grant::Strategy,
        Grant::Review {
            run_id: review_run.clone(),
        },
        Grant::Fix {
            run_id: fix_run.clone(),
        },
    ];
    for grant in grants {
        let kind = grant.kind();
        let server = scoped_as(&h, &mine.id, grant.clone());

        // Another task's card is out of reach for every grant.
        assert_refusal(
            &as_result(
                server
                    .get_task(Parameters(request::<GetTaskRequest>(
                        json!({ "task_id": theirs.id }),
                    )))
                    .await,
            ),
            &format!(
                "this handle is scoped to task {mine}, so get_task cannot be called against task \
                 {theirs}.",
                mine = mine.id,
                theirs = theirs.id,
            ),
        );

        // And so is writing findings against another task, whatever the grant.
        let against_theirs = as_result(
            server
                .record_review_findings(Parameters(request::<RecordReviewFindingsRequest>(
                    json!({ "task_id": theirs.id, "findings": [] }),
                )))
                .await,
        );
        assert_eq!(against_theirs.is_error, Some(true), "{kind:?}");

        // Every tool D30's table marks refused for this grant, through the
        // decision point every handler starts with.
        let scope = RunScope::Run {
            task_id: mine.id.clone(),
            grant: grant.clone(),
        };
        for tool in Tool::ALL {
            if tool.run_access(kind) == RunAccess::Refused {
                let refused = scope
                    .authorize(tool, Some(&mine.id))
                    .expect_err("a refused tool is refused on the run's own task too");
                assert_eq!(
                    refused.to_string(),
                    not_available(tool.as_str(), &mine.id),
                    "{} for a {kind:?} grant",
                    tool.as_str()
                );
            }
        }

        // And through the real handlers, for task 034's six and the findings
        // read, which every grant is refused.
        let refusals = [
            (
                "approve_task",
                as_result(
                    server
                        .approve_task(Parameters(request::<ArchiveTaskRequest>(
                            json!({ "task_id": mine.id }),
                        )))
                        .await,
                ),
            ),
            (
                "reject_task",
                as_result(
                    server
                        .reject_task(Parameters(request::<ReviewNoteRequest>(
                            json!({ "task_id": mine.id, "note": "No." }),
                        )))
                        .await,
                ),
            ),
            (
                "request_task_changes",
                as_result(
                    server
                        .request_task_changes(Parameters(request::<ReviewNoteRequest>(
                            json!({ "task_id": mine.id, "note": "Again." }),
                        )))
                        .await,
                ),
            ),
            (
                "get_task_dependents",
                as_result(
                    server
                        .get_task_dependents(Parameters(request::<ArchiveTaskRequest>(
                            json!({ "task_id": mine.id }),
                        )))
                        .await,
                ),
            ),
            (
                "get_review_digest",
                as_result(server.get_review_digest().await),
            ),
            (
                "mark_review_digest_seen",
                as_result(
                    server
                        .mark_review_digest_seen(Parameters(
                            request::<MarkReviewDigestSeenRequest>(
                                json!({ "through": "2026-08-20T00:00:00Z" }),
                            ),
                        ))
                        .await,
                ),
            ),
            (
                "list_review_findings",
                as_result(
                    server
                        .list_review_findings(Parameters(request::<ListReviewFindingsRequest>(
                            json!({ "task_id": mine.id }),
                        )))
                        .await,
                ),
            ),
        ];
        for (tool, result) in &refusals {
            assert_refusal(result, &not_available(tool, &mine.id));
        }

        match kind {
            GrantKind::Review => {
                // A review writes, on its own task, under its grant's run id —
                // the only run id a request can never name.
                let recorded = json_of(
                    server
                        .record_review_findings(Parameters(request::<RecordReviewFindingsRequest>(
                            json!({
                                "task_id": mine.id,
                                "findings": [{
                                    "severity": "high",
                                    "title": "Unchecked index",
                                    "body": "Panics on an empty list.",
                                    "file": "src/lib.rs",
                                    "line": 12,
                                }],
                            }),
                        )))
                        .await,
                );
                assert_eq!(recorded.findings.len(), 1);
                assert_eq!(recorded.findings[0].review_run_id, review_run);
            }
            GrantKind::Fix => {
                // A fix resolves its own task's open finding under its run id,
                // and may not record one.
                assert_refusal(
                    &as_result(
                        server
                            .record_review_findings(Parameters(request::<
                                RecordReviewFindingsRequest,
                            >(
                                json!({ "task_id": mine.id, "findings": [] }),
                            )))
                            .await,
                    ),
                    &not_available("record_review_findings", &mine.id),
                );
                let open = review::findings::list(&h.context, &mine.id, None)
                    .await
                    .expect("the review's findings");
                let resolved = json_of(
                    server
                        .resolve_review_finding(Parameters(request::<ResolveReviewFindingRequest>(
                            json!({
                                "task_id": mine.id,
                                "finding_id": open[0].id,
                                "status": "fixed",
                                "resolution": "Checked the length first.",
                            }),
                        )))
                        .await,
                );
                assert_eq!(
                    resolved.resolved_by_run_id.as_deref(),
                    Some(fix_run.as_str())
                );
            }
            GrantKind::Strategy => {
                assert_refusal(
                    &as_result(
                        server
                            .record_review_findings(Parameters(request::<
                                RecordReviewFindingsRequest,
                            >(
                                json!({ "task_id": mine.id, "findings": [] }),
                            )))
                            .await,
                    ),
                    &not_available("record_review_findings", &mine.id),
                );
            }
        }
    }
}

#[tokio::test]
async fn the_operator_cannot_write_a_finding() {
    // The first thing the operator's door has ever been refused: a finding the
    // operator wrote would look exactly like a reviewer's in the morning (D30
    // point 5). The refusal is the same `{ code, message }` as every other.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let mine = create_task(&h, &repository_id, "Mine").await;
    let operator = RimaiaServer::new(
        h.context.with_source(MutationSource::Mcp),
        testing::doctor::provider(),
        Some(testing::doctor::local_tools(h.machine())),
    );

    let recorded = as_result(
        operator
            .record_review_findings(Parameters(request::<RecordReviewFindingsRequest>(
                json!({ "task_id": mine.id, "findings": [] }),
            )))
            .await,
    );
    let resolved = as_result(
        operator
            .resolve_review_finding(Parameters(request::<ResolveReviewFindingRequest>(json!({
                "task_id": mine.id,
                "finding_id": "any-finding",
                "status": "fixed",
            }))))
            .await,
    );

    for (tool, result) in [
        ("record_review_findings", recorded),
        ("resolve_review_finding", resolved),
    ] {
        let same_shape = Error::invalid(format!(
            "{tool} is not available here: only the run a finding belongs to writes it, through \
             its own run-scoped handle."
        ));
        assert_eq!(result.is_error, Some(true), "{tool}");
        assert_eq!(
            result.structured_content,
            Some(serde_json::to_value(&same_shape).expect("the tauri boundary's payload")),
            "{tool}"
        );
        assert_eq!(message(&result), same_shape.to_string());
    }
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM review_findings")
        .fetch_one(&h.context.pool)
        .await
        .expect("count findings");
    assert_eq!(stored, 0);
}

#[tokio::test]
async fn the_run_scoped_server_reports_its_own_name() {
    // D30 point 1: the handshake names the door. A run that inherits the
    // operator's registration sees `rimaia` there and `rimaia-run` on its own
    // handle, which is what keeps the two apart in its tool names.
    let h = TestContext::new().await;
    let handles = RunHandles::default();
    let (handle, server) = serving(&h, &handles).await;
    let address = handle.status().bound_address.expect("a bound address");

    let grant = handles.grant("task-1", &h.solo.team_id, Grant::Strategy);
    for (url, expected) in [
        (scoped_url(&handles, &grant), mcp::RUN_MCP_SERVER_NAME),
        (format!("http://{address}/mcp"), mcp::MCP_SERVER_NAME),
    ] {
        let client = ()
            .serve(StreamableHttpClientTransport::with_client(
                reqwest::Client::default(),
                StreamableHttpClientTransportConfig::with_uri(url.clone()),
            ))
            .await
            .expect("the server answers `initialize`");
        let info = client.peer_info().expect("the handshake's server info");
        let name = info.server_info.as_ref().map(|server| server.name.as_str());
        assert_eq!(name, Some(expected), "{url}");
        let _ = client.cancel().await;
    }
    assert_eq!(mcp::RUN_MCP_SERVER_NAME, "rimaia-run");
    assert_eq!(mcp::MCP_SERVER_NAME, "rimaia");

    handle.shutdown();
    server.await.expect("the server task ends");
}

// ---------------------------------------------------------------------------
// Board tools in core, machine tools injected by the host (task 041)
// ---------------------------------------------------------------------------

/// The 22 tools that inspect, reconfigure or spawn on this machine: the local
/// router, as task 041's Scope names it.
const LOCAL_TOOLS: [&str; 23] = [
    "run_doctor",
    "dismiss_onboarding",
    "dismiss_doctor_warning",
    "restore_doctor_warning",
    "get_repository_credential_status",
    "list_worktrees",
    "get_worktree_auto_cleanup",
    "set_worktree_auto_cleanup",
    "set_repository_on_archive",
    "get_run_capacity",
    "set_schedule_mode",
    "set_max_concurrency",
    "set_repository_max_concurrency",
    "list_checkouts",
    "list_schedules",
    "create_schedule",
    "update_schedule",
    "set_schedule_enabled",
    "delete_schedule",
    "preview_schedule_preflight",
    "list_timezones",
    "plan_task_strategy",
    "plan_tasks_strategy",
];

/// Every tool name a client listing `url` is offered, sorted.
async fn listed_tools(url: &str) -> Vec<String> {
    let client = ()
        .serve(StreamableHttpClientTransport::with_client(
            reqwest::Client::default(),
            StreamableHttpClientTransportConfig::with_uri(url.to_string()),
        ))
        .await
        .expect("the server answers `initialize`");
    let mut names: Vec<String> = client
        .list_all_tools()
        .await
        .expect("the server lists its tools")
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    let _ = client.cancel().await;
    names.sort();
    names
}

/// Every tool `Tool::ALL` declares, sorted, less any in `excluding`.
fn declared_tools(excluding: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = Tool::ALL
        .into_iter()
        .map(|tool| tool.as_str().to_string())
        .filter(|name| !excluding.contains(&name.as_str()))
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_server_without_a_machine_lists_no_machine_tool() {
    // Task 046's server and task 060's hosted `/mcp` pass `None`: a host with
    // no machine serves the board router alone, and a local tool is not a
    // refusal there but a tool that does not exist.
    let h = TestContext::new().await;
    let (handle, server) = mcp::build(
        h.context.clone(),
        0,
        RunHandles::default(),
        testing::doctor::provider(),
        None,
    )
    .await;
    let server = tokio::spawn(server.run());
    let url = handle.url().expect("the server is listening");

    let listed = listed_tools(&url).await;

    assert_eq!(
        listed,
        declared_tools(&LOCAL_TOOLS),
        "exactly the board router"
    );
    for local in LOCAL_TOOLS {
        assert!(
            !listed.iter().any(|name| name == local),
            "{local} is listed"
        );
    }

    let client = ()
        .serve(StreamableHttpClientTransport::with_client(
            reqwest::Client::default(),
            StreamableHttpClientTransportConfig::with_uri(url.clone()),
        ))
        .await
        .expect("the server answers `initialize`");
    for local in ["run_doctor", "list_schedules", "plan_task_strategy"] {
        let error = client
            .call_tool(CallToolRequestParams::new(local))
            .await
            .expect_err("a tool this server does not have is not a tool result");
        assert!(
            error.to_string().contains("tool not found"),
            "{local}: an unknown-tool error, not a refusal: {error}"
        );
    }
    let _ = client.cancel().await;

    handle.shutdown();
    server.await.expect("the server task ends");
}

#[tokio::test]
async fn with_a_machine_both_doors_list_every_tool_and_a_run_is_refused_a_local_one() {
    // The shell passes `Some` to both constructors. `tools/list` is not
    // filtered by scope, so a run is still offered all 22, and calling one is
    // `RunScope`'s refusal in today's sentence, never an unknown tool.
    let h = TestContext::new().await;
    let handles = RunHandles::default();
    let (handle, server) = serving(&h, &handles).await;
    let operator = handle.url().expect("the server is listening");
    let grant = handles.grant("task-1", &h.solo.team_id, Grant::Strategy);
    let run = scoped_url(&handles, &grant);

    assert_eq!(listed_tools(&operator).await, declared_tools(&[]));
    assert_eq!(listed_tools(&run).await, declared_tools(&[]));

    let client = ()
        .serve(StreamableHttpClientTransport::with_client(
            reqwest::Client::default(),
            StreamableHttpClientTransportConfig::with_uri(run.clone()),
        ))
        .await
        .expect("a run's own handle answers `initialize`");
    let refused = client
        .call_tool(CallToolRequestParams::new("run_doctor"))
        .await
        .expect("a refusal is a tool result");
    assert_refusal(&refused, &not_available("run_doctor", "task-1"));
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
///
/// The planner's grant, which is the table every test before task 035 was
/// written against.
fn scoped(h: &TestContext, task_id: &str) -> RimaiaServer {
    scoped_as(h, task_id, Grant::Strategy)
}

/// The same, holding `grant`.
fn scoped_as(h: &TestContext, task_id: &str, grant: Grant) -> RimaiaServer {
    RimaiaServer::scoped(
        h.context.with_source(MutationSource::Mcp),
        testing::doctor::provider(),
        Some(testing::doctor::local_tools(h.machine())),
        RunScope::Run {
            task_id: task_id.to_string(),
            grant,
        },
    )
}

/// A running row of `kind` on `task_id`, opened through `start_run`, the one
/// writer. Nothing reads the transcript path, so it points nowhere.
async fn open_run(h: &TestContext, task_id: &str, kind: RunKind) -> String {
    start_run(
        &h.context,
        &AppPaths::new(std::path::Path::new("/tmp/rimaia-scope-test")),
        NewRun {
            task_id: task_id.to_string(),
            kind,
            session_id: rimaia_core::db::new_id(),
            prompt: "a prompt".to_string(),
            base_ref: None,
            base_sha: None,
        },
    )
    .await
    .expect("open a run row")
    .id
}

/// One grant of each kind, with run ids that name no row.
fn every_grant() -> [Grant; 3] {
    [
        Grant::Strategy,
        Grant::Review {
            run_id: "a-review-run".to_string(),
        },
        Grant::Fix {
            run_id: "a-fix-run".to_string(),
        },
    ]
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
        testing::doctor::provider(),
        Some(testing::doctor::local_tools(h.machine())),
    )
    .await;
    (handle, tokio::spawn(task.run()))
}

/// The URL a run is actually handed.
///
/// Deliberately not one a test formats: what the runner hands the child has to
/// be what the router serves, and that is exactly the seam a hand-written URL
/// would hide. How that URL is then *spelled* for one agent CLI is the
/// provider's (ADR-0026), and `runner_process.rs` asserts that half.
fn scoped_url(handles: &RunHandles, grant: &RunGrant) -> String {
    handles.endpoint_for(grant).expect("an endpoint is bound")
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
    let team_id = solo_team(pool).await;
    sqlx::query(
        "INSERT INTO repositories (id, team_id, name, path, default_branch, worktree_root, allow_unattended_runs, created_at)
         VALUES (?1, ?5, ?2, ?3, 'main', '/tmp/rimaia-worktrees', 0, ?4)",
    )
    .bind(&id)
    .bind(name)
    .bind(path)
    .bind(NOW)
    .bind(&team_id)
    .execute(pool)
    .await
    .expect("seed a repository");
    id
}

/// This machine's checkout of a seeded repository, for a tool that reads or
/// writes one (task 066).
async fn seed_checkout(h: &TestContext, repository_id: &str) {
    h.machine()
        .store
        .insert_checkout(&testing::machine::checkout_at(
            repository_id,
            std::path::Path::new("/tmp/rimaia"),
        ))
        .await
        .expect("seed a checkout");
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
    let listed: TaskListView = match RimaiaServer::new(
        h.context.clone(),
        testing::doctor::provider(),
        Some(testing::doctor::local_tools(h.machine())),
    )
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

#[test]
fn review_tools_are_refused_to_a_run() {
    // Deciding a review is a run marking its own homework (D30 point 5); the
    // digest and dependents reads enumerate other tasks (D16.6); and the marker
    // write reconfigures the installation (ADR-0021 point 4).
    for grant in every_grant() {
        let kind = grant.kind();
        let run = RunScope::Run {
            task_id: "its-own-task".to_string(),
            grant,
        };
        for tool in [
            Tool::ApproveTask,
            Tool::RejectTask,
            Tool::RequestTaskChanges,
            Tool::GetTaskDependents,
            Tool::GetReviewDigest,
            Tool::MarkReviewDigestSeen,
        ] {
            assert_eq!(
                tool.run_access(kind),
                RunAccess::Refused,
                "{}",
                tool.as_str()
            );
            assert!(
                run.authorize(tool, Some("its-own-task")).is_err(),
                "{} reached a run",
                tool.as_str()
            );
            assert!(run.authorize(tool, None).is_err(), "{}", tool.as_str());
        }
    }
}

#[tokio::test]
async fn the_review_configuration_is_refused_to_every_grant() {
    // A run that could turn its own loop on would be spending on its own
    // authority, and a fixer that could rewrite its own review instructions
    // would be marking its own homework (ADR-0021 point 4, task 021). Refused on
    // its own task, before anything is read or written.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let task = create_task(&h, &repository_id, "Mine").await;
    let on = json!({ "enabled": "on_cost_acknowledged" });

    for grant in every_grant() {
        let server = scoped_as(&h, &task.id, grant);

        assert_refusal(
            &as_result(server.get_review_settings().await),
            &not_available("get_review_settings", &task.id),
        );
        assert_refusal(
            &as_result(
                server
                    .get_review_level(Parameters(request::<GetReviewLevelRequest>(
                        json!({ "level": "task", "id": task.id }),
                    )))
                    .await,
            ),
            &not_available("get_review_level", &task.id),
        );
        assert_refusal(
            &as_result(
                server
                    .set_review_settings(Parameters(request::<SetReviewSettingsRequest>(
                        json!({ "instructions": "", "config": on }),
                    )))
                    .await,
            ),
            &not_available("set_review_settings", &task.id),
        );
        assert_refusal(
            &as_result(
                server
                    .set_repository_review_config(Parameters(request::<
                        SetRepositoryReviewConfigRequest,
                    >(
                        json!({ "repository_id": repository_id, "config": on }),
                    )))
                    .await,
            ),
            &not_available("set_repository_review_config", &task.id),
        );
        assert_refusal(
            &as_result(
                server
                    .set_task_review(Parameters(request::<SetTaskReviewRequest>(
                        json!({ "task_id": task.id, "config": on }),
                    )))
                    .await,
            ),
            &not_available("set_task_review", &task.id),
        );
    }

    let after = tasks::get_task(&h.context, &task.id)
        .await
        .expect("read the task");
    assert_eq!(
        after.review_config,
        Default::default(),
        "nothing was written"
    );
}

#[tokio::test]
async fn getting_review_history_is_refused_to_every_run_grant() {
    // D30's "everything else" row (task 037): the history holds every finding
    // of every loop, and a run is handed what it needs in its prompt. Refused
    // on its own task, before anything is read.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let task = create_task(&h, &repository_id, "Mine").await;

    for grant in every_grant() {
        let server = scoped_as(&h, &task.id, grant);

        assert_refusal(
            &as_result(
                server
                    .get_review_history(Parameters(request::<GetReviewHistoryRequest>(
                        json!({ "task_id": task.id }),
                    )))
                    .await,
            ),
            &not_available("get_review_history", &task.id),
        );
    }
}

#[tokio::test]
async fn a_review_grant_cannot_call_resolve_review_finding() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let task = create_task(&h, &repository_id, "Reviewed").await;
    let review_run = open_run(&h, &task.id, RunKind::Review).await;
    let server = scoped_as(&h, &task.id, Grant::Review { run_id: review_run });

    let refused = as_result(
        server
            .resolve_review_finding(Parameters(request::<ResolveReviewFindingRequest>(json!({
                "task_id": task.id,
                "finding_id": "any",
                "status": "fixed",
            }))))
            .await,
    );

    assert_refusal(&refused, &not_available("resolve_review_finding", &task.id));
}

#[tokio::test]
async fn a_fix_grant_cannot_call_record_review_findings() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let task = create_task(&h, &repository_id, "Fixed").await;
    let fix_run = open_run(&h, &task.id, RunKind::Fix).await;
    let server = scoped_as(&h, &task.id, Grant::Fix { run_id: fix_run });

    let refused = as_result(
        server
            .record_review_findings(Parameters(request::<RecordReviewFindingsRequest>(
                json!({ "task_id": task.id, "findings": [] }),
            )))
            .await,
    );

    assert_refusal(&refused, &not_available("record_review_findings", &task.id));
}

/// The solo team the board's rows belong to: the identity `TestContext`
/// already created, or a first launch's, read through the same
/// `identity::ensure_solo` either way.
async fn solo_team(pool: &SqlitePool) -> String {
    rimaia_core::identity::ensure_solo(
        pool,
        &rimaia_core::testing::TestClock::new(rimaia_core::testing::test_epoch()),
    )
    .await
    .expect("the board's solo identity")
    .team_id
}

#[tokio::test]
async fn a_run_cannot_accept_through_its_handle() {
    // ADR-0032 point 6, and task 045's reason for refusing all four: a run that
    // could accept would launder consent through its own handle, its owner
    // accepting content the run itself wrote. Refused for every grant, on its
    // own task, before anything is read or written; the operator's door is the
    // control.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let task = create_task(&h, &repository_id, "Mine").await;
    let accept = || {
        request::<AcceptContentRequest>(json!({
            "team_id": h.solo.team_id,
            "task_id": task.id,
            "kind": "plan",
            "revision": "1",
        }))
    };

    for grant in every_grant() {
        let server = scoped_as(&h, &task.id, grant);

        assert_refusal(
            &as_result(server.accept_content(Parameters(accept())).await),
            &not_available("accept_content", &task.id),
        );
        assert_refusal(
            &as_result(
                server
                    .assign_task(Parameters(request::<AssignTaskRequest>(
                        json!({ "task_id": task.id, "assignee_id": null }),
                    )))
                    .await,
            ),
            &not_available("assign_task", &task.id),
        );
        assert_refusal(
            &as_result(
                server
                    .set_repository_unattended_ceiling(Parameters(request::<
                        SetRepositoryUnattendedCeilingRequest,
                    >(
                        json!({ "repository_id": repository_id, "allowed": true }),
                    )))
                    .await,
            ),
            &not_available("set_repository_unattended_ceiling", &task.id),
        );
        assert_refusal(
            &as_result(
                server
                    .get_task_consent(Parameters(request::<GetTaskConsentRequest>(
                        json!({ "task_id": task.id, "runner_id": h.solo.runner_id }),
                    )))
                    .await,
            ),
            &not_available("get_task_consent", &task.id),
        );
    }
    let recorded: i64 = sqlx::query_scalar("SELECT count(*) FROM acceptances")
        .fetch_one(&h.context.pool)
        .await
        .expect("count the acceptances");
    assert_eq!(recorded, 0, "no run recorded an acceptance");

    let operator = RimaiaServer::new(
        h.context.with_source(MutationSource::Mcp),
        testing::doctor::provider(),
        Some(testing::doctor::local_tools(h.machine())),
    );
    json_of(operator.accept_content(Parameters(accept())).await);
    let recorded: i64 = sqlx::query_scalar("SELECT count(*) FROM acceptances")
        .fetch_one(&h.context.pool)
        .await
        .expect("count the acceptances");
    assert_eq!(recorded, 1, "the operator's own door accepts");
}
