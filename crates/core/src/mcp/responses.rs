//! What the registered tools hand back (ADR-0006, its 2026-08-28 amendment,
//! and ADR-0021,
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

use crate::analytics::Analytics;
use crate::credentials::StoreStatus;
use crate::db::settings::Dismissal;
use crate::db::{
    BoardColumn, ExitClass, MutationSource, Repository, Run, RunState, RunStatus, Schedule,
    ScheduleMode, StrategyMode, StrategySource, TaskLink,
};
use crate::doctor::{CheckResult, DoctorReport};
use crate::runner::strategy::{PlanOutcome, PlanPass, PlanResult};
use crate::schedule::{PreflightSummary, ScheduleView as CoreScheduleView};
use crate::scheduler::RunCapacity;
use crate::strategy::StrategyApproval;
use crate::tasks::{TaskDetail, TaskSummary};
use crate::worktree::{AutoCleanup, WorktreeInventoryEntry};

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
    /// ADR-0010's per-repository cap, `1` unless the operator opted out.
    /// Surfaced for the same reason as the flag above: it is the difference
    /// between two of this repository's tasks running tonight and them running
    /// one after the other.
    pub max_concurrency: i64,
}

impl From<Repository> for RepositoryView {
    fn from(repository: Repository) -> Self {
        Self {
            id: repository.id,
            name: repository.name,
            path: repository.path,
            default_branch: repository.default_branch,
            allow_unattended_runs: repository.allow_unattended_runs,
            max_concurrency: repository.max_concurrency,
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

/// One row of what `run_doctor` answers with (task 018).
///
/// A projection rather than [`CheckResult`] re-serialized, for the reason this
/// module's header gives — and for one more that is specific to this pair.
/// `is_blocking` and `blocking_summary` on [`DoctorReportView`] are *derived*
/// here rather than left for the caller to recompute: an agent asked to decide
/// whether the queue can start should not have to know that "blocking" means
/// "any status is fail", which is exactly the kind of rule ADR-0006 refuses to
/// let a second surface reimplement.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CheckResultView {
    pub check: String,
    pub label: String,
    pub status: String,
    /// The repository this row is about, for the two per-repository checks.
    pub repository: Option<String>,
    pub detail: String,
    pub remediation: Option<String>,
    /// Whether the user has already read this exact row and put it down (task
    /// 027). Only ever true of a `warn`, and it changes nothing about
    /// `is_blocking` — see [`DoctorReportView`].
    pub dismissed: bool,
}

impl From<&CheckResult> for CheckResultView {
    fn from(result: &CheckResult) -> Self {
        Self {
            check: result.check.as_str().to_string(),
            label: result.label.to_string(),
            status: result.status.as_str().to_string(),
            repository: result.repository.clone(),
            detail: result.detail.clone(),
            remediation: result.remediation.clone(),
            dismissed: result.dismissed,
        }
    }
}

/// Whether a repository has a forge token of its own, and whose (task 022).
///
/// **Carries the login, the label and the date — never the secret.** There is
/// no field here that could be widened into one, and the two tools that would
/// have to take a token do not exist (seam-contract D25).
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CredentialStatusView {
    pub repository_id: String,
    pub repository: String,
    pub configured: bool,
    /// The forge login the token resolved to, or `null` for a save `gh` could
    /// not verify at the time.
    pub login: Option<String>,
    pub label: Option<String>,
    pub added_at: Option<DateTime<Utc>>,
    /// `stored`, `absent` or `unavailable`. **`absent` with `configured` true
    /// is the state that refuses a run**: the row says this repository has a
    /// token and the keychain does not have it, and Rimaia will not fall back
    /// to the operator's own login.
    pub keychain: String,
    pub keychain_detail: Option<String>,
}

impl CredentialStatusView {
    pub fn new(repository: &Repository, store: StoreStatus) -> Self {
        let (keychain, keychain_detail) = match &store {
            StoreStatus::Stored => ("stored", None),
            StoreStatus::Absent => ("absent", None),
            StoreStatus::Unavailable { reason } => ("unavailable", Some(reason.clone())),
        };
        Self {
            repository_id: repository.id.clone(),
            repository: repository.name.clone(),
            configured: repository.credential_added_at.is_some(),
            login: repository.credential_login.clone(),
            label: repository.credential_label.clone(),
            added_at: repository.credential_added_at,
            keychain: keychain.to_string(),
            keychain_detail,
        }
    }
}

/// What `get_analytics` answers with (task 024).
///
/// A **narrower** projection than the page renders, and deliberately so: the
/// per-day chart and the longest-run link are shapes for an eye, and an agent
/// asked "is this worth it" needs the numbers that answer it. Everything here
/// is computed at read time over `runs` — no aggregate is stored (ADR-0022
/// part 3).
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct AnalyticsView {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub runs_total: usize,
    pub runs_succeeded: usize,
    pub runs_failed: usize,
    pub runs_cancelled: usize,
    pub runs_interrupted: usize,
    pub runs_running: usize,
    /// Of the runs that *ended*, `null` when none has.
    pub failure_rate: Option<f64>,
    /// Summed over the rows that have a cost. Read `runs_without_cost` before
    /// quoting it: a period predating ADR-0022's capture columns is partly
    /// unrecorded rather than cheaper (seam-contract D18).
    pub spend_usd: f64,
    pub runs_without_cost: usize,
    pub tasks_attempted: usize,
    pub tasks_completed: usize,
    /// Total spend over completed tasks — every failed attempt included, which
    /// is the only honest way to say what a finished task cost.
    pub cost_per_completed_task_usd: Option<f64>,
    pub median_duration_seconds: Option<i64>,
    /// Summed run duration, not wall-clock: parallel runs each contribute.
    pub unattended_hours: f64,
    pub models: Vec<ModelUseView>,
    pub planner_spend_usd: f64,
    pub implementation_spend_usd: f64,
    /// The user's own figure, and `null` until they give one. Absent means the
    /// comparison must not be drawn, never that the subscription is free.
    pub subscription_monthly_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ModelUseView {
    pub model: String,
    pub runs: usize,
    pub spend_usd: f64,
}

impl From<&Analytics> for AnalyticsView {
    fn from(report: &Analytics) -> Self {
        Self {
            from: report.period.from,
            to: report.period.to,
            runs_total: report.outcomes.total(),
            runs_succeeded: report.outcomes.succeeded,
            runs_failed: report.outcomes.failed,
            runs_cancelled: report.outcomes.cancelled,
            runs_interrupted: report.outcomes.interrupted,
            runs_running: report.outcomes.running,
            failure_rate: report.outcomes.failure_rate(),
            spend_usd: report.spend_usd,
            runs_without_cost: report.runs_without_cost,
            tasks_attempted: report.tasks_attempted,
            tasks_completed: report.tasks_completed,
            cost_per_completed_task_usd: report.cost_per_completed_task_usd,
            median_duration_seconds: report.median_duration_seconds,
            unattended_hours: report.unattended_hours,
            models: report
                .models
                .iter()
                .map(|use_| ModelUseView {
                    model: use_.model.clone(),
                    runs: use_.runs,
                    spend_usd: use_.spend_usd,
                })
                .collect(),
            planner_spend_usd: report.planner_spend_usd,
            implementation_spend_usd: report.implementation_spend_usd,
            subscription_monthly_usd: report.subscription_monthly_usd,
        }
    }
}

/// What the two subscription tools answer with — the stored figure, echoed back
/// after a write for [`OnboardingView`]'s reason.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SubscriptionCostView {
    pub monthly_usd: Option<f64>,
}

/// What one card's planning came to (task 023).
///
/// Flat rather than an enum-shaped union, because a tool result is read by a
/// model and a discriminated union of four shapes is harder to act on than four
/// nullable fields plus an `outcome` word. `outcome` is the thing to branch on:
/// `planned`, `skipped`, `failed` or `cancelled`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct PlanResultView {
    pub task_id: String,
    pub title: String,
    pub outcome: String,
    /// Set on `planned`.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// One line saying why the planner chose what it chose. The thing actually
    /// worth reading before going home.
    pub rationale: Option<String>,
    /// What this planner cost, off the proposal it wrote.
    pub cost_usd: Option<f64>,
    /// Set on `skipped` — a stable tag (`already_proposed`, `not_planned`,
    /// `in_flight`, `repository_not_opted_in`) and the sentence that goes with
    /// it. Set on `failed` too, where only `reason` is filled.
    pub skip: Option<String>,
    pub reason: Option<String>,
}

impl PlanResultView {
    pub fn new(task_id: &str, title: &str, outcome: &PlanOutcome) -> Self {
        let mut view = Self {
            task_id: task_id.to_string(),
            title: title.to_string(),
            outcome: "cancelled".to_string(),
            model: None,
            effort: None,
            rationale: None,
            cost_usd: None,
            skip: None,
            reason: None,
        };
        match outcome {
            PlanOutcome::Planned {
                model,
                effort,
                rationale,
                cost_usd,
            } => {
                view.outcome = "planned".to_string();
                view.model = model.clone();
                view.effort = effort.clone();
                view.rationale = rationale.clone();
                view.cost_usd = *cost_usd;
            }
            PlanOutcome::Skipped(skip) => {
                view.outcome = "skipped".to_string();
                view.skip = Some(skip.as_str().to_string());
                view.reason = Some(skip.message());
            }
            PlanOutcome::Failed(reason) => {
                view.outcome = "failed".to_string();
                view.reason = Some(reason.clone());
            }
            PlanOutcome::Cancelled => {}
        }
        view
    }
}

impl From<&PlanResult> for PlanResultView {
    fn from(result: &PlanResult) -> Self {
        Self::new(&result.task_id, &result.title, &result.outcome)
    }
}

/// What a whole pass came to. Wrapped for the reason [`RepositoryListView`]
/// gives, and carrying the two totals the summary is read for.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct PlanPassView {
    pub results: Vec<PlanResultView>,
    pub planned: usize,
    pub skipped: usize,
    /// What the pass spent, summed off the proposals it wrote.
    pub spent_usd: f64,
    /// Whether it stopped early. Proposals already written stay written.
    pub cancelled: bool,
}

impl From<&PlanPass> for PlanPassView {
    fn from(pass: &PlanPass) -> Self {
        Self {
            results: pass.results.iter().map(PlanResultView::from).collect(),
            planned: pass.planned(),
            skipped: pass.skipped(),
            spent_usd: pass.spent_usd,
            cancelled: pass.cancelled,
        }
    }
}

/// One stored dismissal, as `run_doctor` and the two dismissal tools report it
/// (task 027).
///
/// The three fields are the whole key, and they are exactly what
/// `restore_doctor_warning` takes back — so an agent clearing one copies a
/// value it was given rather than composing it.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct DismissalView {
    pub check: String,
    pub repository: Option<String>,
    pub detail: String,
}

impl From<&Dismissal> for DismissalView {
    fn from(dismissal: &Dismissal) -> Self {
        Self {
            check: dismissal.check.as_str().to_string(),
            repository: dismissal.repository.clone(),
            detail: dismissal.detail.clone(),
        }
    }
}

/// What `dismiss_doctor_warning` and `restore_doctor_warning` answer with: the
/// whole set after the write, for [`OnboardingView`]'s reason.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct DoctorDismissalsView {
    pub dismissals: Vec<DismissalView>,
}

/// What `run_doctor` answers with. Wrapped for the reason
/// [`RepositoryListView`] gives.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct DoctorReportView {
    pub results: Vec<CheckResultView>,
    /// Whether the queue would refuse to start right now.
    ///
    /// Blind to `dismissed`, as the report itself is: a user who put every
    /// warning down has changed what the banner shows and nothing about what
    /// the queue will do (task 027, D22 point 1).
    pub is_blocking: bool,
    /// The exact sentence `start_queue` would refuse with, or `null` when it
    /// would not refuse. Carried so an agent reporting the problem to a human
    /// quotes the same words the window does.
    pub blocking_summary: Option<String>,
    /// Every dismissal on record, including any that no longer match a row
    /// above — the environment was fixed, or the sentence changed.
    pub dismissals: Vec<DismissalView>,
}

impl From<&DoctorReport> for DoctorReportView {
    fn from(report: &DoctorReport) -> Self {
        Self {
            results: report.results.iter().map(CheckResultView::from).collect(),
            is_blocking: report.is_blocking(),
            blocking_summary: report.is_blocking().then(|| report.blocking_summary()),
            dismissals: report.dismissals.iter().map(DismissalView::from).collect(),
        }
    }
}

/// What `dismiss_onboarding` answers with.
///
/// An object rather than nothing at all, because a tool that answers with no
/// content gives a caller no way to tell "it worked" from "the schema was
/// wrong" — and because echoing the stored value back makes the write's effect
/// legible without a second call.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct OnboardingView {
    pub onboarding_dismissed: bool,
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

/// What `get_run_capacity`, `set_schedule_mode` and `set_max_concurrency`
/// answer with — the queue's whole configuration, from any of the three.
///
/// A setter answering with the resolved state rather than nothing is the shape
/// `set_mcp_port` already established on the Tauri side, and it is worth more
/// over MCP: a caller that had to follow every write with a read would be
/// paying two round trips to learn what the first one already knew.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RunCapacityView {
    pub mode: ScheduleMode,
    /// The *stored* limit, which is not what `sequential` resolves to. See
    /// `scheduler::capacity::RunCapacity`.
    pub max_concurrency: usize,
    /// The most runs Rimaia will supervise whatever this says. A constant, not
    /// a setting — reported so a caller can bound its own input rather than
    /// guessing and being refused.
    pub ceiling: usize,
}

impl From<RunCapacity> for RunCapacityView {
    fn from(capacity: RunCapacity) -> Self {
        Self {
            mode: capacity.mode,
            max_concurrency: capacity.max_concurrency,
            ceiling: capacity.ceiling,
        }
    }
}

/// The approval setting, wrapped.
///
/// An object rather than a bare string because every other tool on this surface
/// answers with one, and a caller that has to special-case one tool's shape is
/// a caller that will get it wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct StrategyApprovalView {
    pub approval: StrategyApproval,
}

// ---------------------------------------------------------------------------
// Schedules (task 013, ADR-0010)
// ---------------------------------------------------------------------------

/// One schedule, with the one thing about it that is computed rather than
/// stored.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ScheduleView {
    pub id: String,
    pub name: String,
    pub mode: ScheduleMode,
    pub max_concurrency: i64,
    /// The IANA name every local time on this row is read in.
    pub timezone: Option<String>,
    pub cron: Option<String>,
    pub start_at: Option<DateTime<Utc>>,
    /// A local time of day, `"HH:MM"`.
    pub stop_at: Option<String>,
    pub enabled: bool,
    /// When it **actually** last fired, never when it was due.
    pub last_fired_at: Option<DateTime<Utc>>,
    /// The instant from which missed occurrences count.
    pub armed_at: Option<DateTime<Utc>>,
    /// When it fires next. **In the past when the schedule is overdue**, which
    /// is the one case worth seeing — reporting tomorrow's time for a schedule
    /// that should have started an hour ago would hide it.
    pub next_fire_at: Option<DateTime<Utc>>,
    /// Why there is no next fire time, when a broken row is the reason. A field
    /// rather than a failed read, so one unparseable cron expression does not
    /// make the whole list — the list the caller would use to *fix* it —
    /// unreadable.
    pub next_fire_error: Option<String>,
}

impl From<CoreScheduleView> for ScheduleView {
    fn from(view: CoreScheduleView) -> Self {
        Self {
            id: view.schedule.id,
            name: view.schedule.name,
            mode: view.schedule.mode,
            max_concurrency: view.schedule.max_concurrency,
            timezone: view.schedule.timezone,
            cron: view.schedule.cron,
            start_at: view.schedule.start_at,
            stop_at: view.schedule.stop_at,
            enabled: view.schedule.enabled,
            last_fired_at: view.schedule.last_fired_at,
            armed_at: view.schedule.armed_at,
            next_fire_at: view.next_fire_at,
            next_fire_error: view.next_fire_error,
        }
    }
}

impl From<Schedule> for ScheduleView {
    /// A row a write just returned, with no next fire time computed.
    ///
    /// `next_fire_at` is deliberately `None` here rather than calculated: this
    /// conversion has no clock, and inventing one would put a second answer to
    /// "when does this fire" beside `list_schedules`'. A caller that wants the
    /// time asks the list, which is the one place it is computed.
    fn from(schedule: Schedule) -> Self {
        Self::from(CoreScheduleView {
            schedule,
            next_fire_at: None,
            next_fire_error: None,
        })
    }
}

/// What `list_schedules` answers with.
///
/// An object wrapping the array, for the reason [`RepositoryListView`] gives:
/// MCP requires an object `outputSchema`, and a bare array makes Claude Code
/// drop the entire `tools/list` response — every other tool with it.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ScheduleListView {
    pub schedules: Vec<ScheduleView>,
}

/// What `delete_schedule` answers with.
///
/// An echo object rather than nothing at all, on [`OnboardingView`]'s
/// precedent: a tool that answers with no content gives a caller no way to tell
/// "it worked" from "the schema was wrong".
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ScheduleDeletedView {
    pub schedule_id: String,
    pub deleted: bool,
}

/// What `list_timezones` answers with — every IANA name the service accepts.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct TimezoneListView {
    pub timezones: Vec<String>,
}

/// What `preview_schedule_preflight` answers with: which tasks a schedule would
/// run, in what order, and which are blocked and why.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct PreflightView {
    pub schedule_id: String,
    pub schedule_name: String,
    pub next_fire_at: Option<DateTime<Utc>>,
    pub closes_at: Option<DateTime<Utc>>,
    pub mode: ScheduleMode,
    pub max_concurrency: i64,
    /// How many tasks the window will get through.
    pub will_start: usize,
    /// How many it will pass over — and therefore how many will still be
    /// sitting there in the morning.
    pub blocked: usize,
    /// Every `ready` task in board order, including the ones the queue will
    /// pass over, each carrying its reason. Filtering the skipped ones out
    /// would answer "which tasks will run" and silently drop "and which are
    /// blocked and why", which is the half that costs a night.
    pub plan: Vec<PreflightEntryView>,
}

/// One task in a [`PreflightView`].
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct PreflightEntryView {
    pub task_id: String,
    pub title: String,
    /// `1` is next up. `null` for a task the queue will pass over.
    pub queue_position: Option<i64>,
    /// `null` when the queue would start this task. Otherwise the reason, in
    /// the same words the board's own badge uses.
    pub skipped_because: Option<String>,
}

impl From<PreflightSummary> for PreflightView {
    fn from(summary: PreflightSummary) -> Self {
        Self {
            schedule_id: summary.schedule_id.clone(),
            schedule_name: summary.schedule_name.clone(),
            next_fire_at: summary.next_fire_at,
            closes_at: summary.closes_at,
            mode: summary.mode,
            max_concurrency: summary.max_concurrency,
            will_start: summary.startable(),
            blocked: summary.blocked(),
            plan: summary
                .plan
                .into_iter()
                .map(|entry| PreflightEntryView {
                    task_id: entry.task_id,
                    title: entry.title,
                    queue_position: entry.queue_position,
                    skipped_because: entry.skip.map(|skip| skip.explanation().to_string()),
                })
                .collect(),
        }
    }
}

/// One worktree, as `list_worktrees` reports it (task 016).
///
/// A projection for this module's usual reason — the core type is `camelCase`
/// for the frontend — and it drops two fields rather than mirroring: the
/// `repository_id` and `base_ref` an agent has no use for here, since it cannot
/// act on either through this surface. What it keeps is everything needed to
/// *explain* the disk: which task, how big, how long since anyone touched it,
/// and the three facts that decide whether removing it is safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct WorktreeView {
    pub task_id: String,
    pub task_title: String,
    pub repository_name: String,
    pub column: BoardColumn,
    pub run_state: RunState,
    pub path: String,
    /// The directory is on disk and git still lists it. `false` is a worktree
    /// deleted outside the app, which startup reconciliation clears.
    pub exists: bool,
    pub branch: Option<String>,
    pub size_bytes: u64,
    pub last_activity: Option<DateTime<Utc>>,
    pub merged: bool,
    pub uncommitted_changes: i64,
    pub unpushed_commits: i64,
    /// A run is working here, so nothing removes it — not even the window, and
    /// not with any confirmation.
    pub live: bool,
}

impl From<WorktreeInventoryEntry> for WorktreeView {
    fn from(entry: WorktreeInventoryEntry) -> Self {
        WorktreeView {
            task_id: entry.task_id,
            task_title: entry.task_title,
            repository_name: entry.repository_name,
            column: entry.column,
            run_state: entry.run_state,
            path: entry.path,
            exists: entry.exists,
            branch: entry.branch,
            size_bytes: entry.size_bytes,
            last_activity: entry.last_activity,
            merged: entry.merged,
            uncommitted_changes: entry.uncommitted_changes,
            unpushed_commits: entry.unpushed_commits,
            live: entry.live,
        }
    }
}

/// The list, wrapped — MCP requires an object output schema and Claude Code
/// refuses the *entire* `tools/list` response when one tool disagrees, which is
/// why [`RepositoryListView`] does the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct WorktreeListView {
    pub worktrees: Vec<WorktreeView>,
    pub total_bytes: u64,
}

/// The auto-removal policy, wrapped for [`StrategyApprovalView`]'s reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct WorktreeAutoCleanupView {
    pub setting: AutoCleanup,
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
            blocking_title: None,
            last_run: Some(LastRunSummary {
                status: RunStatus::Succeeded,
                exit_class: Some(ExitClass::Success),
                ended_at: None,
                resume_after: None,
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
            blocking_title: None,
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
            base_ref: None,
            model: None,
            effort: None,
            run_environment: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
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
