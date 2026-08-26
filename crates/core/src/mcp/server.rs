//! The ten tools (ADR-0006), and nothing else.
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

use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};

use crate::context::ServiceContext;
use crate::db::BoardColumn;
use crate::mcp::error::ToolError;
use crate::mcp::requests::{
    AddTaskLinkRequest, ClearableField, CreateTaskRequest, GetTaskRequest, ListTasksRequest,
    MoveTaskRequest, RemoveTaskLinkRequest, SetTaskDependenciesRequest, UpdateTaskRequest,
};
use crate::mcp::responses::{BaseInstructionsView, RepositoryView, TaskListItem, TaskView};
use crate::runner::prompt::TEMPLATE_VARIABLES;
use crate::tasks::{NewTask, NewTaskLink, Patch, TaskFilter, TaskPatch};
use crate::{db, repo, tasks, Result};

/// What Claude Code is told this server is for, before it has read a single
/// tool description.
const SERVER_INSTRUCTIONS: &str = "\
Rimaia is a desktop app on this machine that queues implementation plans and runs them later, \
unattended, with Claude Code — each in its own git worktree, producing a branch and a pull \
request for the user to review in the morning. Use these tools to hand a finished plan over to \
Rimaia instead of implementing it in this session. You are writing for a future agent that will \
have the plan and nothing else: no memory of this conversation, and nobody to ask. Anything the \
implementation depends on must be in the plan.";

/// The tool handler. One field, because a tool handler has no state of its own
/// — everything it can do, it does through the same services the Tauri
/// commands call.
///
/// Cheap to clone, like [`ServiceContext`] itself: the streamable-HTTP
/// transport builds one per session.
#[derive(Clone)]
pub struct RimaiaServer {
    ctx: ServiceContext,
}

#[tool_router]
impl RimaiaServer {
    /// Takes the context already re-sourced by `mcp::build`, so nothing here
    /// has to remember that its writes are `mcp` (ADR-0019).
    pub fn new(ctx: ServiceContext) -> Self {
        Self { ctx }
    }

    #[tool(
        description = "List the git repositories registered with Rimaia, with the id each one is \
known by. Call this before creating a task: every task belongs to exactly one repository, and \
`create_task` needs its `repository_id`, which is a UUID you cannot derive from the repository's \
name or path. Also call it when the user names a project you have not seen an id for in this \
session."
    )]
    pub async fn list_repositories(&self) -> Result<Json<Vec<RepositoryView>>, ToolError> {
        let repositories = repo::list(&self.ctx).await?;
        Ok(Json(
            repositories.into_iter().map(RepositoryView::from).collect(),
        ))
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
    ) -> Result<Json<Vec<TaskListItem>>, ToolError> {
        let summaries = tasks::list_tasks(
            &self.ctx,
            TaskFilter {
                repository_id: request.repository_id,
                column: request.column,
                run_state: request.run_state,
            },
        )
        .await?;

        Ok(Json(
            summaries.into_iter().map(TaskListItem::from).collect(),
        ))
    }

    #[tool(
        description = "Change an existing task's title, plan, extra instructions, model or effort. \
Call this to amend a plan you have already handed over — read it with `get_task` first and send \
the full replacement text, because `plan` is replaced wholesale and is not appended to. Fields \
you do not mention keep their current value. Use `clear` to erase `extra_instructions`, `model` \
or `effort`; a plan cannot be erased over MCP. This tool does not move a task between columns — \
that is `move_task` — and does not change what it depends on — that is `set_task_dependencies`."
    )]
    pub async fn update_task(
        &self,
        Parameters(request): Parameters<UpdateTaskRequest>,
    ) -> Result<Json<TaskView>, ToolError> {
        // Before the service call, because the request contradicts itself and
        // there is no patch to build from it.
        request.ensure_no_conflicting_clear()?;

        let cleared = |field: ClearableField| request.clear.contains(&field);
        let patch = TaskPatch {
            repository_id: request.repository_id.clone(),
            title: request.title.clone(),
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
        // Read first: the answer is the whole task, and after the delete there
        // is no row left to say which task that was.
        let link = tasks::get_task_link(&self.ctx, &request.link_id).await?;
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
        tasks::set_task_dependencies(&self.ctx, &request.task_id, &request.depends_on).await?;

        self.task_view(&request.task_id).await
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
            .filter(|id| id != task_id)
            .next_back())
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
            .with_server_info(Implementation::new("rimaia", env!("CARGO_PKG_VERSION")))
            .with_instructions(SERVER_INSTRUCTIONS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Every tool ADR-0006's table names, and nothing else. The table pairs
    /// add/remove link on one row; these are ten distinct tools.
    const ADR_0006_TOOLS: [&str; 10] = [
        "add_task_link",
        "create_task",
        "get_base_instructions",
        "get_task",
        "list_repositories",
        "list_tasks",
        "move_task",
        "remove_task_link",
        "set_task_dependencies",
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
        assert_eq!(tool_names(), ADR_0006_TOOLS);
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
