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

use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};

use crate::context::ServiceContext;
use crate::db::{BoardColumn, StrategySource};
use crate::mcp::error::ToolError;
use crate::mcp::requests::{
    AddTaskLinkRequest, ClearableField, CreateTaskRequest, GetStrategyDefaultsRequest,
    GetTaskRequest, ListTasksRequest, MoveTaskRequest, RemoveTaskLinkRequest,
    SetStrategyApprovalRequest, SetStrategyCatalogueRequest, SetStrategyDefaultsRequest,
    SetTaskDependenciesRequest, SetTaskStrategyRequest, SetWorktreeAutoCleanupRequest,
    TaskStrategyRequest, UpdateTaskRequest,
};
use crate::mcp::responses::{
    BaseInstructionsView, RepositoryListView, RepositoryView, StrategyApprovalView, TaskListItem,
    TaskListView, TaskView, WorktreeAutoCleanupView, WorktreeListView, WorktreeView,
};
use crate::mcp::scope::{RunScope, Tool};
use crate::runner::prompt::TEMPLATE_VARIABLES;
use crate::strategy::{self, Catalogue, StrategyDefaults};
use crate::tasks::{NewTask, NewTaskLink, Patch, TaskFilter, TaskPatch};
use crate::{db, repo, tasks, worktree, Result};

/// What Claude Code is told this server is for, before it has read a single
/// tool description.
const SERVER_INSTRUCTIONS: &str = "\
Rimaia is a desktop app on this machine that queues implementation plans and runs them later, \
unattended, with Claude Code — each in its own git worktree, producing a branch and a pull \
request for the user to review in the morning. Use these tools to hand a finished plan over to \
Rimaia instead of implementing it in this session. You are writing for a future agent that will \
have the plan and nothing else: no memory of this conversation, and nobody to ask. Anything the \
implementation depends on must be in the plan.";

/// The tool handler. Two fields: the services it calls through, and which door
/// it was reached through. It still has no state of its own — everything it can
/// do, it does through the same services the Tauri commands call.
///
/// Cheap to clone, like [`ServiceContext`] itself: the streamable-HTTP
/// transport builds one per request.
#[derive(Clone)]
pub struct RimaiaServer {
    ctx: ServiceContext,
    /// Carried on the *value*, not on the request, which is the whole argument
    /// for putting the token in the path — see [`RunScope`].
    scope: RunScope,
}

/// `vis = "pub"` so `tests/mcp_scope.rs`, which lives outside this crate, can
/// enumerate the registered tools and require each one to have declared a
/// run-scope decision. That anti-drift test is the only thing that makes the
/// allow table hard to forget, and it cannot be written against a private
/// router.
#[tool_router(vis = "pub")]
impl RimaiaServer {
    /// Takes the context already re-sourced by `mcp::build`, so nothing here
    /// has to remember that its writes are `mcp` (ADR-0019).
    ///
    /// [`RunScope::Operator`], because `/mcp` is what this constructor serves
    /// and task 020 takes nothing away from it. Keeping the operator surface on
    /// the unchanged constructor is also what lets every direct-call handler
    /// test in `tests/mcp_tools.rs` stay exactly as it was.
    pub fn new(ctx: ServiceContext) -> Self {
        Self {
            ctx,
            scope: RunScope::Operator,
        }
    }

    /// A server reached through `/mcp/run/{token}`: one run, one task.
    ///
    /// A second constructor rather than a parameter on [`new`](Self::new),
    /// because a scope is not something the operator path should be able to get
    /// wrong by passing the wrong argument.
    pub fn scoped(ctx: ServiceContext, task_id: impl Into<String>) -> Self {
        Self {
            ctx,
            scope: RunScope::Run {
                task_id: task_id.into(),
            },
        }
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
        let base_instructions = db::settings::base_instructions(&self.ctx.pool).await?;

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
        Ok(Json(strategy::catalogue::catalogue(&self.ctx.pool).await?))
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
        Ok(Json(strategy::catalogue::catalogue(&self.ctx.pool).await?))
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
                strategy::settings::repository_default(&self.ctx.pool, repository_id).await?
            }
            None => strategy::settings::global_default(&self.ctx.pool).await?,
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
            approval: strategy::settings::approval(&self.ctx.pool).await?,
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

    // Task 016's read surface. The three commands that *delete* a worktree
    // have no tool here, and that is not an oversight — see this module's own
    // note and seam-contract D19. What an agent can do is find out what is on
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

        let inventory = worktree::inventory(&self.ctx).await?;
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
        Ok(Json(WorktreeAutoCleanupView {
            setting: worktree::auto_cleanup(&self.ctx.pool).await?,
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
        worktree::set_auto_cleanup(&self.ctx, request.setting).await?;
        Ok(Json(WorktreeAutoCleanupView {
            setting: request.setting,
        }))
    }
}

impl RimaiaServer {
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
        let column_tasks = tasks::list_tasks(
            &self.ctx,
            TaskFilter {
                repository_id: Some(task.task.repository_id.clone()),
                column: Some(column),
                run_state: None,
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

#[tool_handler]
impl ServerHandler for RimaiaServer {
    /// Written out rather than left to the macro's `name`/`version` arguments,
    /// which take string literals only — this way the version is
    /// `CARGO_PKG_VERSION` and cannot drift from the crate's.
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                crate::mcp::MCP_SERVER_NAME,
                env!("CARGO_PKG_VERSION"),
            ))
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
    const REGISTERED_TOOLS: [&str; 22] = [
        "accept_task_strategy",
        "add_task_link",
        "clear_task_strategy",
        "create_task",
        "get_base_instructions",
        "get_strategy_approval",
        "get_strategy_catalogue",
        "get_strategy_defaults",
        "get_task",
        "get_worktree_auto_cleanup",
        "list_repositories",
        "list_tasks",
        "list_worktrees",
        "move_task",
        "remove_task_link",
        "set_strategy_approval",
        "set_strategy_catalogue",
        "set_strategy_defaults",
        "set_task_dependencies",
        "set_task_strategy",
        "set_worktree_auto_cleanup",
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
        let info = RimaiaServer::new(harness.context).get_info();

        assert_eq!(info.server_info.name, "rimaia");
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
        assert!(info
            .instructions
            .as_deref()
            .expect("instructions")
            .contains("unattended"));
    }
}
