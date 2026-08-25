//! Task CRUD, board ordering, run-state transitions and link commands (task
//! 004, ADR-0003, ADR-0007, ADR-0018).
//!
//! Every rule — the empty-plan guard, the dependents-block-delete check, the
//! run-state transition table, the fractional placement math — lives in
//! `rimaia_core::tasks`. This file only reshapes wire args into that module's
//! input types and calls it, per this crate's own module doc: a rule enforced
//! here and not also on the MCP path (task 010) is a bug (ADR-0006).

use rimaia_core::db::{BoardColumn, RunState, Task, TaskLink};
use rimaia_core::tasks::{
    self, NewTask, NewTaskLink, Patch, TaskDetail, TaskFilter, TaskLinkPatch, TaskPatch,
    TaskSummary,
};
use rimaia_core::Result;
use serde::{Deserialize, Deserializer};
use tauri::State;

use crate::state::AppState;

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

/// Creates a task at the bottom of its target column.
#[tauri::command]
pub async fn create_task(state: State<'_, AppState>, input: NewTaskInput) -> Result<Task> {
    tasks::create_task(
        &state.context,
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
#[tauri::command]
pub async fn get_task(state: State<'_, AppState>, id: String) -> Result<TaskDetail> {
    tasks::get_task(&state.context, &id).await
}

/// The board's bulk read: every task matching `filter`, ordered the way the
/// board reads a column, as the [`TaskSummary`] projection seam-contract D12
/// fixes — the row plus the counts and last-run fields a card draws, so a
/// fifty-card board is one query and not fifty [`get_task`] calls.
#[tauri::command]
pub async fn list_tasks(
    state: State<'_, AppState>,
    filter: TaskFilterInput,
) -> Result<Vec<TaskSummary>> {
    tasks::list_tasks(
        &state.context,
        TaskFilter {
            repository_id: filter.repository_id,
            column: filter.column,
            run_state: filter.run_state,
        },
    )
    .await
}

/// Patch semantics: only fields present in `patch` change.
#[tauri::command]
pub async fn update_task(
    state: State<'_, AppState>,
    id: String,
    patch: TaskPatchInput,
) -> Result<Task> {
    tasks::update_task(
        &state.context,
        &id,
        TaskPatch {
            repository_id: patch.repository_id,
            title: patch.title,
            plan: to_patch(patch.plan),
            extra_instructions: to_patch(patch.extra_instructions),
            model: to_patch(patch.model),
            effort: to_patch(patch.effort),
        },
    )
    .await
}

/// Deletes a task. Refused, naming what still depends on it, when another
/// task does.
#[tauri::command]
pub async fn delete_task(state: State<'_, AppState>, id: String) -> Result<()> {
    tasks::delete_task(&state.context, &id).await
}

/// Moves a task to `column`, landing it between `before_id` and `after_id` —
/// see [`tasks::move_task`]'s own doc for the neighbour contract and the
/// empty-plan guard on `ready`.
#[tauri::command]
pub async fn move_task(
    state: State<'_, AppState>,
    id: String,
    column: BoardColumn,
    before_id: Option<String>,
    after_id: Option<String>,
) -> Result<Task> {
    tasks::move_task(
        &state.context,
        &id,
        column,
        before_id.as_deref(),
        after_id.as_deref(),
    )
    .await
}

/// The only path to a task's `run_state`. Validates the transition against
/// ADR-0007's table and refuses an illegal one.
#[tauri::command]
pub async fn set_task_run_state(
    state: State<'_, AppState>,
    id: String,
    run_state: RunState,
) -> Result<Task> {
    tasks::set_run_state(&state.context, &id, run_state).await
}

/// Appends a link to the bottom of a task's link list.
#[tauri::command]
pub async fn add_task_link(
    state: State<'_, AppState>,
    task_id: String,
    input: NewTaskLinkInput,
) -> Result<TaskLink> {
    tasks::add_task_link(&state.context, &task_id, input.into()).await
}

/// Patch semantics on one link.
#[tauri::command]
pub async fn update_task_link(
    state: State<'_, AppState>,
    link_id: String,
    patch: TaskLinkPatchInput,
) -> Result<TaskLink> {
    tasks::update_task_link(
        &state.context,
        &link_id,
        TaskLinkPatch {
            label: patch.label,
            url: patch.url,
        },
    )
    .await
}

#[tauri::command]
pub async fn remove_task_link(state: State<'_, AppState>, link_id: String) -> Result<()> {
    tasks::remove_task_link(&state.context, &link_id).await
}

/// Reorders a link among its task's other links — the same neighbour
/// contract [`move_task`] uses for cards.
#[tauri::command]
pub async fn reorder_task_link(
    state: State<'_, AppState>,
    link_id: String,
    before_id: Option<String>,
    after_id: Option<String>,
) -> Result<TaskLink> {
    tasks::reorder_task_link(
        &state.context,
        &link_id,
        before_id.as_deref(),
        after_id.as_deref(),
    )
    .await
}
