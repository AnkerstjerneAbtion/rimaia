//! What the ten tools take off the wire (ADR-0006, seam-contract D16).
//!
//! `snake_case` in both directions, which is the convention MCP tool schemas
//! are written in everywhere else — deliberately *not* the `camelCase` the
//! Tauri boundary uses. Mixing the two inside one process is a bug generator,
//! which is why these are their own types rather than the service's input
//! shapes with a `Deserialize` bolted on.
//!
//! Every struct is `deny_unknown_fields`, so "input schemas are strict" is
//! true of the deserializer and not only of the advertised schema. rmcp turns
//! the resulting serde error into a tool-level error carrying serde's own
//! message — `unknown field \`colum\`, expected one of ...` — which is exactly
//! the "actual problem, not 'invalid input'" the task asks for, and it costs
//! nothing to get.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::db::{BoardColumn, RunState};
use crate::error::{Error, Result};

/// `create_task`: a whole plan, handed over in one call.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CreateTaskRequest {
    /// The repository the task belongs to. `list_repositories` is where the id
    /// comes from — it is a UUID, not derivable from a name or a path.
    pub repository_id: String,
    pub title: String,
    /// The whole brief the implementing agent receives, as Markdown.
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub extra_instructions: Option<String>,
    /// Omitted means `not_ready`, which is where a draft belongs: `ready` is
    /// the run queue.
    #[serde(default)]
    pub column: Option<BoardColumn>,
    #[serde(default)]
    pub links: Vec<NewLinkRequest>,
}

/// One `{label, url}` external reference — the same shape whether it arrives
/// with the task or through `add_task_link`, exactly as
/// [`NewTaskLink`](crate::tasks::NewTaskLink) is.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct NewLinkRequest {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct GetTaskRequest {
    pub task_id: String,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ListTasksRequest {
    #[serde(default)]
    pub repository_id: Option<String>,
    #[serde(default)]
    pub column: Option<BoardColumn>,
    #[serde(default)]
    pub run_state: Option<RunState>,
}

/// `update_task`: patch semantics, with erasure spelled out.
///
/// An omitted field is a no-op. Erasing one means naming it in
/// [`clear`](UpdateTaskRequest::clear) — **not** sending `null**, which is
/// seam-contract D16's decision: an LLM that fills in every property of a
/// schema sends `plan: null` and destroys four thousand words, where an
/// omitted field costs nothing. The two mistakes are not symmetric, so the
/// destructive one is made deliberate.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UpdateTaskRequest {
    pub task_id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Replaces the plan wholesale; it is never appended to. Deliberately not
    /// clearable at all — a task with no plan is a task nobody can run, and an
    /// agent has no reason to want one. That is a capability this adapter
    /// declines to expose, the way ADR-0006 already declines `delete_task`,
    /// not a rule enforced in one path only.
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub extra_instructions: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    /// Re-files the task into another repository. Refused once anything has
    /// tied it to the one it is in (seam-contract D13) — by the service, not
    /// here.
    #[serde(default)]
    pub repository_id: Option<String>,
    /// Fields to erase. Naming a field here *and* giving it a value is
    /// refused, because the two say opposite things and guessing which the
    /// caller meant is how a plan gets lost.
    #[serde(default)]
    pub clear: Vec<ClearableField>,
}

/// The fields `update_task` can erase. `plan` is not among them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ClearableField {
    ExtraInstructions,
    Model,
    Effort,
}

impl ClearableField {
    /// The name the caller wrote, for an error message that quotes it back.
    pub const fn as_str(self) -> &'static str {
        match self {
            ClearableField::ExtraInstructions => "extra_instructions",
            ClearableField::Model => "model",
            ClearableField::Effort => "effort",
        }
    }
}

impl UpdateTaskRequest {
    /// Refuses a field that is both given a value and named in `clear`.
    ///
    /// Raised before the service call, because there is no coherent patch to
    /// hand it: the request says "set this" and "erase this" about one column.
    pub fn ensure_no_conflicting_clear(&self) -> Result<()> {
        for field in &self.clear {
            let given = match field {
                ClearableField::ExtraInstructions => self.extra_instructions.is_some(),
                ClearableField::Model => self.model.is_some(),
                ClearableField::Effort => self.effort.is_some(),
            };
            if given {
                return Err(Error::invalid(format!(
                    "{name} was given both a value and a place in `clear` — send one or the other",
                    name = field.as_str(),
                )));
            }
        }
        Ok(())
    }
}

/// `move_task`: a column, and optionally where in it.
///
/// Naming neither neighbour sends the task to the bottom of the destination
/// column, which is the back of the queue. The service itself refuses that
/// spelling unless the column is empty — the adapter synthesises the bottom
/// neighbour rather than the service relaxing its rule (seam-contract D16).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MoveTaskRequest {
    pub task_id: String,
    pub column: BoardColumn,
    /// The task that ends up directly *above* this one.
    #[serde(default)]
    pub before_task_id: Option<String>,
    /// The task that ends up directly *below* this one.
    #[serde(default)]
    pub after_task_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AddTaskLinkRequest {
    pub task_id: String,
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RemoveTaskLinkRequest {
    /// The link's own id, which `get_task` returns beside each link — not the
    /// task's id, and not the URL.
    pub link_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SetTaskDependenciesRequest {
    pub task_id: String,
    /// The complete set. This replaces whatever the task depended on before,
    /// and an empty list clears every dependency.
    pub depends_on: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn an_omitted_field_is_a_no_op_and_a_cleared_one_is_named() {
        let request: UpdateTaskRequest = serde_json::from_value(json!({
            "task_id": "abc",
            "clear": ["model"],
        }))
        .expect("a minimal patch deserializes");

        assert_eq!(request.task_id, "abc");
        assert_eq!(request.title, None);
        assert_eq!(request.plan, None);
        assert_eq!(request.clear, vec![ClearableField::Model]);
        request
            .ensure_no_conflicting_clear()
            .expect("clearing a field nobody set is fine");
    }

    #[test]
    fn a_field_both_set_and_cleared_is_refused_naming_it() {
        let request: UpdateTaskRequest = serde_json::from_value(json!({
            "task_id": "abc",
            "model": "opus",
            "clear": ["model"],
        }))
        .expect("the request itself is well-formed");

        let error = request
            .ensure_no_conflicting_clear()
            .expect_err("the two halves contradict each other");

        assert_eq!(
            error.to_string(),
            "model was given both a value and a place in `clear` — send one or the other"
        );
    }

    #[test]
    fn the_plan_cannot_be_named_in_clear_at_all() {
        // Not a service rule and not enforced twice: the *schema* has no such
        // value, so a caller asking for it is refused by the deserializer with
        // serde's own list of what is legal.
        let error = serde_json::from_value::<UpdateTaskRequest>(json!({
            "task_id": "abc",
            "clear": ["plan"],
        }))
        .expect_err("`plan` is not a clearable field");

        assert!(
            error.to_string().contains("unknown variant `plan`"),
            "the caller is told what the legal values are: {error}"
        );
    }

    #[test]
    fn an_unknown_field_is_refused_rather_than_silently_dropped() {
        // `deny_unknown_fields` is what makes a typo an error a caller can fix
        // instead of a patch that silently did less than it said.
        let error = serde_json::from_value::<CreateTaskRequest>(json!({
            "repository_id": "repo",
            "title": "A task",
            "colum": "ready",
        }))
        .expect_err("a misspelled field is refused");

        assert!(
            error.to_string().contains("unknown field `colum`"),
            "the message names the typo: {error}"
        );
    }

    #[test]
    fn an_illegal_column_is_refused_naming_the_legal_ones() {
        // Task 010's "column must be one of ..., not 'invalid input'", got for
        // free by deserializing into the enum rather than into a `String`.
        let error = serde_json::from_value::<CreateTaskRequest>(json!({
            "repository_id": "repo",
            "title": "A task",
            "column": "todo",
        }))
        .expect_err("`todo` is not a column");

        let message = error.to_string();
        assert!(message.contains("unknown variant `todo`"), "{message}");
        for legal in ["not_ready", "ready", "in_review", "done"] {
            assert!(message.contains(legal), "{legal} must be listed: {message}");
        }
    }
}
