//! Task CRUD, board ordering, run-state transitions and link commands (task
//! 004, ADR-0003, ADR-0007, ADR-0018).
//!
//! Every rule — the empty-plan guard, the dependents-block-delete check, the
//! run-state transition table, the fractional placement math — lives in
//! `crate::tasks`. This file only reshapes wire args into that module's input
//! types and calls it, per [`api::board`](super)'s own module doc: a rule
//! enforced here and not also on the MCP path (task 010) is a bug (ADR-0006).

use serde::{Deserialize, Deserializer};

use super::{ById, ByLinkId, ByTaskId};
use crate::api::BoardRequest;
use crate::db::{BoardColumn, RunState, StrategyMode, Task, TaskLink};
use crate::tasks::{
    self, ArchiveFilter, ArchiveReport, ArchivedTask, NewTask, NewTaskLink, Patch, TaskDetail,
    TaskFilter, TaskLinkPatch, TaskPatch, TaskSummary,
};
use crate::Result;

/// What the frontend sends [`add_task_link`] and includes in
/// [`NewTaskInput::links`]. Mirrors [`NewTaskLink`] — a `Deserialize` shape
/// for the same reason [`NewTaskInput`] is one for [`NewTask`].
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTaskLinkInput {
    pub label: String,
    pub url: String,
}

impl From<NewTaskLinkInput> for NewTaskLink {
    fn from(input: NewTaskLinkInput) -> Self {
        NewTaskLink {
            label: input.label,
            url: input.url,
        }
    }
}

/// What the frontend sends [`create_task`]. Mirrors [`NewTask`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTaskInput {
    pub repository_id: String,
    pub title: String,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub extra_instructions: Option<String>,
    #[serde(default)]
    pub column: Option<BoardColumn>,
    #[serde(default)]
    pub links: Vec<NewTaskLinkInput>,
}

/// What the frontend sends [`list_tasks`]. Mirrors [`TaskFilter`] — every
/// field left `None` matches everything.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskFilterInput {
    #[serde(default)]
    pub repository_id: Option<String>,
    #[serde(default)]
    pub column: Option<BoardColumn>,
    #[serde(default)]
    pub run_state: Option<RunState>,
    /// Which side of ADR-0025's archive line to read. Absent means the board,
    /// which is what makes every existing caller keep working (seam-contract
    /// D26.1).
    #[serde(default)]
    pub archived: ArchiveFilter,
}

/// Deserializes a field that must distinguish "not provided" from "provided
/// and explicitly cleared" — the wire equivalent of [`Patch`], which plain
/// `Option<T>` cannot represent because both collapse onto `None`.
///
/// Paired with `#[serde(default)]` on the field: a JSON key that is absent
/// never calls this at all, which is what leaves that `default` (`None`, i.e.
/// [`Patch::Unset`]) in place. A key present as `null` deserializes the inner
/// `Option<T>` to `None`, wrapped here as `Some(None)` ([`Patch::Clear`]). A
/// key present with a value is `Some(Some(value))` ([`Patch::Set`]).
fn opt_patch<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

fn to_patch<T>(field: Option<Option<T>>) -> Patch<T> {
    match field {
        None => Patch::Unset,
        Some(None) => Patch::Clear,
        Some(Some(value)) => Patch::Set(value),
    }
}

/// What the frontend sends [`update_task`]. Mirrors [`TaskPatch`] —
/// `repository_id` and `title` are plain `Option`s (never "clear", both
/// columns are `NOT NULL`); the nullable fields go through [`opt_patch`] so
/// the frontend can distinguish leaving a field alone from clearing it.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskPatchInput {
    /// Seam-contract D13. Whether a reassignment is allowed is
    /// `rimaia_core::tasks::update_task`'s to decide, not this adapter's —
    /// the panel disables the selector as a courtesy, and task 010 sends the
    /// same field over MCP.
    #[serde(default)]
    pub repository_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// Task 020's mode, which gets no command of its own: the column is
    /// `NOT NULL` and has no "clear", so a plain `Option` says everything, and
    /// sending it here means the panel and the `update_task` MCP tool reach
    /// D17.6's rule — a model or effort set flips the mode to `manual` — through
    /// the same service (ADR-0006).
    #[serde(default)]
    pub strategy_mode: Option<StrategyMode>,
    #[serde(default, deserialize_with = "opt_patch")]
    pub plan: Option<Option<String>>,
    #[serde(default, deserialize_with = "opt_patch")]
    pub extra_instructions: Option<Option<String>>,
    #[serde(default, deserialize_with = "opt_patch")]
    pub model: Option<Option<String>>,
    #[serde(default, deserialize_with = "opt_patch")]
    pub effort: Option<Option<String>>,
}

/// What the frontend sends [`update_task_link`]. Mirrors [`TaskLinkPatch`] —
/// `label` and `url` are both `NOT NULL`, so plain `Option` is enough; there
/// is no "clear" for either.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskLinkPatchInput {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTask {
    pub input: NewTaskInput,
}

/// Creates a task at the bottom of its target column.
pub async fn create_task(request: BoardRequest, args: CreateTask) -> Result<Task> {
    let input = args.input;
    tasks::create_task(
        &request.ctx,
        NewTask {
            repository_id: input.repository_id,
            title: input.title,
            plan: input.plan,
            extra_instructions: input.extra_instructions,
            column: input.column,
            links: input.links.into_iter().map(Into::into).collect(),
        },
    )
    .await
}

/// The full detail read: the task, its links, what it depends on, and its
/// most recent run.
pub async fn get_task(request: BoardRequest, args: ById) -> Result<TaskDetail> {
    tasks::get_task(&request.ctx, &args.id).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTasks {
    pub filter: TaskFilterInput,
}

/// The board's bulk read: every task matching `filter`, ordered the way the
/// board reads a column, as the [`TaskSummary`] projection seam-contract D12
/// fixes — the row plus the counts and last-run fields a card draws, so a
/// fifty-card board is one query and not fifty [`get_task`] calls.
pub async fn list_tasks(request: BoardRequest, args: ListTasks) -> Result<Vec<TaskSummary>> {
    let filter = args.filter;
    tasks::list_tasks(
        &request.ctx,
        TaskFilter {
            repository_id: filter.repository_id,
            column: filter.column,
            run_state: filter.run_state,
            archived: filter.archived,
        },
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTask {
    pub id: String,
    pub patch: TaskPatchInput,
}

/// Patch semantics: only fields present in `patch` change.
pub async fn update_task(request: BoardRequest, args: UpdateTask) -> Result<Task> {
    let patch = args.patch;
    tasks::update_task(
        &request.ctx,
        &args.id,
        TaskPatch {
            repository_id: patch.repository_id,
            title: patch.title,
            strategy_mode: patch.strategy_mode,
            plan: to_patch(patch.plan),
            extra_instructions: to_patch(patch.extra_instructions),
            model: to_patch(patch.model),
            effort: to_patch(patch.effort),
        },
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignTask {
    pub task_id: String,
    pub assignee_id: Option<String>,
}

/// Assigns a task to one member of its team, or returns it to the pool with
/// `None` (ADR-0032 point 1).
pub async fn assign_task(request: BoardRequest, args: AssignTask) -> Result<Task> {
    tasks::assign_task(&request.ctx, &args.task_id, args.assignee_id.as_deref()).await
}

/// Deletes a task. Refused, naming what still depends on it, when another
/// task does.
pub async fn delete_task(request: BoardRequest, args: ById) -> Result<()> {
    tasks::delete_task(&request.ctx, &args.id).await
}

/// Takes one task off the board, keeping its runs, its links and its edges
/// (ADR-0025) — and, given this machine, runs whatever cleanup its repository
/// is configured for (task 046 point 3a).
///
/// Refused for a `running` or `waiting_retry` task, with no force to pass.
pub async fn archive_task(request: BoardRequest, args: ById) -> Result<ArchivedTask> {
    tasks::archive_task(&request.ctx, request.machine.as_ref(), &args.id).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveTasks {
    pub ids: Vec<String>,
}

/// Archives the board's hand-picked set, reporting refusals instead of
/// aborting on the first one (seam-contract D20 point 2, D26.3).
pub async fn archive_tasks(request: BoardRequest, args: ArchiveTasks) -> Result<ArchiveReport> {
    tasks::archive_tasks(&request.ctx, request.machine.as_ref(), &args.ids).await
}

/// Puts an archived task back in the column it was in. Nothing the cleanup
/// deleted comes back.
pub async fn unarchive_task(request: BoardRequest, args: ById) -> Result<Task> {
    tasks::unarchive_task(&request.ctx, &args.id).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveTask {
    pub id: String,
    pub column: BoardColumn,
    pub before_id: Option<String>,
    pub after_id: Option<String>,
}

/// Moves a task to `column`, landing it between `before_id` and `after_id` —
/// see [`tasks::move_task`]'s own doc for the neighbour contract and the
/// empty-plan guard on `ready`. Given this machine, entering `done` runs D20.3's
/// auto-removal in the same call (task 046 point 3a).
pub async fn move_task(request: BoardRequest, args: MoveTask) -> Result<Task> {
    tasks::move_task(
        &request.ctx,
        request.machine.as_ref(),
        &args.id,
        args.column,
        args.before_id.as_deref(),
        args.after_id.as_deref(),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTaskRunState {
    pub id: String,
    pub run_state: RunState,
}

/// The only path to a task's `run_state`. Validates the transition against
/// ADR-0007's table and refuses an illegal one.
pub async fn set_task_run_state(request: BoardRequest, args: SetTaskRunState) -> Result<Task> {
    tasks::set_run_state(&request.ctx, &args.id, args.run_state).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddTaskLink {
    pub task_id: String,
    pub input: NewTaskLinkInput,
}

/// Appends a link to the bottom of a task's link list.
pub async fn add_task_link(request: BoardRequest, args: AddTaskLink) -> Result<TaskLink> {
    tasks::add_task_link(&request.ctx, &args.task_id, args.input.into()).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTaskLink {
    pub link_id: String,
    pub patch: TaskLinkPatchInput,
}

/// Patch semantics on one link.
pub async fn update_task_link(request: BoardRequest, args: UpdateTaskLink) -> Result<TaskLink> {
    tasks::update_task_link(
        &request.ctx,
        &args.link_id,
        TaskLinkPatch {
            label: args.patch.label,
            url: args.patch.url,
        },
    )
    .await
}

pub async fn remove_task_link(request: BoardRequest, args: ByLinkId) -> Result<()> {
    tasks::remove_task_link(&request.ctx, &args.link_id).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReorderTaskLink {
    pub link_id: String,
    pub before_id: Option<String>,
    pub after_id: Option<String>,
}

/// Reorders a link among its task's other links — the same neighbour
/// contract [`move_task`] uses for cards.
pub async fn reorder_task_link(request: BoardRequest, args: ReorderTaskLink) -> Result<TaskLink> {
    tasks::reorder_task_link(
        &request.ctx,
        &args.link_id,
        args.before_id.as_deref(),
        args.after_id.as_deref(),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTaskDependencies {
    pub task_id: String,
    pub depends_on: Vec<String>,
}

/// Replaces the whole set of tasks `task_id` is blocked by (ADR-0008).
///
/// **Replace, never merge** — an empty list clears every dependency, which is
/// how the panel removes one edge without a second command for it. Cycle
/// detection, the self-edge refusal and the cross-repository refusal all live in
/// `tasks::set_task_dependencies`, which task 010 shipped for the MCP door
/// (seam-contract D16.4); this is the second door onto the same function, so the
/// board cannot enforce a different graph from an agent (ADR-0006).
pub async fn set_task_dependencies(
    request: BoardRequest,
    args: SetTaskDependencies,
) -> Result<Vec<String>> {
    tasks::set_task_dependencies(&request.ctx, &args.task_id, &args.depends_on).await
}

/// The dependencies keeping a task out of the queue, as whole rows — ADR-0008's
/// "the card shows which task is blocking it", at panel width.
///
/// The board's own read already carries the flag and the first blocker's title
/// on every card (seam-contract D12); this is for the one task the panel is
/// open on, where the user needs the whole list and each one's column.
pub async fn get_blocking_reason(request: BoardRequest, args: ByTaskId) -> Result<Vec<Task>> {
    tasks::blocking_reason(&request.ctx, &args.task_id).await
}
