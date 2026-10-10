//! The registered tools (ADR-0006, its 2026-08-28 amendment, and ADR-0021),
//! and nothing else.
//!
//! Every handler here marshals a request, calls a `rimaia-core` service, and
//! projects the result. **No business rule lives in this file.** A rule
//! enforced in only one of the two doors is a bug — which is why
//! `tests/mcp_tools.rs` asserts not that both paths fail, but that both fail
//! with the same payload.
//!
//! The one thing that *is* decided here is adapter ergonomics, and it is
//! exactly one thing: `move_task` synthesises the bottom-of-column neighbour
//! when the caller names none, because `tasks::move_task` requires a neighbour
//! or an empty column and that rule is not relaxed for MCP (seam-contract
//! D16). Ergonomics is not an invariant.
//!
//! Tool descriptions say *when* to call, not only what a tool does — ADR-0006
//! requires it, and it measurably improves tool selection. They are written for
//! the agent that will read them cold, in a session that knows nothing about
//! Rimaia.
//!
//! Since task 020 there is one more thing every handler does, and it is the
//! first thing: [`RunScope::authorize`]. A server reached through
//! `/mcp/run/{token}` is one run working on one task, and the allow table lives
//! in [`crate::mcp::scope`] rather than in eleven `if` statements here.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};

use crate::analytics::{self, Period};
use crate::context::{ServiceContext, TeamScope};
use crate::db::{BoardColumn, StrategySource};
use crate::doctor;
use crate::machine::{self, MachineContext};
use crate::mcp::error::ToolError;
use crate::mcp::requests::{
    AddTaskLinkRequest, AnalyticsRequest, ArchiveTaskRequest, ArchiveTasksRequest, ClearableField,
    CreateTaskRequest, DoctorDismissalRequest, GetReviewHistoryRequest, GetReviewLevelRequest,
    GetStrategyDefaultsRequest, GetTaskRequest, ListReviewFindingsRequest, ListTasksRequest,
    MarkReviewDigestSeenRequest, MoveTaskRequest, PlanSelectionRequest,
    RecordReviewFindingsRequest, RemoveTaskLinkRequest, RepositoryRequest,
    ResolveReviewFindingRequest, ReviewNoteRequest, ScheduleConfigRequest, ScheduleRequest,
    SetMaxConcurrencyRequest, SetRepositoryMaxConcurrencyRequest, SetRepositoryOnArchiveRequest,
    SetRepositoryReviewConfigRequest, SetReviewSettingsRequest, SetScheduleEnabledRequest,
    SetScheduleModeRequest, SetStrategyApprovalRequest, SetStrategyCatalogueRequest,
    SetStrategyDefaultsRequest, SetTaskDependenciesRequest, SetTaskReviewRequest,
    SetTaskStrategyRequest, SetWorktreeAutoCleanupRequest, SubscriptionCostRequest,
    TaskStrategyRequest, UpdateScheduleRequest, UpdateTaskRequest,
};
use crate::mcp::responses::{
    AnalyticsView, ArchiveReportView, ArchivedTaskView, BaseInstructionsView, CheckoutListView,
    CheckoutView, CredentialStatusView, DigestMarkerView, DismissalView, DoctorDismissalsView,
    DoctorReportView, OnboardingView, PlanPassView, PlanResultView, PreflightView,
    RepositoryListView, RepositoryOnArchiveView, RepositoryView, ReviewDigestView,
    ReviewFindingView, ReviewFindingsView, ReviewHistoryView, ReviewOutcomeView, ReviewedTaskView,
    RunCapacityView, ScheduleDeletedView, ScheduleListView, ScheduleView, StrategyApprovalView,
    SubscriptionCostView, TaskDependentsView, TaskListItem, TaskListView, TaskView,
    TimezoneListView, WorktreeAutoCleanupView, WorktreeListView, WorktreeView,
};
use crate::mcp::scope::{RunScope, Tool};
use crate::review;
use crate::review_loop::{self, ReviewConfig, ReviewLevel, ReviewSettings, TaskReview};
use crate::runner::prompt::TEMPLATE_VARIABLES;
use crate::runner::provider::AgentProvider;
use crate::runner::strategy::{self as runner_strategy, PlanOutcome, PlanSelection, PlannerAccess};
use crate::schedule;
use crate::scheduler::{self, capacity};
use crate::strategy::{self, Catalogue, StrategyDefaults};
use crate::tasks::{NewTask, NewTaskLink, Patch, TaskFilter, TaskPatch};
use crate::{archive, db, repo, tasks, worktree, Result};

/// What Claude Code is told this server is for, before it has read a single
/// tool description.
const SERVER_INSTRUCTIONS: &str = "\
Rimaia is a desktop app on this machine that queues implementation plans and runs them later, \
unattended, with Claude Code — each in its own git worktree, producing a branch and a pull \
request for the user to review in the morning. Use these tools to hand a finished plan over to \
Rimaia instead of implementing it in this session. You are writing for a future agent that will \
have the plan and nothing else: no memory of this conversation, and nobody to ask. Anything the \
implementation depends on must be in the plan.";

/// The tool handler. Three fields: the board services it calls through,
/// which door it was reached through, and this machine, when the host has
/// one. It still has no state of its own — everything it can do, it does
/// through the same services the Tauri commands call.
///
/// Cheap to clone, like [`ServiceContext`] itself: the streamable-HTTP
/// transport builds one per request.
#[derive(Clone)]
pub struct RimaiaServer {
    ctx: ServiceContext,
    /// Carried on the *value*, not on the request, which is the whole argument
    /// for putting the token in the path — see [`RunScope`].
    scope: RunScope,
    /// The agent CLI whose catalogue the board tools read and validate
    /// against, read for nothing else: the board side's counterpart of
    /// `InProcessBoard`'s provider (D31 point 9), which task 046 replaces with
    /// D32's `ProviderProfile`. A field of its own rather than read off
    /// [`LocalTools`], because the board router serves without a machine.
    provider: Arc<dyn AgentProvider>,
    /// This machine, for the local router (task 041). `None` on a host with
    /// no machine, which then serves the board router alone.
    local: Option<LocalTools>,
}

/// What the local router reaches this machine through (task 041, ADR-0035
/// point 6).
///
/// Core defines the handlers and the [`MachineStore`](crate::machine::MachineStore)
/// trait; the host decides whether a machine exists and supplies it. The shell
/// passes `Some` to both the operator's and the run-scoped constructor; task
/// 046's server and task 060's hosted `/mcp` pass `None`.
#[derive(Clone)]
pub struct LocalTools {
    /// This machine's own state: the settings, the schedules and, from task
    /// 066, the checkouts and worktree records.
    pub machine: MachineContext,
    /// What `run_doctor` reports about (task 018).
    ///
    /// An explicit field rather than a default, because the two things it
    /// carries cannot be guessed from here and a wrong guess would be a doctor
    /// that reassures about the wrong installation: the app data directory is a
    /// platform lookup only the shell can do (see [`AppPaths`](crate::AppPaths)),
    /// and `programs.agent` must be the very binary the runner would spawn.
    pub doctor: doctor::Environment,
    /// What `plan_task_strategy` and `plan_tasks_strategy` spawn through (task
    /// 023), for exactly the reason `doctor` is a field: the data directory,
    /// the `claude` the runner would spawn and the shared in-flight registry
    /// are all things only the shell knows. Also where
    /// `get_repository_credential_status` reaches this machine's keychain.
    pub planner: PlannerAccess,
}

impl std::fmt::Debug for LocalTools {
    /// By hand because neither a machine store nor a board port is `Debug`.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalTools")
            .field("machine", &self.machine)
            .field("doctor", &self.doctor)
            .field("planner", &self.planner)
            .finish()
    }
}

/// The board router: every tool that needs only the board, including the
/// ones tasks 033–035 and 021 added. Five of them, `archive_task`,
/// `archive_tasks`, `move_task`, `approve_task` and `reject_task`, also hand
/// this machine to their core function when there is one, for the reaction
/// it runs on the machine (task 041).
///
/// `vis = "pub"` here and on [`local_router`](Self::local_router) so
/// `tests/mcp_scope.rs`, which lives outside this crate, can enumerate the
/// registered tools through [`tool_router`](Self::tool_router) and require each
/// one to have declared a run-scope decision. That anti-drift test is the only
/// thing that makes the allow table hard to forget, and it cannot be written
/// against a private router.
#[tool_router(router = board_router, vis = "pub")]
impl RimaiaServer {
    /// The operator's server, on `/mcp`: [`RunScope::Operator`], because that
    /// is what this constructor serves and task 020 takes nothing away from
    /// it.
    ///
    /// Takes the context already re-sourced by `mcp::build`, so nothing here
    /// has to remember that its writes are `mcp` (ADR-0019). `local` is this
    /// machine, when the host has one: `Some` serves the local router beside
    /// the board router, and `None`, a server's call, serves the board router
    /// alone (task 041).
    pub fn new(
        ctx: ServiceContext,
        provider: Arc<dyn AgentProvider>,
        local: Option<LocalTools>,
    ) -> Self {
        Self {
            ctx,
            scope: RunScope::Operator,
            provider,
            local,
        }
    }

    /// A server reached through `/mcp/run/{token}`: one run, one task, one
    /// grant.
    ///
    /// A second constructor rather than a parameter on [`new`](Self::new),
    /// because a scope is not something the operator path should be able to get
    /// wrong by passing the wrong argument. `local` is what the operator's
    /// server was given: a run is still offered every local tool, and calling
    /// one is [`RunScope::authorize`]'s refusal, never an unknown tool.
    pub fn scoped(
        ctx: ServiceContext,
        provider: Arc<dyn AgentProvider>,
        local: Option<LocalTools>,
        scope: RunScope,
    ) -> Self {
        Self {
            ctx,
            scope,
            provider,
            local,
        }
    }

    #[tool(
        description = "Report what this Rimaia installation has actually done over a period: what \
it spent, how many runs succeeded or failed, how many tasks reached review, how long runs take, \
which models were used, and what a completed task costs once its failed attempts are counted. \
Call this when the user asks whether Rimaia is worth what it costs, why their bill looks the way \
it does, or whether runs have started failing more often. Omit both bounds for all time; pass \
`from` and `to` as RFC 3339 instants otherwise. Read `runs_without_cost` before quoting a total \
— it is how many runs in the period recorded no cost at all, and a period that predates the \
capture columns is partly unrecorded rather than cheaper."
    )]
    pub async fn get_analytics(
        &self,
        Parameters(request): Parameters<AnalyticsRequest>,
    ) -> Result<Json<AnalyticsView>, ToolError> {
        self.scope.authorize(Tool::GetAnalytics, None)?;

        let report = analytics::analytics(
            &self.ctx,
            Period {
                from: request.from,
                to: request.to,
            },
        )
        .await?;
        Ok(Json(AnalyticsView::from(&report)))
    }

    #[tool(
        description = "Read what the user has told Rimaia their Claude subscription costs per \
month, or `null` when they have not said. Call this before comparing spend against a \
subscription — the figure is the *user's own* and Rimaia cannot verify it, so an absent one means \
the comparison must not be drawn rather than that it is free."
    )]
    pub async fn get_subscription_cost(&self) -> Result<Json<SubscriptionCostView>, ToolError> {
        self.scope.authorize(Tool::GetSubscriptionCost, None)?;

        Ok(Json(SubscriptionCostView {
            monthly_usd: db::settings::subscription_monthly_usd(&self.ctx).await?,
        }))
    }

    #[tool(
        description = "Record what the user pays for their Claude subscription each month, so the \
analytics page can show spend as a share of it. Call it only when the user states a figure; pass \
`null` to clear one. A negative figure is refused."
    )]
    pub async fn set_subscription_cost(
        &self,
        Parameters(request): Parameters<SubscriptionCostRequest>,
    ) -> Result<Json<SubscriptionCostView>, ToolError> {
        self.scope.authorize(Tool::SetSubscriptionCost, None)?;

        db::settings::set_subscription_monthly_usd(&self.ctx, request.monthly_usd).await?;
        Ok(Json(SubscriptionCostView {
            monthly_usd: db::settings::subscription_monthly_usd(&self.ctx).await?,
        }))
    }

    #[tool(
        description = "List the git repositories registered with Rimaia, with the id each one is \
known by. Call this before creating a task: every task belongs to exactly one repository, and \
`create_task` needs its `repository_id`, which is a UUID you cannot derive from the repository's \
name or path. Also call it when the user names a project you have not seen an id for in this \
session."
    )]
    pub async fn list_repositories(&self) -> Result<Json<RepositoryListView>, ToolError> {
        self.scope.authorize(Tool::ListRepositories, None)?;

        let repositories = repo::list(&self.ctx).await?;
        Ok(Json(RepositoryListView {
            repositories: repositories.into_iter().map(RepositoryView::from).collect(),
        }))
    }

    #[tool(
        description = "Hand a finished implementation plan to Rimaia as a new task on the user's \
board. Call this when you and the user have settled on what should be built and they want it \
implemented later, unattended, rather than right now in this session. Put the entire plan in \
`plan` as Markdown: it is the whole brief the implementing agent receives, so it must stand alone \
without this conversation — file paths, the approach, what \"done\" looks like, and anything you \
learned here that it would otherwise have to rediscover. Set `column` to `ready` only when the \
plan is complete enough to run with no further input, because `ready` is the run queue and a task \
placed there may start executing within the minute. Leave `column` unset (`not_ready`) for \
anything still being drafted."
    )]
    pub async fn create_task(
        &self,
        Parameters(request): Parameters<CreateTaskRequest>,
    ) -> Result<Json<TaskView>, ToolError> {
        self.scope.authorize(Tool::CreateTask, None)?;

        let created = tasks::create_task(
            &self.ctx,
            NewTask {
                repository_id: request.repository_id,
                title: request.title,
                plan: request.plan,
                extra_instructions: request.extra_instructions,
                column: request.column,
                links: request
                    .links
                    .into_iter()
                    .map(|link| NewTaskLink {
                        label: link.label,
                        url: link.url,
                    })
                    .collect(),
            },
        )
        .await?;

        self.task_view(&created.id).await
    }

    #[tool(
        description = "Read one task in full: its plan, its links, what it depends on, its current \
column and run state, and how its last run ended. Call this before `update_task` so you amend the \
existing plan rather than overwriting work you cannot see, after a run to find out what happened, \
and whenever the user refers to a task you only have the id of. This is the only tool that \
returns plan text; `list_tasks` deliberately omits it."
    )]
    pub async fn get_task(
        &self,
        Parameters(request): Parameters<GetTaskRequest>,
    ) -> Result<Json<TaskView>, ToolError> {
        self.scope
            .authorize(Tool::GetTask, Some(&request.task_id))?;

        self.task_view(&request.task_id).await
    }

    #[tool(
        description = "List the tasks on the user's board, optionally narrowed by repository, \
column or run state. Plans are omitted — call `get_task` for one task's plan. Call this to find \
the id of a task the user is describing by name, to see what is already queued before adding more \
work, to check what is waiting in `ready`, or to find the tasks a new one should depend on."
    )]
    pub async fn list_tasks(
        &self,
        Parameters(request): Parameters<ListTasksRequest>,
    ) -> Result<Json<TaskListView>, ToolError> {
        self.scope.authorize(Tool::ListTasks, None)?;

        let summaries = tasks::list_tasks(
            &self.ctx,
            TaskFilter {
                repository_id: request.repository_id,
                column: request.column,
                run_state: request.run_state,
                archived: request.archived,
            },
        )
        .await?;

        Ok(Json(TaskListView {
            tasks: summaries.into_iter().map(TaskListItem::from).collect(),
        }))
    }

    #[tool(
        description = "Change an existing task's title, plan, extra instructions, model, effort or \
strategy mode. Call this to amend a plan you have already handed over — read it with `get_task` \
first and send the full replacement text, because `plan` is replaced wholesale and is not appended \
to. Fields you do not mention keep their current value. Use `clear` to erase \
`extra_instructions`, `model` or `effort`; a plan cannot be erased over MCP. Set `strategy_mode` \
to `planned` for work whose model and effort a cheap planner run should decide, `manual` to fix \
them yourself, or `default` to inherit whatever the repository and the user's settings say; \
naming a `model` or an `effort` selects `manual` on its own. This tool does not move a task \
between columns — that is `move_task` — does not change what it depends on — that is \
`set_task_dependencies` — and does not record a planner's proposal — that is \
`set_task_strategy`."
    )]
    pub async fn update_task(
        &self,
        Parameters(request): Parameters<UpdateTaskRequest>,
    ) -> Result<Json<TaskView>, ToolError> {
        self.scope
            .authorize(Tool::UpdateTask, Some(&request.task_id))?;

        // Before the service call, because the request contradicts itself and
        // there is no patch to build from it.
        request.ensure_no_conflicting_clear()?;

        let cleared = |field: ClearableField| request.clear.contains(&field);
        let patch = TaskPatch {
            repository_id: request.repository_id.clone(),
            title: request.title.clone(),
            // A plain `Option`, not a `Patch`: `strategy_mode` is NOT NULL and
            // `default` is already how it spells "no opinion" (seam-contract
            // D17.6), so there is nothing for `Patch::Clear` to mean.
            strategy_mode: request.strategy_mode,
            // Set or leave alone. Never `Patch::Clear`: `plan` is not in
            // `ClearableField` at all (seam-contract D16).
            plan: patch_field(request.plan.clone(), false),
            extra_instructions: patch_field(
                request.extra_instructions.clone(),
                cleared(ClearableField::ExtraInstructions),
            ),
            model: patch_field(request.model.clone(), cleared(ClearableField::Model)),
            effort: patch_field(request.effort.clone(), cleared(ClearableField::Effort)),
        };

        let updated = tasks::update_task(&self.ctx, &request.task_id, patch).await?;
        self.task_view(&updated.id).await
    }

    #[tool(
        description = "Move a task to a different column, or change its priority within one. Call \
this when the user says a plan is finished and should be queued (`ready`), when they want it \
pulled back out of the queue (`not_ready`), or when they want it run ahead of something already \
waiting. Board order is execution order: with no neighbour named the task goes to the bottom of \
the destination column, which is the back of the queue; name `after_task_id` to place it directly \
above an existing task instead. A task cannot enter `ready` without a plan. `in_review` and \
`done` describe where a *human* is in reviewing finished work — set them only when the user \
explicitly asks you to."
    )]
    pub async fn move_task(
        &self,
        Parameters(request): Parameters<MoveTaskRequest>,
    ) -> Result<Json<TaskView>, ToolError> {
        self.scope
            .authorize(Tool::MoveTask, Some(&request.task_id))?;

        let before_id = match (&request.before_task_id, &request.after_task_id) {
            (None, None) => {
                self.bottom_of_column(&request.task_id, request.column)
                    .await?
            }
            _ => request.before_task_id.clone(),
        };

        let moved = tasks::move_task(
            &self.ctx,
            self.machine(),
            &request.task_id,
            request.column,
            before_id.as_deref(),
            request.after_task_id.as_deref(),
        )
        .await?;

        self.task_view(&moved.id).await
    }

    #[tool(
        description = "Attach an external reference to a task — an Asana task, a GitHub issue, a \
design doc, a Figma file. Call this when the user mentions a ticket or a document the \
implementing agent will need to open, or when you created a task and then learned about something \
relevant. Links appear on the card, and the base instructions can inject them into the run \
prompt, so a link is a better place for a URL than the middle of the plan text."
    )]
    pub async fn add_task_link(
        &self,
        Parameters(request): Parameters<AddTaskLinkRequest>,
    ) -> Result<Json<TaskView>, ToolError> {
        self.scope
            .authorize(Tool::AddTaskLink, Some(&request.task_id))?;

        tasks::add_task_link(
            &self.ctx,
            &request.task_id,
            NewTaskLink {
                label: request.label,
                url: request.url,
            },
        )
        .await?;

        self.task_view(&request.task_id).await
    }

    #[tool(
        description = "Remove one external reference from a task. Call this when a link is wrong \
or obsolete. Takes the link's own id, which `get_task` returns beside each link — not the task's \
id, and not the URL."
    )]
    pub async fn remove_task_link(
        &self,
        Parameters(request): Parameters<RemoveTaskLinkRequest>,
    ) -> Result<Json<TaskView>, ToolError> {
        // The one handler whose authorization is not literally its first
        // statement, because the request names a link and the scope is about a
        // task: resolve, then decide. The read is needed anyway — the answer is
        // the whole task, and after the delete there is no row left to say
        // which task that was.
        let link = tasks::get_task_link(&self.ctx, &request.link_id).await?;
        self.scope
            .authorize(Tool::RemoveTaskLink, Some(&link.task_id))?;

        tasks::remove_task_link(&self.ctx, &request.link_id).await?;

        self.task_view(&link.task_id).await
    }

    #[tool(
        description = "Declare which tasks must finish successfully before this one may start, \
replacing whatever it depended on before. Call this whenever you hand over several tasks that \
have to land in order — the API before the caller, the migration before the code that reads it — \
so that Rimaia runs them in sequence overnight and branches each dependent task off its \
dependency instead of off the default branch, which is what stops the second task being written \
against code that is not there yet. Send the complete list every time: this replaces the set, and \
an empty list clears every dependency. Every task involved must be in the same repository, and a \
set that would create a cycle is refused with the loop spelled out."
    )]
    pub async fn set_task_dependencies(
        &self,
        Parameters(request): Parameters<SetTaskDependenciesRequest>,
    ) -> Result<Json<TaskView>, ToolError> {
        self.scope
            .authorize(Tool::SetTaskDependencies, Some(&request.task_id))?;

        tasks::set_task_dependencies(&self.ctx, &request.task_id, &request.depends_on).await?;

        self.task_view(&request.task_id).await
    }

    #[tool(
        description = "Record the execution strategy for the task you were started to plan: which \
model and effort level its implementation run should spawn with, whether the work is worth \
splitting into phases, and why. Call this exactly once, as the last thing you do, and print \
nothing else — this call is the entire answer, and a strategy that is only written out in prose \
is not recorded at all. Use the model and effort ids exactly as the prompt listed them; they \
reach the command line unchanged. Omit either to leave it to the user's defaults. `phases` \
describes work you would split up: the agent implementing the task runs them itself, in one \
session, with its own subagents — nothing here starts a second run. `rationale` is read by a \
human in the morning, so say what about this particular plan made you choose as you did. Writing \
a strategy is refused unless the task is in `planned` mode, which is what stops a proposal \
overwriting a choice the user has made by hand."
    )]
    pub async fn set_task_strategy(
        &self,
        Parameters(request): Parameters<SetTaskStrategyRequest>,
    ) -> Result<Json<TaskView>, ToolError> {
        self.scope
            .authorize(Tool::SetTaskStrategy, Some(&request.task_id))?;

        // `Planner`, always. The tool exists for a planner run, and the mode
        // guard `tasks::set_task_strategy` applies is exactly the guard that
        // source asks for — letting a caller name itself `user` here would hand
        // it the way around the check (ADR-0006's amendment, D17.7). The panel's
        // own writes are `update_task` and `accept_task_strategy`, which take
        // authorship deliberately rather than by claiming it in a field.
        let task_id = request.task_id.clone();
        tasks::set_task_strategy(
            &self.ctx,
            &task_id,
            request.into_plan(),
            StrategySource::Planner,
        )
        .await?;

        self.task_view(&task_id).await
    }

    #[tool(
        description = "Read the standing instructions Rimaia prepends to every run in this \
workspace. Call this before writing a plan, so the plan does not repeat, contradict, or leave a \
gap in what is already asked of every run — whether runs are expected to commit as they go, run \
the tests and linters, or open a pull request when they finish. Returned verbatim, with `{{…}}` \
placeholders left unexpanded; those are substituted per task when a run actually starts, and \
`template_variables` lists the ones that exist."
    )]
    pub async fn get_base_instructions(&self) -> Result<Json<BaseInstructionsView>, ToolError> {
        self.scope.authorize(Tool::GetBaseInstructions, None)?;

        // Deliberately the stored template, not a composed preview: composing
        // needs a task and a repository (ADR-0009), and an agent asking "what
        // will be prepended to my plan?" has no task yet.
        let base_instructions = db::settings::base_instructions(&self.ctx).await?;

        Ok(Json(BaseInstructionsView {
            base_instructions,
            template_variables: TEMPLATE_VARIABLES
                .iter()
                .map(|name| name.to_string())
                .collect(),
        }))
    }

    // ADR-0021's capability parity. Each of the eight below had a Tauri command
    // and no tool, so the window could configure execution and an agent could
    // not. All are operator-only: they either reconfigure the installation or
    // speak for a human, and `Tool::run_access` is where that is argued.

    #[tool(
        description = "Read the models and effort levels a task may be given, and the planner's \
own budget. Call this before `set_task_strategy` or `update_task`: the ids here are the exact \
strings that reach the CLI, and a model that is not listed is one this installation has not been \
told about."
    )]
    pub async fn get_strategy_catalogue(&self) -> Result<Json<Catalogue>, ToolError> {
        self.scope.authorize(Tool::GetStrategyCatalogue, None)?;
        Ok(Json(
            strategy::catalogue::catalogue(&self.ctx, self.provider.as_ref()).await?,
        ))
    }

    #[tool(
        description = "Replace the model and effort catalogue with a JSON document. This is how a \
newly released model becomes selectable without a new version of Rimaia. Call this after reading \
the current catalogue, and send the whole document: it replaces rather than merges. It is validated \
before it is stored, so an unparseable one is refused and the previous catalogue is left alone."
    )]
    pub async fn set_strategy_catalogue(
        &self,
        Parameters(request): Parameters<SetStrategyCatalogueRequest>,
    ) -> Result<Json<Catalogue>, ToolError> {
        self.scope.authorize(Tool::SetStrategyCatalogue, None)?;
        strategy::catalogue::set_catalogue(&self.ctx, &request.catalogue).await?;
        Ok(Json(
            strategy::catalogue::catalogue(&self.ctx, self.provider.as_ref()).await?,
        ))
    }

    #[tool(
        description = "Read the execution strategy a task falls back to when it names none of its \
own. Call this before setting a task's strategy, to see what it would already inherit — one \
repository's defaults, or the global ones beneath them when `repository_id` is omitted."
    )]
    pub async fn get_strategy_defaults(
        &self,
        Parameters(request): Parameters<GetStrategyDefaultsRequest>,
    ) -> Result<Json<StrategyDefaults>, ToolError> {
        self.scope.authorize(Tool::GetStrategyDefaults, None)?;
        Ok(Json(match request.repository_id.as_deref() {
            Some(repository_id) => {
                strategy::settings::repository_default(&self.ctx, repository_id).await?
            }
            None => strategy::settings::global_default(&self.ctx).await?,
        }))
    }

    #[tool(
        description = "Set the default execution strategy for one repository, or globally when \
`repository_id` is omitted. Call this instead of editing cards one by one: a repository of small \
tasks can be defaulted low here without touching any of them. It replaces the whole record rather \
than patching it, so sending no `model` means the default names no model."
    )]
    pub async fn set_strategy_defaults(
        &self,
        Parameters(request): Parameters<SetStrategyDefaultsRequest>,
    ) -> Result<Json<StrategyDefaults>, ToolError> {
        self.scope.authorize(Tool::SetStrategyDefaults, None)?;

        let defaults = StrategyDefaults {
            mode: request.mode,
            model: request.model.clone(),
            effort: request.effort.clone(),
        };
        match request.repository_id.as_deref() {
            Some(repository_id) => {
                strategy::settings::set_repository_default(&self.ctx, repository_id, &defaults)
                    .await?
            }
            None => strategy::settings::set_global_default(&self.ctx, &defaults).await?,
        }
        Ok(Json(defaults))
    }

    #[tool(
        description = "Read whether a planned strategy waits for a human to accept it before the \
implementation run starts, or proceeds automatically. Call this before queueing planned work \
overnight — `manual` will stop the queue at every planned task."
    )]
    pub async fn get_strategy_approval(&self) -> Result<Json<StrategyApprovalView>, ToolError> {
        self.scope.authorize(Tool::GetStrategyApproval, None)?;
        Ok(Json(StrategyApprovalView {
            approval: strategy::settings::approval(&self.ctx).await?,
        }))
    }

    #[tool(
        description = "Set whether a planned strategy waits for a human before the implementation \
run. Call this with `automatic` for an overnight queue; `manual` stops the queue at every planned \
task until somebody accepts it, which is only useful while someone is watching."
    )]
    pub async fn set_strategy_approval(
        &self,
        Parameters(request): Parameters<SetStrategyApprovalRequest>,
    ) -> Result<Json<StrategyApprovalView>, ToolError> {
        self.scope.authorize(Tool::SetStrategyApproval, None)?;
        strategy::settings::set_approval(&self.ctx, request.approval).await?;
        Ok(Json(StrategyApprovalView {
            approval: request.approval,
        }))
    }

    #[tool(
        description = "Stop retrying a task that is waiting to resume, landing it in `failed` so a \
human reviews it instead. Call this when the error on its last run will not clear on its own and \
the remaining attempts would hit the same wall; it speaks for that human, so a run cannot call it. \
Use `move_task` for a card that simply belongs somewhere else, and cancel rather than this for a \
run that is still in flight."
    )]
    pub async fn give_up_on_task(
        &self,
        Parameters(request): Parameters<TaskStrategyRequest>,
    ) -> Result<Json<TaskView>, ToolError> {
        self.scope.authorize(Tool::GiveUpOnTask, None)?;
        scheduler::give_up(&self.ctx, &request.task_id).await?;
        self.task_view(&request.task_id).await
    }

    #[tool(
        description = "Take a task off the user's board without deleting anything. The task keeps \
its run history, its links and its dependency edges, and can be put back with `unarchive_task`. \
Call this instead of asking a human to delete a task when a card is finished with: it is the \
safe way to tidy a board. Archiving also fires whatever cleanup the repository is configured for, which may \
remove the task's git worktree or run a script the user wrote, so the result tells you what it \
did. A task that is running or waiting to retry is refused: cancel the run first."
    )]
    pub async fn archive_task(
        &self,
        Parameters(request): Parameters<ArchiveTaskRequest>,
    ) -> Result<Json<ArchivedTaskView>, ToolError> {
        self.scope.authorize(Tool::ArchiveTask, None)?;
        let archived = tasks::archive_task(&self.ctx, self.machine(), &request.task_id).await?;
        Ok(Json(archived.into()))
    }

    #[tool(
        description = "Archive several tasks at once, in the order given. Call this to tidy a \
finished milestone in one go. Nothing stops at the first refusal: the result lists what was archived and what was left alone with the reason for \
each, so a set containing one running task still archives the rest. Use `archive_task` instead when you are asking about one card and want the \
refusal as an error."
    )]
    pub async fn archive_tasks(
        &self,
        Parameters(request): Parameters<ArchiveTasksRequest>,
    ) -> Result<Json<ArchiveReportView>, ToolError> {
        self.scope.authorize(Tool::ArchiveTasks, None)?;
        let report = tasks::archive_tasks(&self.ctx, self.machine(), &request.task_ids).await?;
        Ok(Json(report.into()))
    }

    #[tool(
        description = "Put an archived task back on the board, in the column it was in. Call \
this when a user asks for an archived card back, or when work you thought was finished turns out \
not to be. Nothing the archive's cleanup deleted comes back — a removed worktree stays removed \
and the next run recreates it. Use `list_tasks` with `archived: archived` to find the task id."
    )]
    pub async fn unarchive_task(
        &self,
        Parameters(request): Parameters<ArchiveTaskRequest>,
    ) -> Result<Json<TaskView>, ToolError> {
        self.scope.authorize(Tool::UnarchiveTask, None)?;
        tasks::unarchive_task(&self.ctx, &request.task_id).await?;
        let detail = tasks::get_task(&self.ctx, &request.task_id).await?;
        Ok(Json(TaskView::from(detail)))
    }

    #[tool(
        description = "Approve a task that is waiting in review: it moves to the bottom of `done`, \
and its dependents stop being blocked. Call this only when a human asked you to, or when the work \
has been checked. Refused when the task is not in review, is archived, or has a run queued, running \
or waiting to retry. Approving also removes the task's worktree if the user turned on automatic \
cleanup for done tasks."
    )]
    pub async fn approve_task(
        &self,
        Parameters(request): Parameters<ArchiveTaskRequest>,
    ) -> Result<Json<ReviewedTaskView>, ToolError> {
        self.scope.authorize(Tool::ApproveTask, None)?;
        let task = review::approve(&self.ctx, self.machine(), &request.task_id).await?;
        Ok(Json(task.into()))
    }

    #[tool(
        description = "Send a reviewed task back for another round, KEEPING its branch, its \
worktree and every commit on it. The task goes to the bottom of `ready` with your note appended \
to its extra instructions, and the next run continues on the same commits and reads the note. Call \
this when the work is on the right track and needs fixing. Do not use it when the approach is \
wrong: that is `reject_task`. The note is required. A task whose last run failed or was cancelled \
is refused: use Retry for it. The result lists every task that depends on this one, because both \
this and `reject_task` take it out of review and so block them."
    )]
    pub async fn request_task_changes(
        &self,
        Parameters(request): Parameters<ReviewNoteRequest>,
    ) -> Result<Json<ReviewOutcomeView>, ToolError> {
        self.scope.authorize(Tool::RequestTaskChanges, None)?;
        let outcome = review::request_changes(&self.ctx, &request.task_id, &request.note).await?;
        Ok(Json(outcome.into()))
    }

    #[tool(
        description = "Throw a reviewed task's work away and start it over. The task goes to the \
bottom of `ready` with your note appended, its worktree directory is removed and its branch is \
cleared, so the next run starts on a FRESH branch from the base and does NOT contain the rejected \
commits. The old branch is not deleted: it stays in git, and `set_aside_branch` names it. Any pull \
request opened from it is left as it was. Call this when the approach is wrong; call \
`request_task_changes` when the work should be built on instead. Refused, with the count, when the \
worktree has uncommitted changes, because removing them would lose them for good; there is no way \
to force it. The note is required. The result lists every dependent, with `built_on` set for the \
ones that already ran on this task's work."
    )]
    pub async fn reject_task(
        &self,
        Parameters(request): Parameters<ReviewNoteRequest>,
    ) -> Result<Json<ReviewOutcomeView>, ToolError> {
        self.scope.authorize(Tool::RejectTask, None)?;
        let outcome =
            review::reject(&self.ctx, self.machine(), &request.task_id, &request.note).await?;
        Ok(Json(outcome.into()))
    }

    #[tool(
        description = "List the tasks that depend directly on this one, archived ones included, \
with `built_on` true for each that already ran on top of this task's work. Call this before \
`reject_task` or `request_task_changes`: either one blocks every dependent until the task \
succeeds again."
    )]
    pub async fn get_task_dependents(
        &self,
        Parameters(request): Parameters<ArchiveTaskRequest>,
    ) -> Result<Json<TaskDependentsView>, ToolError> {
        self.scope.authorize(Tool::GetTaskDependents, None)?;
        let dependents = review::dependents(&self.ctx, &request.task_id).await?;
        Ok(Json(TaskDependentsView {
            dependents: dependents.into_iter().map(Into::into).collect(),
        }))
    }

    #[tool(
        description = "What the queue did since the last review was finished: one entry per task \
(never per run), failures and blocked chains first, with run totals and the cost where it was \
recorded. A quiet board gives an empty digest. Call this to brief a human on the night."
    )]
    pub async fn get_review_digest(&self) -> Result<Json<ReviewDigestView>, ToolError> {
        self.scope.authorize(Tool::GetReviewDigest, None)?;
        // This runner's consent, when the server has a machine to read it on
        // (task 066); the shell's server always does.
        let digest = review::digest(&self.ctx, self.local.as_ref().map(|l| &l.machine)).await?;
        Ok(Json(digest.into()))
    }

    #[tool(
        description = "Mark the review digest as seen through an instant, normally the `until` of \
the digest that was shown, so the next digest starts after it. It never moves backwards, and a \
time in the future is refused. Call this once a digest has been read."
    )]
    pub async fn mark_review_digest_seen(
        &self,
        Parameters(request): Parameters<MarkReviewDigestSeenRequest>,
    ) -> Result<Json<DigestMarkerView>, ToolError> {
        self.scope.authorize(Tool::MarkReviewDigestSeen, None)?;
        let seen_through = review::mark_seen(&self.ctx, request.through).await?;
        Ok(Json(DigestMarkerView { seen_through }))
    }

    #[tool(
        description = "Record what this review found, in ONE call. Call this as the last thing you do. \
List every finding with its severity (`critical`, `high`, `medium` or `low`), a one-line `title`, \
a `body` saying what is wrong and why it matters, and the repository-relative `file` and `line` \
when it is about one place. When you found nothing, call this with `findings: []`: a review that \
never calls is a failed review, not a clean one. A second call is refused. Only a review run can \
call this, and only for its own task."
    )]
    pub async fn record_review_findings(
        &self,
        Parameters(request): Parameters<RecordReviewFindingsRequest>,
    ) -> Result<Json<ReviewFindingsView>, ToolError> {
        self.scope
            .authorize(Tool::RecordReviewFindings, Some(&request.task_id))?;
        let review_run_id = self.grant_run_id(Tool::RecordReviewFindings)?;
        let recorded =
            review::findings::record(&self.ctx, &request.task_id, review_run_id, request.findings)
                .await?;
        Ok(Json(recorded.into()))
    }

    #[tool(
        description = "Say what you did about one open review finding on your task: `fixed`, \
with an optional `resolution` saying how, or `rejected`, with a `resolution` saying why the \
finding is wrong or not worth fixing (required). A \
finding already resolved is refused. Call this once per finding you were handed; only a fix run \
can, and only for its own task."
    )]
    pub async fn resolve_review_finding(
        &self,
        Parameters(request): Parameters<ResolveReviewFindingRequest>,
    ) -> Result<Json<ReviewFindingView>, ToolError> {
        self.scope
            .authorize(Tool::ResolveReviewFinding, Some(&request.task_id))?;
        let fix_run_id = self.grant_run_id(Tool::ResolveReviewFinding)?;
        let task_id = request.task_id.clone();
        let finding_id = request.finding_id.clone();
        let resolved = review::findings::resolve(
            &self.ctx,
            &task_id,
            &finding_id,
            fix_run_id,
            request.into_resolution(),
        )
        .await?;
        Ok(Json(resolved.into()))
    }

    #[tool(
        description = "List what review runs found on a task, oldest review first and each \
review's findings in the order the reviewer gave them, optionally only those with one `status` \
(`open`, `fixed` or `rejected`). A fixed or rejected finding carries the fix run's `resolution`. \
Call this to tell the user what the automated review raised and what became of it."
    )]
    pub async fn list_review_findings(
        &self,
        Parameters(request): Parameters<ListReviewFindingsRequest>,
    ) -> Result<Json<ReviewFindingsView>, ToolError> {
        self.scope
            .authorize(Tool::ListReviewFindings, Some(&request.task_id))?;
        let findings = review::findings::list(&self.ctx, &request.task_id, request.status).await?;
        Ok(Json(findings.into()))
    }

    #[tool(
        description = "Read what a task's automated review came to: every loop the task has had, \
oldest first and the newest last. A loop is an implementation and the rounds after it; a round is \
a review, the findings it raised (each with `status`, its `resolution` and whether it is \
`blocking` under the task's effective `blocking_severity`), the fix that followed and what that \
fix resolved, and `regressed` and `new_after_fix` for the signs of a loop going in circles. Each \
loop carries its `verdict` and its open blocking and advisory counts. Call this to tell the user \
what the reviewer could not fix."
    )]
    pub async fn get_review_history(
        &self,
        Parameters(request): Parameters<GetReviewHistoryRequest>,
    ) -> Result<Json<ReviewHistoryView>, ToolError> {
        self.scope
            .authorize(Tool::GetReviewHistory, Some(&request.task_id))?;
        let history = review_loop::history(&self.ctx, &request.task_id).await?;
        Ok(Json(history.into()))
    }

    #[tool(
        description = "Call this before changing the review-and-fix loop, or when the user asks how \
automated review is set up. It reads the loop's global settings: the review instructions every \
review run is given (often just the name of the user's own review skill or slash command), and the \
loop's configuration. In the configuration every field is optional and an absent one inherits: \
`enabled` (`off` or `on_cost_acknowledged`; absent is off), `max_review_loops` (how many fix \
phases one loop may spend, 0 to 5, default 2), `blocking_severity` (the least severity that \
starts a fix: `critical`, `high`, `medium` or `low`; default `medium`), `review_model` and \
`review_effort` (absent means the task's own strategy) and `fix_session` (`fresh` or `resume`). \
A repository and a task can each override any field."
    )]
    pub async fn get_review_settings(&self) -> Result<Json<ReviewSettings>, ToolError> {
        self.scope.authorize(Tool::GetReviewSettings, None)?;
        Ok(Json(
            review_loop::config::get_review_settings(&self.ctx).await?,
        ))
    }

    #[tool(
        description = "Read one level of the review-and-fix loop's configuration next to what it \
inherits. `level` is `global`, `repository` or `task`; the last two need the `id` of the \
repository or task. `config` is what that level stores, `inherited` is what each field becomes if \
the level stops setting it (the level above's answer), and `effective` is what the level resolves \
to with its own settings applied. Call this to tell the user what a repository or a task would \
actually do, rather than working the precedence out yourself."
    )]
    pub async fn get_review_level(
        &self,
        Parameters(request): Parameters<GetReviewLevelRequest>,
    ) -> Result<Json<ReviewLevel>, ToolError> {
        self.scope.authorize(Tool::GetReviewLevel, None)?;
        Ok(Json(
            review_loop::config::get_review_level(&self.ctx, request.level, request.id.as_deref())
                .await?,
        ))
    }

    #[tool(
        description = "Call this when the user wants to change how every task is reviewed. It \
replaces the review-and-fix loop's global settings: `instructions` (the text \
every review run is given; empty for none) and `config` (see `get_review_settings`; null or {} \
for nothing set). The loop multiplies what every task costs, so turning it on is spelled \
`\"enabled\": \"on_cost_acknowledged\"` and nothing else: confirm the cost with the user before you \
send it. `true` or `\"on\"` is refused, as is a `max_review_loops` above 5 or a model or effort \
that is not in the strategy catalogue."
    )]
    pub async fn set_review_settings(
        &self,
        Parameters(request): Parameters<SetReviewSettingsRequest>,
    ) -> Result<Json<ReviewSettings>, ToolError> {
        self.scope.authorize(Tool::SetReviewSettings, None)?;
        Ok(Json(
            review_loop::config::set_review_settings(
                &self.ctx,
                self.provider.as_ref(),
                &request.instructions,
                request.config,
            )
            .await?,
        ))
    }

    #[tool(
        description = "Call this when the user wants one repository reviewed differently. It \
replaces that repository's review-and-fix loop configuration, which overrides \
the global one field by field (see `get_review_settings`; null or {} inherits everything). Turning \
the loop on is spelled `\"enabled\": \"on_cost_acknowledged\"`; confirm the cost with the user first."
    )]
    pub async fn set_repository_review_config(
        &self,
        Parameters(request): Parameters<SetRepositoryReviewConfigRequest>,
    ) -> Result<Json<ReviewConfig>, ToolError> {
        self.scope
            .authorize(Tool::SetRepositoryReviewConfig, None)?;
        Ok(Json(
            review_loop::config::set_repository_review_config(
                &self.ctx,
                self.provider.as_ref(),
                &request.repository_id,
                request.config,
            )
            .await?,
        ))
    }

    #[tool(
        description = "Call this when the user wants one task reviewed differently. It replaces \
that task's review settings: `review_instructions`, which REPLACES the \
global review instructions for this task when it says anything (omit it or send blank to use the \
global ones), and `config`, which overrides the repository's and the global configuration field by \
field (see `get_review_settings`). Turning the loop on is spelled \
`\"enabled\": \"on_cost_acknowledged\"`; confirm the cost with the user first."
    )]
    pub async fn set_task_review(
        &self,
        Parameters(request): Parameters<SetTaskReviewRequest>,
    ) -> Result<Json<TaskReview>, ToolError> {
        self.scope
            .authorize(Tool::SetTaskReview, Some(&request.task_id))?;
        Ok(Json(
            review_loop::config::set_task_review(
                &self.ctx,
                self.provider.as_ref(),
                &request.task_id,
                request.review_instructions,
                request.config,
            )
            .await?,
        ))
    }

    #[tool(
        description = "Accept a planner's proposal on behalf of the user, marking the strategy as \
theirs rather than the planner's. A later planner run will then leave it alone. Call this when a human has \
reviewed a proposal and is happy with it; it speaks for that human, so a run cannot call it — \
not even about its own task."
    )]
    pub async fn accept_task_strategy(
        &self,
        Parameters(request): Parameters<TaskStrategyRequest>,
    ) -> Result<Json<TaskView>, ToolError> {
        self.scope.authorize(Tool::AcceptTaskStrategy, None)?;
        let task = tasks::strategy::accept_task_strategy(&self.ctx, &request.task_id).await?;
        self.task_view(&task.id).await
    }

    #[tool(
        description = "Discard a task's recorded strategy proposal so a planned task will be \
planned again on its next run. Call this after a planner failed, or when the plan has changed \
enough that the old proposal no longer describes the work."
    )]
    pub async fn clear_task_strategy(
        &self,
        Parameters(request): Parameters<TaskStrategyRequest>,
    ) -> Result<Json<TaskView>, ToolError> {
        self.scope.authorize(Tool::ClearTaskStrategy, None)?;
        let task = tasks::strategy::clear_task_strategy(&self.ctx, &request.task_id).await?;
        self.task_view(&task.id).await
    }
}

/// The tools that inspect, reconfigure or spawn on this machine (ADR-0035
/// point 6, task 041): the doctor and its dismissals, credentials and
/// worktrees, capacity, schedules, and the two planning tools, which are here
/// because they spawn.
///
/// Registered only when the host handed in a [`LocalTools`], so a server that
/// has no machine serves none of them: calling one there is an unknown tool.
/// Each reaches this machine only through `self.local()`, and none issues a
/// board query of its own; a board fact comes through a named core read
/// function over the board context (D32's 2026-10-04 amendment), for task 059
/// to convert.
#[tool_router(router = local_router, vis = "pub")]
impl RimaiaServer {
    #[tool(
        description = "Check this Rimaia installation for the environment problems that make an \
overnight queue fail: a missing or signed-out Claude Code CLI, a git too old for worktrees, a \
GitHub CLI that cannot open a pull request, an unwritable or full data directory, a registered \
repository whose directory has moved, and an MCP port nothing is listening on. Call this when the \
user reports that runs are failing, before telling them to start the queue, or when \
`start_queue` has refused — every result carries the specific command that fixes it, and \
`is_blocking` says whether the queue would refuse to start right now."
    )]
    pub async fn run_doctor(&self) -> Result<Json<DoctorReportView>, ToolError> {
        self.scope.authorize(Tool::RunDoctor, None)?;
        let local = self.local()?;

        let report = doctor::run(&local.machine, &self.ctx, &local.doctor).await?;
        Ok(Json(DoctorReportView::from(&report)))
    }

    #[tool(
        description = "Record that the first-run walkthrough has been seen, so Rimaia opens on \
the board instead of the welcome screen. Call it only when the user says they are done with \
setup, or want to skip it — it changes nothing about how runs work, and un-dismissing it is not \
something this surface offers."
    )]
    pub async fn dismiss_onboarding(&self) -> Result<Json<OnboardingView>, ToolError> {
        self.scope.authorize(Tool::DismissOnboarding, None)?;
        let local = self.local()?;

        db::settings::set_onboarding_dismissed(&local.machine, true).await?;
        Ok(Json(OnboardingView {
            onboarding_dismissed: true,
        }))
    }

    #[tool(
        description = "Run the strategy planner for one task now and wait for it to finish, then \
report the model, effort and rationale it proposed. Call this when the user wants to see how a \
task will be modelled before committing to the expensive implementation run — a planner costs a \
few cents and a handful of turns, where the run it is checking costs on the order of a dollar. \
The task must resolve to `planned` mode and its repository must have opted into unattended runs; \
a task that already carries a proposal is re-planned, which is what this tool means that \
`plan_tasks_strategy` deliberately does not."
    )]
    pub async fn plan_task_strategy(
        &self,
        Parameters(request): Parameters<TaskStrategyRequest>,
    ) -> Result<Json<PlanResultView>, ToolError> {
        self.scope
            .authorize(Tool::PlanTaskStrategy, Some(&request.task_id))?;
        let local = self.local()?;

        let claim = runner_strategy::claim_for_planning(
            local.planner.board.as_ref(),
            &local.machine,
            &local.planner.in_flight,
            &request.task_id,
            scheduler::SlotOwner::Manual,
        )
        .await?;

        let (title, outcome) = match claim {
            Ok(claim) => {
                let title = claim.title().to_string();
                let outcome = runner_strategy::plan_claimed(
                    local.planner.board.as_ref(),
                    &local.machine,
                    &self.ctx,
                    &local.planner.paths,
                    &local.planner.runner,
                    claim,
                )
                .await?;
                (title, outcome)
            }
            Err(skip) => {
                let detail = tasks::get_task(&self.ctx, &request.task_id).await?;
                (detail.task.title, PlanOutcome::Skipped(skip))
            }
        };

        Ok(Json(PlanResultView::new(
            &request.task_id,
            &title,
            &outcome,
        )))
    }

    #[tool(
        description = "Plan a whole column, a whole repository or a hand-picked set of tasks in \
one pass, one planner at a time, and report what each one was modelled as. This is the preflight \
the user runs before leaving for the evening: spending forty cents to see how ten cards are \
modelled, before committing forty dollars of implementation, is the cheapest check this product \
offers. Call it when the user asks to check how a set of tasks will run, or before starting the \
queue on a column they have not reviewed. State at least one of `column`, `repository_id` or \
`task_ids`; each one narrows the set, and a selection that states none of them is refused rather \
than taken to mean the whole board. A card that already carries a proposal is skipped with that \
named as the reason and its proposal left untouched — use `plan_task_strategy` to re-plan one \
deliberately."
    )]
    pub async fn plan_tasks_strategy(
        &self,
        Parameters(request): Parameters<PlanSelectionRequest>,
    ) -> Result<Json<PlanPassView>, ToolError> {
        self.scope.authorize(Tool::PlanTasksStrategy, None)?;
        let local = self.local()?;

        let selection: PlanSelection = request.into();
        // A pass reached over MCP has no Cancel button to trip, so the signal
        // is one nothing holds. It is still passed rather than made optional:
        // one loop, one cancellation story, and the surface that *does* have a
        // button hands in a real one.
        let cancel = crate::runner::CancelSignal::new();
        let pass = runner_strategy::plan_all(
            local.planner.board.as_ref(),
            &local.machine,
            &self.ctx,
            &local.planner.paths,
            &local.planner.runner,
            &local.planner.in_flight,
            &selection,
            &cancel,
            &|_progress| {},
        )
        .await?;

        Ok(Json(PlanPassView::from(&pass)))
    }

    #[tool(
        description = "Report whether one repository carries a forge token of its own, whose \
account it belongs to, what the user called it, and whether this machine's keychain still holds \
it. Call this when a run failed to push or to open a pull request, or before telling the user \
their token is fine — a repository whose credential is configured and whose keychain item has \
gone refuses to run rather than falling back to the operator's own login, and this is what says \
so. **The token itself is never returned by anything**, and there is no tool that sets or \
removes one: a live forge token has no business travelling over this protocol."
    )]
    pub async fn get_repository_credential_status(
        &self,
        Parameters(request): Parameters<RepositoryRequest>,
    ) -> Result<Json<CredentialStatusView>, ToolError> {
        self.scope
            .authorize(Tool::GetRepositoryCredentialStatus, None)?;
        let local = self.local()?;

        let repository = repo::get(&self.ctx, &request.repository_id).await?;
        let checkout = machine::checkout_of(&local.machine, &repository).await?;
        let store = local
            .planner
            .runner
            .credentials
            .status(&repository.id)
            .await;

        Ok(Json(CredentialStatusView::new(
            &repository,
            &checkout,
            store,
        )))
    }

    #[tool(
        description = "Put down one doctor warning the user has read and decided about, so it \
stops appearing in the banner above every screen. Take `check`, `repository` and `detail` \
verbatim from a `run_doctor` row — all three are the key, so the same check about a different \
repository stays visible, and the warning comes back by itself if its `detail` ever changes. \
This is presentation only: it never changes whether the queue will start, and a `fail` row \
cannot be dismissed at all. Call it when the user says they know about a warning and want it \
out of the way, never to tidy up a report on their behalf."
    )]
    pub async fn dismiss_doctor_warning(
        &self,
        Parameters(request): Parameters<DoctorDismissalRequest>,
    ) -> Result<Json<DoctorDismissalsView>, ToolError> {
        self.scope.authorize(Tool::DismissDoctorWarning, None)?;
        let local = self.local()?;

        let dismissals = doctor::dismiss(&local.machine, request.into()).await?;
        Ok(Json(DoctorDismissalsView {
            dismissals: dismissals.iter().map(DismissalView::from).collect(),
        }))
    }

    #[tool(
        description = "Bring a dismissed doctor warning back, so it appears in the banner again. \
Call this when the user asks to see a warning they previously put down, or wants to tidy up \
dismissals that no longer apply. Take the three fields from `run_doctor`'s `dismissals` list, \
which holds every dismissal on record — including ones that match no current row, because the \
environment was fixed or the warning's wording changed. Removing one of those is how they are \
cleared."
    )]
    pub async fn restore_doctor_warning(
        &self,
        Parameters(request): Parameters<DoctorDismissalRequest>,
    ) -> Result<Json<DoctorDismissalsView>, ToolError> {
        self.scope.authorize(Tool::RestoreDoctorWarning, None)?;
        let local = self.local()?;

        let dismissals = doctor::restore(&local.machine, &request.into()).await?;
        Ok(Json(DoctorDismissalsView {
            dismissals: dismissals.iter().map(DismissalView::from).collect(),
        }))
    }

    // Task 016's read surface. The three commands that *delete* a worktree
    // have no tool here, and that is not an oversight — see this module's own
    // note and seam-contract D20. What an agent can do is find out what is on
    // the disk and say so, which is the half of the problem it can help with
    // without being able to make it irreversible.

    #[tool(
        description = "List every git worktree Rimaia has created, with the task it belongs to, \
its branch, its size on disk, when anything last wrote in it, and whether its branch is already \
merged into the repository's default branch. Call this when the user asks what is taking up \
space, or before suggesting a cleanup: `uncommitted_changes` and `unpushed_commits` are work that \
exists nowhere else, and a worktree with either is one to leave alone. Removing a worktree is \
deliberately not available here — it is irreversible, so it lives only in Settings → Storage, \
where a human confirms it."
    )]
    pub async fn list_worktrees(&self) -> Result<Json<WorktreeListView>, ToolError> {
        self.scope.authorize(Tool::ListWorktrees, None)?;
        let local = self.local()?;

        let inventory = worktree::inventory(&self.ctx, &local.machine).await?;
        Ok(Json(WorktreeListView {
            worktrees: inventory
                .entries
                .into_iter()
                .map(WorktreeView::from)
                .collect(),
            total_bytes: inventory.total_bytes,
        }))
    }

    #[tool(
        description = "Read whether a task reaching the `done` column automatically has its git \
worktree removed. Call this before advising on disk usage: when it is `off`, which is the \
default, every finished task keeps a full checkout until somebody clears it by hand, and that is \
usually the explanation for a large `worktrees` directory."
    )]
    pub async fn get_worktree_auto_cleanup(
        &self,
    ) -> Result<Json<WorktreeAutoCleanupView>, ToolError> {
        self.scope.authorize(Tool::GetWorktreeAutoCleanup, None)?;
        let local = self.local()?;
        Ok(Json(WorktreeAutoCleanupView {
            setting: worktree::auto_cleanup(&local.machine).await?,
        }))
    }

    #[tool(
        description = "Turn automatic worktree removal on or off. Call it with \
`on_done_acknowledged` only after telling the user what it deletes: every task they move to \
`done` will lose its checkout, including any uncommitted file in it that a run left behind. It \
never forces and never deletes a branch, so work that was committed survives — but work that was \
not is gone. `off` restores the default."
    )]
    pub async fn set_worktree_auto_cleanup(
        &self,
        Parameters(request): Parameters<SetWorktreeAutoCleanupRequest>,
    ) -> Result<Json<WorktreeAutoCleanupView>, ToolError> {
        self.scope.authorize(Tool::SetWorktreeAutoCleanup, None)?;
        let local = self.local()?;
        worktree::set_auto_cleanup(&local.machine, request.setting).await?;
        Ok(Json(WorktreeAutoCleanupView {
            setting: request.setting,
        }))
    }

    #[tool(
        description = "Call this to choose what archiving a task in one repository cleans up: `none` leaves \
everything alone, `remove_worktree` deletes the task's checkout using Rimaia's own guards (it \
refuses a dirty or unpushed worktree and never deletes a branch), and `script` runs an executable \
the user names instead. Tell the user before setting `script` that Rimaia then does no cleanup \
of its own and applies none of those guards — their script can delete uncommitted work. The \
script must be an absolute path to an executable file, not a command line; it is run with the \
repository as its working directory and is given RIMAIA_TASK_ID, RIMAIA_TASK_TITLE, \
RIMAIA_REPOSITORY_PATH, RIMAIA_BRANCH and RIMAIA_WORKTREE_PATH in its environment."
    )]
    pub async fn set_repository_on_archive(
        &self,
        Parameters(request): Parameters<SetRepositoryOnArchiveRequest>,
    ) -> Result<Json<RepositoryOnArchiveView>, ToolError> {
        self.scope.authorize(Tool::SetRepositoryOnArchive, None)?;
        let local = self.local()?;
        let checkout = archive::set_repository_on_archive(
            &self.ctx,
            &local.machine,
            &request.repository_id,
            request.on_archive,
            request.script,
        )
        .await?;
        Ok(Json(RepositoryOnArchiveView {
            repository_id: checkout.repository_id,
            on_archive: checkout.on_archive,
            script: checkout.on_archive_script,
        }))
    }

    // Task 012's four (ADR-0010). Every one is refused to a run — see
    // `scope::Tool::run_access` for the argument, which is ADR-0021 point 4's
    // second permanent refusal one layer out.

    #[tool(
        description = "Read how many runs Rimaia will have in flight at once: the mode \
(`sequential` or `parallel`), the configured limit, and the ceiling no setting can raise. Call \
this before queueing a long list overnight, to see whether it will be worked one at a time or \
several at once. The limit is reported as stored, so it survives a switch back to `sequential` \
even though sequential always runs exactly one."
    )]
    pub async fn get_run_capacity(&self) -> Result<Json<RunCapacityView>, ToolError> {
        self.scope.authorize(Tool::GetRunCapacity, None)?;
        let local = self.local()?;
        Ok(Json(capacity::configured(&local.machine).await?.into()))
    }

    #[tool(
        description = "Switch the run queue between one task at a time (`sequential`) and several \
at once (`parallel`). Call it before an evening of independent tasks across several repositories, \
which finishes far sooner in parallel. `sequential` is the safe default and the one that matches \
\"implement these in this order\"; it runs exactly one task at a time whatever the configured \
limit says."
    )]
    pub async fn set_schedule_mode(
        &self,
        Parameters(request): Parameters<SetScheduleModeRequest>,
    ) -> Result<Json<RunCapacityView>, ToolError> {
        self.scope.authorize(Tool::SetScheduleMode, None)?;
        let local = self.local()?;
        capacity::set_schedule_mode(&local.machine, request.mode).await?;
        Ok(Json(capacity::configured(&local.machine).await?.into()))
    }

    #[tool(
        description = "Set how many runs parallel mode may have in flight at once. Call this \
together with `set_schedule_mode`; on its own it changes nothing while the mode is `sequential`. \
Read `get_run_capacity` first for the ceiling — a value outside that range is refused rather than \
clamped. It bounds the queue in total; each repository still holds at most its own limit."
    )]
    pub async fn set_max_concurrency(
        &self,
        Parameters(request): Parameters<SetMaxConcurrencyRequest>,
    ) -> Result<Json<RunCapacityView>, ToolError> {
        self.scope.authorize(Tool::SetMaxConcurrency, None)?;
        let local = self.local()?;
        capacity::set_max_concurrency(&local.machine, request.max_concurrency).await?;
        Ok(Json(capacity::configured(&local.machine).await?.into()))
    }

    #[tool(
        description = "Set how many runs one repository will hold at once. Call this only when \
that repository's tasks genuinely do not interfere: git isolates the worktrees, but two agents in \
one repository fight over ports, test databases and lockfiles, which is why the default is 1 even \
in parallel mode and why raising it is a deliberate act. Parallelism across repositories is the \
safe kind and needs nothing here."
    )]
    pub async fn set_repository_max_concurrency(
        &self,
        Parameters(request): Parameters<SetRepositoryMaxConcurrencyRequest>,
    ) -> Result<Json<CheckoutView>, ToolError> {
        self.scope
            .authorize(Tool::SetRepositoryMaxConcurrency, None)?;
        let local = self.local()?;
        let checkout = repo::set_max_concurrency(
            &self.ctx,
            &local.machine,
            &request.repository_id,
            request.max_concurrency,
        )
        .await?;
        Ok(Json(CheckoutView::from(checkout)))
    }

    #[tool(
        description = "List this computer's clone of each repository: where it is, where its worktrees go, how many runs it holds at once, whether unattended runs are allowed in it here, and what archiving a task in it cleans up. Call this before explaining why a repository's tasks are not running tonight, or when the user asks where a project lives on disk. A repository from `list_repositories` that is missing here is not set up on this computer, and nothing of it runs here."
    )]
    pub async fn list_checkouts(&self) -> Result<Json<CheckoutListView>, ToolError> {
        self.scope.authorize(Tool::ListCheckouts, None)?;
        let local = self.local()?;
        let checkouts = repo::checkouts(&self.ctx, &local.machine).await?;
        Ok(Json(CheckoutListView {
            checkouts: checkouts.into_iter().map(CheckoutView::from).collect(),
        }))
    }
    // -----------------------------------------------------------------------
    // Schedules (task 013, ADR-0010). Operator-only, every one.
    // -----------------------------------------------------------------------

    #[tool(
        description = "List the schedules that start Rimaia's run queue by themselves, each with \
the time it will next fire. Call this whenever the user asks when work will run, or says a \
nightly queue did not happen — an overdue schedule reports the occurrence it *owes*, which is in \
the past, and a schedule whose cron expression cannot be read reports why instead of a time. \
Schedules are the operator's own standing instructions, so a run cannot read or change them."
    )]
    pub async fn list_schedules(&self) -> Result<Json<ScheduleListView>, ToolError> {
        self.scope.authorize(Tool::ListSchedules, None)?;
        let local = self.local()?;
        Ok(Json(ScheduleListView {
            schedules: schedule::list(&local.machine)
                .await?
                .into_iter()
                .map(ScheduleView::from)
                .collect(),
        }))
    }

    #[tool(
        description = "Create a schedule that starts the run queue at a chosen time — once, or \
every night. Call this when the user says something like \"run the queue at 22:00 and stop at \
06:00\": give it `cron` (\"0 22 * * *\") or `start_at`, never both, plus the IANA `timezone` the \
times are read in, which `list_timezones` supplies. `stop_at` is a local time of day at which the \
queue stops starting new tasks and lets the ones in flight finish. It spawns runs unattended, so \
a run cannot call it."
    )]
    pub async fn create_schedule(
        &self,
        Parameters(request): Parameters<ScheduleConfigRequest>,
    ) -> Result<Json<ScheduleView>, ToolError> {
        self.scope.authorize(Tool::CreateSchedule, None)?;
        let local = self.local()?;
        let created = schedule::create(&local.machine, request.into()).await?;
        Ok(Json(created.into()))
    }

    #[tool(
        description = "Replace a schedule's whole configuration — its name, times, timezone, stop \
time, mode and concurrency. Call this to change when or how a scheduled queue runs; send every \
field, because this replaces rather than patches, and the fields constrain each other. It leaves \
the schedule's fire history alone, so editing tonight's stop time does not make tonight's start \
happen again. Reconfiguring an unattended queue is the operator's, so a run cannot call it."
    )]
    pub async fn update_schedule(
        &self,
        Parameters(request): Parameters<UpdateScheduleRequest>,
    ) -> Result<Json<ScheduleView>, ToolError> {
        self.scope.authorize(Tool::UpdateSchedule, None)?;
        let local = self.local()?;
        let updated =
            schedule::update(&local.machine, &request.schedule_id, request.config.into()).await?;
        Ok(Json(updated.into()))
    }

    #[tool(
        description = "Turn a schedule on or off without deleting what it is set to. Call this \
when the user wants to skip the automatic runs for a while — it is the reversible answer, and \
`delete_schedule` is not. Turning one back on re-arms it from that moment, so a schedule that \
spent a month off does not immediately fire for the last of thirty nights it missed. A run cannot \
call it."
    )]
    pub async fn set_schedule_enabled(
        &self,
        Parameters(request): Parameters<SetScheduleEnabledRequest>,
    ) -> Result<Json<ScheduleView>, ToolError> {
        self.scope.authorize(Tool::SetScheduleEnabled, None)?;
        let local = self.local()?;
        let updated =
            schedule::set_enabled(&local.machine, &request.schedule_id, request.enabled).await?;
        Ok(Json(updated.into()))
    }

    #[tool(
        description = "Delete a schedule outright. Call it only when the user wants the schedule \
gone rather than paused — `set_schedule_enabled` is the reversible option and is almost always \
what they mean. A window this schedule already opened keeps running; deleting the instruction is \
not the same act as stopping tonight. A run cannot call it."
    )]
    pub async fn delete_schedule(
        &self,
        Parameters(request): Parameters<ScheduleRequest>,
    ) -> Result<Json<ScheduleDeletedView>, ToolError> {
        self.scope.authorize(Tool::DeleteSchedule, None)?;
        let local = self.local()?;
        schedule::delete(&local.machine, &request.schedule_id).await?;
        Ok(Json(ScheduleDeletedView {
            schedule_id: request.schedule_id,
            deleted: true,
        }))
    }

    #[tool(
        description = "Preview what a schedule would do: which tasks it will run, in what order, \
and which it will pass over and why. Call this in the evening, before the user leaves — it is the \
answer to \"what will happen tonight\", and it is computed from the board as it is right now, so \
it changes when a card is dragged. A task listed with a `skipped_because` will still be sitting \
there in the morning unless somebody acts. A run cannot call it."
    )]
    pub async fn preview_schedule_preflight(
        &self,
        Parameters(request): Parameters<ScheduleRequest>,
    ) -> Result<Json<PreflightView>, ToolError> {
        self.scope.authorize(Tool::PreviewSchedulePreflight, None)?;
        let local = self.local()?;
        Ok(Json(
            schedule::preview(&local.machine, &self.ctx, &request.schedule_id)
                .await?
                .into(),
        ))
    }

    #[tool(
        description = "List every IANA timezone name a schedule may be configured with. Call it \
before `create_schedule` or `update_schedule` to get the exact spelling — a schedule needs a real \
zone name such as \"Europe/Copenhagen\", never an offset or an abbreviation, because a nightly \
queue configured with one of those runs an hour out for half the year and nothing says so."
    )]
    pub async fn list_timezones(&self) -> Result<Json<TimezoneListView>, ToolError> {
        self.scope.authorize(Tool::ListTimezones, None)?;
        Ok(Json(TimezoneListView {
            timezones: schedule::timezones(),
        }))
    }
}

impl RimaiaServer {
    /// Every tool this server can register: the board router and the local
    /// router combined. What a server with a machine serves, and what the
    /// anti-drift test `every_registered_tool_has_a_run_scope_decision`
    /// iterates, so a tool added to either block still needs a decision.
    pub fn tool_router() -> ToolRouter<Self> {
        Self::board_router() + Self::local_router()
    }

    /// What this server serves: both routers given a machine, and the board
    /// router alone without one, where calling a local tool is an unknown
    /// tool.
    fn router(&self) -> ToolRouter<Self> {
        match self.local {
            Some(_) => Self::tool_router(),
            None => Self::board_router(),
        }
    }

    /// This machine, for a local handler. Only the local router calls it, and
    /// that router is registered only when there is a machine, so `Internal`
    /// here is a wiring mistake.
    fn local(&self) -> Result<&LocalTools> {
        self.local.as_ref().ok_or_else(|| {
            crate::Error::internal("a local tool was reached on a server with no machine")
        })
    }

    /// This machine, for the five board tools whose core function reacts on
    /// it when there is one.
    fn machine(&self) -> Option<&MachineContext> {
        self.local.as_ref().map(|local| &local.machine)
    }

    /// The uniform answer: whatever a tool touched, read back in full.
    async fn task_view(&self, task_id: &str) -> Result<Json<TaskView>, ToolError> {
        let detail = tasks::get_task(&self.ctx, task_id).await?;
        Ok(Json(TaskView::from(detail)))
    }

    /// The card currently at the bottom of `column`, excluding the task being
    /// moved, or `None` when the destination holds nothing else.
    ///
    /// This is the adapter ergonomic seam-contract D16 allows, and its whole
    /// extent. `tasks::move_task` still refuses "no neighbour named" unless
    /// the destination column is empty; this names the neighbour a caller who
    /// said "just put it at the back" meant. Excluding the task itself matters:
    /// a task already alone in the destination would otherwise be handed its
    /// own id and refused with "a task cannot be moved next to itself".
    ///
    /// The read is outside `move_task`'s transaction, so a neighbour could in
    /// principle vanish between the two. Bounded and benign: the result is a
    /// plain retryable refusal naming the id, never a corrupted order — the
    /// position arithmetic itself is still one transaction.
    async fn bottom_of_column(
        &self,
        task_id: &str,
        column: BoardColumn,
    ) -> Result<Option<String>, ToolError> {
        let task = tasks::get_task(&self.ctx, task_id).await?;
        // The column is the task's own team's: the move names an entity, so
        // it needs no sole team even under a context that reaches several.
        let team_id = tasks::service::team_of(&self.ctx, task_id).await?;
        let column_tasks = tasks::list_tasks(
            &self.ctx.with_scope(TeamScope::one(team_id)),
            TaskFilter {
                repository_id: Some(task.task.repository_id.clone()),
                column: Some(column),
                run_state: None,
                // The bottom of a *visible* column. An archived card still
                // carries a position, and landing a move below one would put
                // the new card off the end of the board it can see.
                ..TaskFilter::default()
            },
        )
        .await?;

        Ok(column_tasks
            .into_iter()
            .map(|summary| summary.task.id)
            .rfind(|id| id != task_id))
    }
}

/// One field of an [`UpdateTaskRequest`] as the service's own patch type.
///
/// The asymmetry D16 argues for, in three lines: a value sets, a name in
/// `clear` erases, and everything else — including the absence of both — leaves
/// the column exactly as it is.
fn patch_field(value: Option<String>, cleared: bool) -> Patch<String> {
    match (value, cleared) {
        (Some(value), _) => Patch::Set(value),
        (None, true) => Patch::Clear,
        (None, false) => Patch::Unset,
    }
}

impl RimaiaServer {
    /// The run id a review or fix handle was minted for, which is what its
    /// writes are recorded under. Called after `authorize`, which has already
    /// refused every door that has none, so a `None` here is a wiring mistake.
    fn grant_run_id(&self, tool: Tool) -> Result<&str> {
        self.scope.run_id().ok_or_else(|| {
            crate::Error::internal(format!(
                "{tool} was authorized on a handle with no run to record it under",
                tool = tool.as_str(),
            ))
        })
    }
}

#[tool_handler(router = self.router())]
impl ServerHandler for RimaiaServer {
    /// Written out rather than left to the macro's `name`/`version` arguments,
    /// which take string literals only — this way the version is
    /// `CARGO_PKG_VERSION` and cannot drift from the crate's.
    ///
    /// The name follows the door (seam-contract D30 point 1): `rimaia` on the
    /// operator's `/mcp`, `rimaia-run` on a run's own handle.
    fn get_info(&self) -> ServerInfo {
        let name = match self.scope {
            RunScope::Operator => crate::mcp::MCP_SERVER_NAME,
            RunScope::Run { .. } => crate::mcp::RUN_MCP_SERVER_NAME,
        };
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(name, env!("CARGO_PKG_VERSION")))
            .with_instructions(SERVER_INSTRUCTIONS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Every tool ADR-0006's table names, and nothing else. The table pairs
    /// add/remove link on one row; these are eleven distinct tools.
    ///
    /// **Ten until task 020.** The eleventh is `set_task_strategy`, added by
    /// ADR-0006's 2026-08-28 amendment, which also restates that the table is
    /// otherwise still closed — `delete_task` and every run operation remain
    /// deliberately absent. Anyone reaching this constant because a build went
    /// red should be reading that amendment, not widening the array: the count
    /// moved once, on purpose, and this comment is how the next reader tells a
    /// deliberate eleventh from a drifted one.
    /// Every tool the server registers, as ADR-0021 leaves it: not a fixed
    /// count anyone asserts, but a set that must agree with the scope table.
    ///
    /// ADR-0006's original ten were the v1 planning surface and are still
    /// correct as that; they stopped being the boundary when ADR-0021 made
    /// capability parity a rule. What replaces a count is the property that
    /// actually matters — a registered tool with no run-scope decision cannot
    /// reach the wire.
    const REGISTERED_TOOLS: [&str; 64] = [
        "accept_task_strategy",
        "add_task_link",
        "approve_task",
        "archive_task",
        "archive_tasks",
        "clear_task_strategy",
        "create_schedule",
        "create_task",
        "delete_schedule",
        "dismiss_doctor_warning",
        "dismiss_onboarding",
        "get_analytics",
        "get_base_instructions",
        "get_repository_credential_status",
        "get_review_digest",
        "get_review_history",
        "get_review_level",
        "get_review_settings",
        "get_run_capacity",
        "get_strategy_approval",
        "get_strategy_catalogue",
        "get_strategy_defaults",
        "get_subscription_cost",
        "get_task",
        "get_task_dependents",
        "get_worktree_auto_cleanup",
        "give_up_on_task",
        "list_checkouts",
        "list_repositories",
        "list_review_findings",
        "list_schedules",
        "list_tasks",
        "list_timezones",
        "list_worktrees",
        "mark_review_digest_seen",
        "move_task",
        "plan_task_strategy",
        "plan_tasks_strategy",
        "preview_schedule_preflight",
        "record_review_findings",
        "reject_task",
        "remove_task_link",
        "request_task_changes",
        "resolve_review_finding",
        "restore_doctor_warning",
        "run_doctor",
        "set_max_concurrency",
        "set_repository_max_concurrency",
        "set_repository_on_archive",
        "set_repository_review_config",
        "set_review_settings",
        "set_schedule_enabled",
        "set_schedule_mode",
        "set_strategy_approval",
        "set_strategy_catalogue",
        "set_strategy_defaults",
        "set_subscription_cost",
        "set_task_dependencies",
        "set_task_review",
        "set_task_strategy",
        "set_worktree_auto_cleanup",
        "unarchive_task",
        "update_schedule",
        "update_task",
    ];

    fn tool_names() -> Vec<String> {
        let mut names: Vec<String> = RimaiaServer::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn every_tool_adr_0006_names_is_registered() {
        assert_eq!(tool_names(), REGISTERED_TOOLS);
    }

    #[test]
    fn every_tool_description_says_when_to_call_it() {
        // A crude check for a prose requirement, and the only mechanical one
        // available: ADR-0006 asks that descriptions say *when* to call a tool,
        // and every one of ours says so with the words "Call this" or "Call
        // it". It cannot judge whether the sentence is any good — that is a
        // reviewer's job — but it does catch a tool added later with a bare
        // "Creates a task."
        for tool in RimaiaServer::tool_router().list_all() {
            let description = tool
                .description
                .as_deref()
                .unwrap_or_else(|| panic!("{} has no description at all", tool.name));
            assert!(
                description.contains("Call this") || description.contains("Call it"),
                "{}'s description never says when to call it: {description}",
                tool.name,
            );
        }
    }

    #[test]
    fn create_task_requires_only_a_repository_and_a_title() {
        let required = required_properties("create_task");

        assert_eq!(required, vec!["repository_id", "title"]);
    }

    #[test]
    fn update_task_requires_nothing_but_the_task_id() {
        assert_eq!(required_properties("update_task"), vec!["task_id"]);
    }

    #[test]
    fn every_tool_output_schema_is_an_object() {
        // MCP requires it, and Claude Code refuses the *entire* `tools/list`
        // response when one tool disagrees — dropping every other tool with
        // it, which is a far worse failure than the one tool being wrong. A
        // list-returning tool therefore wraps its array in an object; see
        // `responses::RepositoryListView`. Found by the CLI, not by a test,
        // which is why there is now a test.
        for tool in RimaiaServer::tool_router().list_all() {
            let schema = tool
                .output_schema
                .as_ref()
                .unwrap_or_else(|| panic!("{} advertises no output schema", tool.name));
            let schema = serde_json::to_value(schema).expect("a schema serializes");
            assert_eq!(
                schema.get("type").and_then(|value| value.as_str()),
                Some("object"),
                "{}'s output schema is not an object: {schema}",
                tool.name,
            );
        }
    }

    #[test]
    fn every_tool_input_property_is_snake_case() {
        // Seam-contract D16. The row types serialize camelCase for the
        // frontend, so a DTO built by re-serializing one would fail here.
        for tool in RimaiaServer::tool_router().list_all() {
            let schema = serde_json::to_value(&tool.input_schema).expect("a schema serializes");
            let Some(properties) = schema.get("properties").and_then(|value| value.as_object())
            else {
                continue;
            };
            for name in properties.keys() {
                assert!(
                    name.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                    "{}'s `{name}` is not snake_case",
                    tool.name,
                );
            }
        }
    }

    /// The `required` list of one tool's input schema, sorted.
    fn required_properties(tool_name: &str) -> Vec<String> {
        let tool = RimaiaServer::tool_router()
            .list_all()
            .into_iter()
            .find(|tool| tool.name == tool_name)
            .unwrap_or_else(|| panic!("{tool_name} is registered"));

        let schema = serde_json::to_value(&tool.input_schema).expect("a schema serializes");
        let mut required: Vec<String> = schema
            .get("required")
            .and_then(|value| value.as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        required.sort();
        required
    }

    #[tokio::test]
    async fn the_server_introduces_itself_as_rimaia_with_instructions() {
        let harness = crate::testing::TestContext::new().await;
        let info = RimaiaServer::new(
            harness.context.clone(),
            Arc::new(crate::runner::provider::ClaudeProvider),
            Some(crate::testing::doctor::local_tools(harness.machine())),
        )
        .get_info();

        assert_eq!(info.server_info.name, "rimaia");
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
        assert!(info
            .instructions
            .as_deref()
            .expect("instructions")
            .contains("unattended"));
    }
}
