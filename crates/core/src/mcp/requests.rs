//! What the registered tools take off the wire (ADR-0006, its 2026-08-28
//! amendment, seam-contract D16).
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

use chrono::{DateTime, Utc};

use crate::db::settings::Dismissal;
use crate::db::{BoardColumn, RunState, ScheduleMode, StrategyMode};
use crate::doctor::Check;
use crate::error::{Error, Result};
use crate::runner::strategy::PlanSelection;
use crate::schedule::ScheduleInput;
use crate::strategy::StrategyApproval;
use crate::tasks::{StrategyPhase, StrategyPlan, StrategyWorkflow};
use crate::worktree::AutoCleanup;

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
    /// ADR-0016's mode, and the one part of a strategy that *is* a patch field
    /// (ADR-0006's amendment): it is a plain enum an operator sets from either
    /// door, where the planner's proposal is a document with its own tool.
    ///
    /// Not in [`ClearableField`], and D16.5's asymmetry is not an argument that
    /// it should be: that rule exists because sending `null` for a *plan*
    /// destroys four thousand words nobody can get back. A mode has three legal
    /// values, one of which — [`StrategyMode::Default`] — already means "no
    /// opinion", so "erase it" has both a spelling and no cost.
    ///
    /// Setting `model` or `effort` in the same call overrides whatever this
    /// says, per seam-contract D17.6, in
    /// [`tasks::update_task`](crate::tasks::update_task) — where both doors get
    /// it, rather than here where only one would.
    #[serde(default)]
    pub strategy_mode: Option<StrategyMode>,
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

/// `set_task_strategy`: a planner's whole answer, in one call (ADR-0006's
/// 2026-08-28 amendment).
///
/// Not a patch, and that is the amendment's first argument for it being its own
/// tool: the envelope is only coherent whole, so there is no `clear` list and no
/// field whose absence means "leave what was there". A proposal that names no
/// model names no model.
///
/// Two things this deliberately does **not** take. `status`, because a proposal
/// is what this tool records and a planner declaring its own write a failure
/// would be recording an outcome the runner owns; and `run` — the session id,
/// turn count and cost — because the planner is still mid-session when it calls
/// this and the runner amends the envelope with its own accounting once the
/// process has exited (seam-contract D17.3). `version` is stamped by
/// [`tasks::set_task_strategy`](crate::tasks::set_task_strategy) for the reason
/// that function gives.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SetTaskStrategyRequest {
    pub task_id: String,
    /// The model id the implementation run should spawn with — one of the ids
    /// the prompt listed, verbatim, since it reaches `--model` unchanged.
    /// Omitting it is legal and means "no opinion": the task then falls back
    /// through the repository and global defaults.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub workflow: Option<StrategyWorkflow>,
    /// The phase breakdown, when the work is worth splitting. Rendered into the
    /// implementation prompt as guidance for the *agent*, which runs the phases
    /// itself; nothing here reaches a flag or a second process (ADR-0016).
    #[serde(default)]
    pub phases: Vec<StrategyPhaseRequest>,
    /// Why this strategy, for the human reading the card in the morning.
    #[serde(default)]
    pub rationale: Option<String>,
}

/// One phase of a proposal.
///
/// Its own DTO rather than [`StrategyPhase`] itself, for this module's stated
/// reason: a request is a message from a caller who can be told they got a field
/// name wrong, so it is `deny_unknown_fields`, where the stored envelope is a
/// document that must stay readable when a later version adds a key.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct StrategyPhaseRequest {
    pub name: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    /// How many agents the phase wants. Defaulted rather than required, and to
    /// the same one the stored envelope defaults to, so a phase that arrived
    /// here and a phase hand-written into the column describe the same thing.
    #[serde(default = "one_agent")]
    pub agents: u32,
    #[serde(default)]
    pub summary: String,
}

fn one_agent() -> u32 {
    1
}

impl SetTaskStrategyRequest {
    /// The proposal as the service's own document.
    ///
    /// A conversion and nothing else: no defaulting a missing model, no
    /// rejecting a `multi_agent` with no phases, no deciding whether the task
    /// may be written to. Every one of those is
    /// [`tasks::set_task_strategy`](crate::tasks::set_task_strategy)'s, so that
    /// the panel and this tool cannot come to disagree about them (ADR-0006).
    pub fn into_plan(self) -> StrategyPlan {
        StrategyPlan {
            workflow: self.workflow,
            phases: self
                .phases
                .into_iter()
                .map(|phase| StrategyPhase {
                    name: phase.name,
                    model: phase.model,
                    effort: phase.effort,
                    agents: phase.agents,
                    summary: phase.summary,
                })
                .collect(),
            rationale: self.rationale,
            ..StrategyPlan::proposed(self.model, self.effort)
        }
    }
}

/// A task id and nothing else — the shape every task-scoped tool that takes no
/// other argument shares.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskStrategyRequest {
    pub task_id: String,
}

/// Which defaults to read: one repository's, or the global ones beneath them.
///
/// `None` means global. A separate tool per scope was the alternative and it
/// would double the surface to express one optional field (ADR-0021's own
/// warning about a list that is large and badly described).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct GetStrategyDefaultsRequest {
    #[serde(default)]
    pub repository_id: Option<String>,
}

/// The defaults to store, and where.
///
/// `mode`, `model` and `effort` are the whole record, so this replaces rather
/// than patches: there is no `clear` list because sending the record without a
/// `model` already says "no model", and D16.5's argument for an explicit clear
/// applies to a field whose accidental erasure is expensive. A default model is
/// not that.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SetStrategyDefaultsRequest {
    /// Omitted sets the global defaults.
    #[serde(default)]
    pub repository_id: Option<String>,
    pub mode: StrategyMode,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
}

/// The catalogue as a JSON document.
///
/// A string rather than a typed structure, deliberately: the catalogue is
/// configuration whose whole point is that a new model does not require a
/// release (ADR-0016), and a typed request would put this crate's idea of the
/// shape between the operator and their own settings row. The service parses it
/// and refuses what it cannot read, which is the one place that check belongs.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SetStrategyCatalogueRequest {
    pub catalogue: String,
}

/// Whether a planned strategy waits for a human before the implementation run.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SetStrategyApprovalRequest {
    pub approval: StrategyApproval,
}

/// One run at a time, or several (ADR-0010's Modes).
///
/// The enum rather than a string, for the reason every other enum on this
/// surface is one: a tool advertising `mode: string` is a tool that gets
/// `"concurrent"` sent to it and has to explain why that is not a mode.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SetScheduleModeRequest {
    pub mode: ScheduleMode,
}

/// How many runs the queue may have in flight at once in parallel mode.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SetMaxConcurrencyRequest {
    /// Between 1 and the ceiling `get_run_capacity` reports. `usize` rather
    /// than a signed integer so a negative arrives as a schema error naming the
    /// field, not as a service refusal a caller has to read prose to
    /// understand.
    pub max_concurrency: usize,
}

/// ADR-0010's per-repository opt-out, for one repository.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SetRepositoryMaxConcurrencyRequest {
    /// From `list_repositories` — a UUID, not a name or a path.
    pub repository_id: String,
    /// How many runs this repository will hold at once. `1` is the default and
    /// the safe answer; raising it means two agents in two worktrees of the
    /// same repository, which git tolerates and ports, test databases and
    /// lockfiles do not.
    pub max_concurrency: i64,
}

// ---------------------------------------------------------------------------
// Schedules (task 013, ADR-0010). Operator-only, every one of them.
// ---------------------------------------------------------------------------

/// The whole configuration of a schedule, which is what both `create_schedule`
/// and `update_schedule` take.
///
/// A whole row rather than a patch, matching
/// [`ScheduleInput`](crate::schedule::ScheduleInput) — the fields constrain each
/// other (`cron` and `start_at` are exclusive; `stop_at` is meaningless without
/// a `timezone` to resolve it through), so "clear the cron and set a one-off
/// time" has to be one write or there is an illegal row in the middle of it.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ScheduleConfigRequest {
    /// What the user calls this schedule — it is what the Runs view says while
    /// the window is open ("Running until 06:00 — Nightly").
    pub name: String,
    /// `sequential` or `parallel`. Overrides the installation default **while
    /// this schedule's window is open**, and only then.
    pub mode: ScheduleMode,
    /// How many runs at once, in `parallel`. Between 1 and the ceiling
    /// `get_run_capacity` reports.
    pub max_concurrency: i64,
    /// An IANA name, such as `"Europe/Copenhagen"`. **Required** — `list_timezones`
    /// is where the value comes from. Not an offset and not an abbreviation:
    /// a nightly queue configured with one runs an hour out for half the year.
    pub timezone: String,
    /// A cron expression, read in `timezone`. `"0 22 * * *"` is every night at
    /// 22:00. Exclusive with `start_at`.
    #[serde(default)]
    pub cron: Option<String>,
    /// A one-off instant, RFC 3339. Fires once. Exclusive with `cron`.
    #[serde(default)]
    pub start_at: Option<DateTime<Utc>>,
    /// A local time of day, `"HH:MM"`, at which the window stops starting new
    /// tasks. Runs already in flight are allowed to finish. Omit for a window
    /// that runs until something pauses it.
    #[serde(default)]
    pub stop_at: Option<String>,
    pub enabled: bool,
}

impl From<ScheduleConfigRequest> for ScheduleInput {
    fn from(request: ScheduleConfigRequest) -> Self {
        Self {
            name: request.name,
            mode: request.mode,
            max_concurrency: request.max_concurrency,
            timezone: request.timezone,
            cron: request.cron,
            start_at: request.start_at,
            stop_at: request.stop_at,
            enabled: request.enabled,
        }
    }
}

/// `update_schedule`: which schedule, and its whole new configuration.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UpdateScheduleRequest {
    pub schedule_id: String,
    #[serde(flatten)]
    pub config: ScheduleConfigRequest,
}

/// A schedule id and nothing else — the shape `delete_schedule` and
/// `preview_schedule_preflight` share.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ScheduleRequest {
    pub schedule_id: String,
}

/// `set_schedule_enabled`: turning one on or off without deleting it.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SetScheduleEnabledRequest {
    pub schedule_id: String,
    pub enabled: bool,
}

/// The worktree auto-removal policy to store (task 016).
///
/// One field, and it is an enum whose "on" value spells its own
/// acknowledgement — `on_done_acknowledged` rather than `true`. An agent
/// setting this has to type the word, which is the closest a tool schema comes
/// to the sentence the Settings panel puts in front of a human.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SetWorktreeAutoCleanupRequest {
    pub setting: AutoCleanup,
}

/// One repository, by id (task 022).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RepositoryRequest {
    /// From `list_repositories` — a UUID, not derivable from a name or a path.
    pub repository_id: String,
}

/// The window `get_analytics` reports on (task 024).
///
/// Both bounds optional: omitting them is all time, which is the answer for
/// "what has this thing cost me". `from` is inclusive and `to` is exclusive, so
/// two adjacent periods cannot both claim a run.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", default, deny_unknown_fields)]
pub struct AnalyticsRequest {
    /// RFC 3339, e.g. `2026-09-01T00:00:00Z`.
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

/// What the user pays per month, or `null` to clear it (task 024).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", default, deny_unknown_fields)]
pub struct SubscriptionCostRequest {
    pub monthly_usd: Option<f64>,
}

/// Which cards `plan_tasks_strategy` plans (task 023).
///
/// Every stated field narrows the set, and stating none of them is refused
/// rather than taken to mean the whole board — `PlanSelection` is a core type
/// precisely so this surface and the board's "Plan all" cannot disagree about
/// what "the ready column" is.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", default, deny_unknown_fields)]
pub struct PlanSelectionRequest {
    /// `not_ready`, `ready`, `in_review` or `done`. `ready` is the run queue and
    /// is what a preflight is normally about.
    pub column: Option<BoardColumn>,
    pub repository_id: Option<String>,
    /// A hand-picked set. An id that is not in the rest of the selection is
    /// refused naming it, never silently dropped.
    pub task_ids: Vec<String>,
}

impl From<PlanSelectionRequest> for PlanSelection {
    fn from(request: PlanSelectionRequest) -> Self {
        PlanSelection {
            column: request.column,
            repository_id: request.repository_id,
            task_ids: request.task_ids,
        }
    }
}

/// One doctor row to put down, or pick back up (task 027).
///
/// All three fields, because a dismissal is keyed on the row's *content*: the
/// same check about a different repository is a different warning, and a
/// changed `detail` is a sentence the user has not read yet. A tool that took
/// only `check` would be the mute button task 027's Notes argue against.
///
/// Copy the three values out of `run_doctor`'s own output rather than composing
/// them — a `detail` that differs by a character matches nothing.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DoctorDismissalRequest {
    pub check: Check,
    /// The repository the row was about, for the two per-repository checks.
    /// Omit it for the six that describe the installation as a whole.
    #[serde(default)]
    pub repository: Option<String>,
    pub detail: String,
}

impl From<DoctorDismissalRequest> for Dismissal {
    fn from(request: DoctorDismissalRequest) -> Self {
        Dismissal {
            check: request.check,
            repository: request.repository,
            detail: request.detail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::StrategyPlanStatus;
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
    fn a_proposal_becomes_a_proposed_envelope_without_the_planner_saying_so() {
        // `status` is not on the request at all: this tool records proposals,
        // and a planner declaring its own write a failure would be recording an
        // outcome the runner owns.
        let request: SetTaskStrategyRequest = serde_json::from_value(json!({
            "task_id": "abc",
            "model": "sonnet",
            "effort": "high",
            "workflow": "multi_agent",
            "phases": [{ "name": "Schema", "summary": "the migration" }],
            "rationale": "The plan names a migration and a command surface.",
        }))
        .expect("a planner's answer deserializes");

        let plan = request.into_plan();

        assert_eq!(plan.status, StrategyPlanStatus::Proposed);
        assert_eq!(plan.model.as_deref(), Some("sonnet"));
        assert_eq!(plan.effort.as_deref(), Some("high"));
        assert_eq!(plan.workflow, Some(StrategyWorkflow::MultiAgent));
        assert_eq!(plan.phases.len(), 1);
        assert_eq!(
            plan.phases[0].agents, 1,
            "a phase that omits its agent count describes one agent"
        );
        assert_eq!(
            plan.rationale.as_deref(),
            Some("The plan names a migration and a command surface.")
        );
        assert_eq!(
            plan.run, None,
            "the planner is still mid-session; the runner records the accounting"
        );
    }

    #[test]
    fn a_proposal_that_names_no_model_is_a_proposal_and_not_a_bad_request() {
        // "No opinion" is a legal answer — the task falls back through the
        // repository and global defaults — so it must not be spelled by
        // omitting the *call*, which is the one failure this tool cannot tell
        // from a planner that crashed.
        let request: SetTaskStrategyRequest =
            serde_json::from_value(json!({ "task_id": "abc" })).expect("a bare proposal is legal");

        let plan = request.into_plan();

        assert_eq!(plan.model, None);
        assert_eq!(plan.effort, None);
        assert!(plan.phases.is_empty());
    }

    #[test]
    fn a_strategy_field_misspelled_inside_a_phase_is_refused_naming_it() {
        // The nested object is `deny_unknown_fields` too — the reason phases
        // have a request DTO of their own rather than reusing the stored
        // envelope's tolerant one.
        let error = serde_json::from_value::<SetTaskStrategyRequest>(json!({
            "task_id": "abc",
            "phases": [{ "name": "Schema", "sumary": "the migration" }],
        }))
        .expect_err("a misspelled field is refused");

        assert!(
            error.to_string().contains("unknown field `sumary`"),
            "the message names the typo: {error}"
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
