//! What the eleven tools hand back (ADR-0006 and its 2026-08-28 amendment,
//! seam-contract D16).
//!
//! Projections, not mirrors. A conversion layer is unavoidable — the row types
//! serialize `camelCase` for the frontend and MCP is `snake_case` — so these
//! earn it by also deciding *what an external planning agent has any business
//! seeing*. Two things are left out on purpose and each says so at its field.
//!
//! Every task-returning tool answers with a [`TaskView`], including the
//! mutating ones. That costs one extra read on a write and buys a uniform
//! contract: whatever an agent touched, it gets the full current state of it
//! back, and never has to call `get_task` to find out what its own call did.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use crate::db::{
    BoardColumn, ExitClass, MutationSource, Repository, Run, RunState, RunStatus, StrategyMode,
    StrategySource, TaskLink,
};
use crate::tasks::{TaskDetail, TaskSummary};

/// One registered repository, as `list_repositories` reports it.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RepositoryView {
    pub id: String,
    pub name: String,
    pub path: String,
    pub default_branch: String,
    /// ADR-0012's per-repository opt-in. Surfaced because it is the difference
    /// between a task that will run unattended tonight and one that will sit
    /// in `ready` waiting for a human to say yes.
    pub allow_unattended_runs: bool,
}

impl From<Repository> for RepositoryView {
    fn from(repository: Repository) -> Self {
        Self {
            id: repository.id,
            name: repository.name,
            path: repository.path,
            default_branch: repository.default_branch,
            allow_unattended_runs: repository.allow_unattended_runs,
        }
    }
}

/// One task in full: the plan, the links, what it depends on, and how its last
/// attempt ended.
///
/// Omits `position` deliberately: board arithmetic is not an external planner's
/// business — it sends `before_task_id` and `after_task_id`, exactly as the
/// frontend does.
///
/// The four `strategy_*` fields were omitted for the same kind of reason until
/// task 020, and are here now because that task gave them a caller. A planner
/// answering with `set_task_strategy` gets the card back with its own proposal
/// on it, and any agent calling `get_task` can see whether a strategy is
/// recorded, who decided it, and when — which is the difference between amending
/// a plan whose model was chosen by a human and one nobody has looked at.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct TaskView {
    pub id: String,
    pub repository_id: String,
    pub title: String,
    pub plan: Option<String>,
    pub extra_instructions: Option<String>,
    pub column: BoardColumn,
    pub run_state: RunState,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// How `model` and `effort` get chosen (ADR-0016). The **stored** mode, not
    /// the resolved one: `default` here means "fall through to the repository
    /// and then the settings" (seam-contract D17.6), and reporting the resolved
    /// value would tell a caller its card names a model it does not.
    pub strategy_mode: StrategyMode,
    /// Whose decision the next run executes — `planner` until a human takes
    /// authorship, which is what accepting a proposal means (D17.7).
    pub strategy_source: Option<StrategySource>,
    /// The recorded proposal, **verbatim as stored**: the version-1 envelope
    /// seam-contract D17.3 documents, as JSON text.
    ///
    /// Text rather than a re-serialization of this build's own struct, so an
    /// envelope written by a later Rimaia reaches the caller with everything it
    /// carried, and one somebody hand-edited into nonsense reaches it as the
    /// nonsense it is rather than as a parse failure on a `get_task` that had
    /// nothing to do with the strategy. It is the same text the panel
    /// `JSON.parse`s, so there is one document and not two spellings of it.
    pub strategy_plan: Option<String>,
    pub strategy_updated_at: Option<DateTime<Utc>>,
    /// Creation provenance (ADR-0019): a task an agent created reads `mcp`
    /// forever, even after the user edits it on the board.
    pub source: MutationSource,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub links: Vec<TaskLinkView>,
    pub depends_on: Vec<String>,
    pub last_run: Option<RunView>,
}

impl From<TaskDetail> for TaskView {
    fn from(detail: TaskDetail) -> Self {
        // Destructured exhaustively rather than with `..`, so a field added to
        // `TaskDetail` has to be either projected or declined here.
        //
        // The three `effective_*` are declined: they are the *resolved* answer
        // the board renders on a card, and an agent reading a task wants to know
        // what the card itself says — a `strategy_mode` of `default` with no
        // model is a task nobody has decided anything about, which is precisely
        // what an inherited "opus" would hide from it.
        let TaskDetail {
            task,
            links,
            depends_on,
            last_run,
            effective_model: _,
            effective_effort: _,
            effective_origin: _,
        } = detail;

        Self {
            id: task.id,
            repository_id: task.repository_id,
            title: task.title,
            plan: task.plan,
            extra_instructions: task.extra_instructions,
            column: task.column,
            run_state: task.run_state,
            branch: task.branch,
            worktree_path: task.worktree_path,
            model: task.model,
            effort: task.effort,
            strategy_mode: task.strategy_mode,
            strategy_source: task.strategy_source,
            strategy_plan: task.strategy_plan,
            strategy_updated_at: task.strategy_updated_at,
            source: task.source,
            created_at: task.created_at,
            updated_at: task.updated_at,
            links: links.into_iter().map(TaskLinkView::from).collect(),
            depends_on,
            last_run: last_run.map(RunView::from),
        }
    }
}

/// What `list_repositories` answers with.
///
/// An object wrapping the array rather than a bare array, and not for taste:
/// MCP requires a tool's `outputSchema` to be an object schema, and Claude
/// Code refuses the whole `tools/list` response when one is not — *silently
/// dropping every other tool with it*. A bare `Vec` produces
/// `{"type": "array"}` and earns `expected "object" (at
/// tools.N.outputSchema.type)`, which is how this was found: the CLI, not a
/// test. Any future list-returning tool wraps its array the same way.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RepositoryListView {
    pub repositories: Vec<RepositoryView>,
}

/// What `list_tasks` answers with. Wrapped for the reason
/// [`RepositoryListView`] gives.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct TaskListView {
    pub tasks: Vec<TaskListItem>,
}

/// One card's worth of a task, as `list_tasks` reports it.
///
/// **No plan** (seam-contract D16): fifty tasks times a multi-thousand-word
/// plan is a context bomb in the caller's own session, and `has_plan` answers
/// the only question a list has about one — whether the task could enter
/// `ready` at all. `get_task` is how an agent reads a plan.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct TaskListItem {
    pub id: String,
    pub repository_id: String,
    pub title: String,
    pub column: BoardColumn,
    pub run_state: RunState,
    pub has_plan: bool,
    pub link_count: i64,
    pub dependency_count: i64,
    pub updated_at: DateTime<Utc>,
}

impl From<TaskSummary> for TaskListItem {
    fn from(summary: TaskSummary) -> Self {
        Self {
            id: summary.task.id,
            repository_id: summary.task.repository_id,
            title: summary.task.title,
            column: summary.task.column,
            run_state: summary.task.run_state,
            // The same "something other than whitespace" the service's own
            // ready-needs-a-plan guard uses, reached through the row it
            // normalised: a blank plan is stored as NULL, never as "".
            has_plan: summary.task.plan.is_some(),
            link_count: summary.link_count,
            dependency_count: summary.dependency_count,
            updated_at: summary.task.updated_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct TaskLinkView {
    /// The link's own id — what `remove_task_link` takes.
    pub id: String,
    pub label: String,
    pub url: String,
}

impl From<TaskLink> for TaskLinkView {
    fn from(link: TaskLink) -> Self {
        Self {
            id: link.id,
            label: link.label,
            url: link.url,
        }
    }
}

/// How a task's most recent attempt ended.
///
/// Enough for an agent to decide whether to amend a plan and hand it back, and
/// nothing more: the transcript is a file on disk (ADR-0013), and reading it
/// is not something this tool surface offers.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RunView {
    pub id: String,
    pub attempt: i64,
    pub status: RunStatus,
    pub exit_class: Option<ExitClass>,
    pub error_message: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub pr_url: Option<String>,
}

impl From<Run> for RunView {
    fn from(run: Run) -> Self {
        Self {
            id: run.id,
            attempt: run.attempt,
            status: run.status,
            exit_class: run.exit_class,
            error_message: run.error_message,
            started_at: run.started_at,
            ended_at: run.ended_at,
            pr_url: run.pr_url,
        }
    }
}

/// The standing instructions every run is prepended with (ADR-0009).
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct BaseInstructionsView {
    /// Verbatim, with `{{...}}` placeholders left unexpanded — see
    /// `mcp::server::get_base_instructions` for why a composed preview is not
    /// well defined without a task.
    pub base_instructions: String,
    /// The placeholder names that exist, so an agent writing a plan can use
    /// one rather than inventing it.
    pub template_variables: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Task;
    use crate::strategy::StrategyOrigin;
    use crate::tasks::LastRunSummary;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn task() -> Task {
        Task {
            id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
            repository_id: "3f2b1c00-0000-4000-8000-000000000002".to_string(),
            title: "Wire the board to the store".to_string(),
            plan: Some("## Steps\n1. ...".to_string()),
            extra_instructions: None,
            column: BoardColumn::Ready,
            position: 1.5,
            run_state: RunState::Idle,
            branch: None,
            worktree_path: None,
            strategy_mode: StrategyMode::Default,
            model: Some("opus".to_string()),
            effort: None,
            strategy_plan: Some(r#"{"phases":[]}"#.to_string()),
            strategy_source: None,
            strategy_updated_at: None,
            created_at: "2026-08-20T12:00:00Z".parse().expect("a literal timestamp"),
            updated_at: "2026-08-20T12:30:00Z".parse().expect("a literal timestamp"),
            source: MutationSource::Mcp,
        }
    }

    #[test]
    fn a_task_view_serializes_the_keys_the_tool_schema_promises() {
        let view = TaskView::from(TaskDetail {
            task: task(),
            links: vec![TaskLink {
                id: "link-1".to_string(),
                task_id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
                label: "ADR-0006".to_string(),
                url: "https://example.com/adr".to_string(),
                position: 0.0,
            }],
            depends_on: vec!["3f2b1c00-0000-4000-8000-000000000009".to_string()],
            last_run: None,
            // What the board would render on the card. Set to something this
            // view must *not* pick up: the row names no effort, so an
            // `effective_effort` reaching the wire would be the repository's
            // answer presented as the task's.
            effective_model: Some("opus".to_string()),
            effective_effort: Some("high".to_string()),
            effective_origin: StrategyOrigin::Repository,
        });

        let wire = serde_json::to_value(&view).expect("a DTO must always serialize");

        // snake_case, in both directions, and unlike the row's own camelCase
        // (seam-contract D16).
        assert_eq!(
            wire["repository_id"],
            json!("3f2b1c00-0000-4000-8000-000000000002")
        );
        assert_eq!(wire["run_state"], json!("idle"));
        assert_eq!(wire["extra_instructions"], json!(null));
        assert_eq!(wire["source"], json!("mcp"));
        assert_eq!(
            wire["links"],
            json!([{ "id": "link-1", "label": "ADR-0006", "url": "https://example.com/adr" }])
        );
        assert_eq!(wire["last_run"], json!(null));

        // The strategy, since task 020 gave these fields a caller. The envelope
        // crosses as the text that is in the column, not as this build's struct
        // re-serialized (seam-contract D17.3).
        assert_eq!(wire["strategy_mode"], json!("default"));
        assert_eq!(wire["strategy_source"], json!(null));
        assert_eq!(wire["strategy_plan"], json!(r#"{"phases":[]}"#));
        assert_eq!(wire["strategy_updated_at"], json!(null));

        // Deliberately absent, not forgotten.
        assert!(wire.get("position").is_none(), "board arithmetic is ours");
        assert_eq!(
            wire["effort"],
            json!(null),
            "the row's own effort, never the repository default the card renders"
        );
        assert!(wire.get("effective_effort").is_none());
        assert!(wire.get("effective_origin").is_none());
    }

    #[test]
    fn a_task_list_item_omits_the_plan() {
        let item = TaskListItem::from(TaskSummary {
            task: task(),
            link_count: 2,
            dependency_count: 1,
            blocked_by_incomplete: false,
            last_run: Some(LastRunSummary {
                status: RunStatus::Succeeded,
                exit_class: Some(ExitClass::Success),
                ended_at: None,
            }),
            // The card's badge, which `list_tasks` does not carry for the reason
            // `TaskView` does not: a summary reports what the row says, and the
            // resolved answer is the board's rendering of it.
            effective_model: Some("opus".to_string()),
            effective_effort: Some("high".to_string()),
            effective_origin: StrategyOrigin::Repository,
        });

        let wire = serde_json::to_value(&item).expect("a DTO must always serialize");

        assert!(
            wire.get("plan").is_none(),
            "fifty plans in one response is a context bomb (D16)"
        );
        assert_eq!(wire["has_plan"], json!(true));
        assert_eq!(wire["link_count"], json!(2));
        assert_eq!(wire["dependency_count"], json!(1));
    }

    #[test]
    fn a_task_without_a_plan_reports_has_plan_false() {
        let mut without = task();
        without.plan = None;

        let item = TaskListItem::from(TaskSummary {
            task: without,
            link_count: 0,
            dependency_count: 0,
            blocked_by_incomplete: false,
            last_run: None,
            effective_model: None,
            effective_effort: None,
            effective_origin: StrategyOrigin::ClaudeCode,
        });

        assert!(!item.has_plan);
    }

    #[test]
    fn a_run_view_carries_why_the_last_attempt_stopped() {
        let view = RunView::from(Run {
            id: "run-1".to_string(),
            task_id: "task-1".to_string(),
            attempt: 2,
            status: RunStatus::Failed,
            session_id: "session".to_string(),
            prompt: "the whole composed prompt".to_string(),
            started_at: "2026-08-20T12:00:00Z".parse().expect("a literal timestamp"),
            ended_at: Some("2026-08-20T12:30:00Z".parse().expect("a literal timestamp")),
            exit_class: Some(ExitClass::UsageLimit),
            error_message: Some("usage limit reached".to_string()),
            num_turns: Some(12),
            cost_usd: Some(1.5),
            log_path: "/tmp/run-1.jsonl".to_string(),
            pr_url: None,
            resume_after: None,
        });

        let wire = serde_json::to_value(&view).expect("a DTO must always serialize");

        assert_eq!(wire["exit_class"], json!("usage_limit"));
        assert_eq!(wire["error_message"], json!("usage limit reached"));
        // The transcript is a file (ADR-0013) and the prompt is the run's own
        // record; neither is an agent's business through this surface.
        assert!(wire.get("log_path").is_none());
        assert!(wire.get("prompt").is_none());
        assert!(wire.get("cost_usd").is_none());
    }
}
