//! Which door a tool call arrived through, and what that door may do
//! (ADR-0006's 2026-08-28 amendment, seam-contract D17.4).
//!
//! Until task 020 this server had exactly one caller: the operator's own
//! planning session, reaching `/mcp`, allowed every tool on ADR-0006's table.
//! A run is a second caller with a far narrower job — it has one task, and its
//! entire business with Rimaia is that task — so it gets a second route,
//! `/mcp/run/{token}`, minted per run and revoked when the run ends. `/mcp`
//! itself is untouched: it is the URL the user pasted into `claude mcp add`,
//! ADR-0006 fixes it, and re-scoping it would break every registered session.
//!
//! # The token is not a secret, and is not meant to be
//!
//! It travels in argv, inside `--mcp-config`, so `ps` shows it to the same
//! user. That is not a widening. ADR-0006's trust boundary is already "anything
//! on this machine that can reach loopback", and ADR-0012 hands the run
//! arbitrary bash besides — a secret a process could read out of its own
//! process table protects nothing from that process.
//!
//! **The token's job is to stop the confused deputy.** The realistic failure is
//! a run that has been prompt-injected by a file it read, or is simply mistaken
//! about which card it is working on, addressing a task that is not its own.
//! Before this module the only thing standing between that run and someone
//! else's board was a task id in a sentence in the prompt, which the model may
//! or may not still be attending to twenty turns later. After it, the task id
//! is on the server value and the check is a function call.
//!
//! # One decision point
//!
//! [`RunScope::authorize`] is every handler's first statement, and the only
//! place a tool's availability is decided. `tests/mcp_scope.rs` requires every
//! *registered* tool to have an entry in [`Tool`], so a tool added later cannot
//! reach the wire without someone having said what a run may do with it.
//!
//! # A token says what it was minted for
//!
//! The planner, a review and a fix each hold a handle to their own task, and
//! each may do something different with it, so a token resolves to a task
//! *and* a [`Grant`], and [`Tool::run_access`] decides per grant
//! (seam-contract D30 point 5). The handle is served as `rimaia-run`, never as
//! the operator's `rimaia`, so the denial of the operator surface every run
//! carries never reaches it (D30 points 1 and 2).
//!
//! `tools/list` is deliberately **not** filtered by scope, so a run is offered
//! tools it will be refused. That is the price of there being one decision
//! point: a filtered advertisement would be a second copy of the table, free to
//! disagree with this one, and the disagreement would be invisible — a tool
//! quietly missing from a list is much harder to notice than a call that comes
//! back with a sentence saying why. A run that tries anyway is told, in words,
//! that it is scoped to one task.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::db::new_id;
use crate::error::{Error, Result};
use crate::events::TeamId;
use crate::mcp::MCP_PATH;

/// Where the scoped route hangs, relative to [`MCP_PATH`].
///
/// One constant, so the path axum registers and the URL a run is handed cannot
/// drift apart — the failure mode being a token that resolves fine and a route
/// that never sees it.
pub(crate) const RUN_ROUTE_PREFIX: &str = "/run/";

/// The scope a [`RimaiaServer`](crate::mcp::RimaiaServer) was reached through.
///
/// It lives on the *server value*, not on the request. That is the whole reason
/// the token is a path segment: `StreamableHttpService`'s service factory is
/// `Fn() -> Result<S, io::Error>` with no access to the request, so a
/// header-carried token would have to be pulled out of request extensions
/// inside each handler — a second parameter on every one of them, every
/// direct-call test rewritten, and the scope living somewhere a newly added
/// tool can silently forget to read (seam-contract D17.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunScope {
    /// `/mcp` — the operator's own session. ADR-0006's table in full, less the
    /// two tools only a run writes ([`Tool::is_run_output`]).
    Operator,
    /// `/mcp/run/{token}` — one unattended run, working on one task, for the
    /// purpose its grant names.
    Run { task_id: String, grant: Grant },
}

/// What a run-scoped token was minted for (seam-contract D30 point 5).
///
/// The planner, a review and a fix each hold a handle to their own task, and
/// what each may do with it differs: the planner may amend the plan it is
/// planning, a reviewer may only report, and a fixer may only resolve what was
/// reported. A grant that said only "this task" would hand review and fix runs
/// the planner's table, and a fixer rewriting the plan to match what it did is
/// marking its own homework.
///
/// Not [`RunGrant`], which is the token holder whose `Drop` revokes the token.
/// This is what the token was minted *for*, and the only one of the two an
/// access decision reads. A review or fix carries its own run id, so what it
/// writes is attributed to the run that holds the token and never to an id a
/// request could name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    Strategy,
    Review { run_id: String },
    Fix { run_id: String },
}

/// [`Grant`]'s discriminant, which is all [`Tool::run_access`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GrantKind {
    Strategy,
    Review,
    Fix,
}

impl GrantKind {
    pub const ALL: [GrantKind; 3] = [GrantKind::Strategy, GrantKind::Review, GrantKind::Fix];
}

impl Grant {
    pub fn kind(&self) -> GrantKind {
        match self {
            Grant::Strategy => GrantKind::Strategy,
            Grant::Review { .. } => GrantKind::Review,
            Grant::Fix { .. } => GrantKind::Fix,
        }
    }

    /// The run a review or fix grant was minted for; `None` for the planner,
    /// which has no `runs` row (D17.5).
    pub fn run_id(&self) -> Option<&str> {
        match self {
            Grant::Strategy => None,
            Grant::Review { run_id } | Grant::Fix { run_id } => Some(run_id),
        }
    }
}

/// Every tool this server registers (ADR-0021: the set is open, the scope
/// decision is not optional).
///
/// An enum rather than a string, for the reason every other closed set in this
/// crate is one: a typo in `"remove_task_link"` inside an allow table is a hole
/// that compiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    AddTaskLink,
    CreateTask,
    GetBaseInstructions,
    GetTask,
    ListRepositories,
    ListTasks,
    MoveTask,
    RemoveTaskLink,
    SetTaskDependencies,
    /// Task 020's eleventh tool. Its decision is recorded here ahead of the
    /// handler because the decision is ADR-0006's to make, not the handler's.
    SetTaskStrategy,
    UpdateTask,

    // ADR-0021's capability parity. Each of these had a Tauri command and no
    // tool, which is the asymmetry that ADR exists to end.
    AcceptTaskStrategy,
    ClearTaskStrategy,
    GetStrategyApproval,
    GetStrategyCatalogue,
    GetStrategyDefaults,
    SetStrategyApproval,
    SetStrategyCatalogue,
    SetStrategyDefaults,

    // Task 012's four. Same argument, one layer out: these reconfigure how
    // many runs the installation will start at once, rather than how any one
    // of them is spawned.
    GetRunCapacity,
    SetScheduleMode,
    SetMaxConcurrency,
    SetRepositoryMaxConcurrency,

    /// Task 066's. This machine's clone of each repository, which
    /// `list_repositories` carried until no board DTO held a path.
    ListCheckouts,

    /// Task 014's one. `retry_task_now` is deliberately **not** here — see
    /// `run_access` and seam-contract D23.
    GiveUpOnTask,

    // Task 018. Both inspect or reconfigure *this installation* rather than any
    // task, which is ADR-0021 point 4's second permanent refusal.
    RunDoctor,
    DismissOnboarding,

    // Task 027's two, and they are the same refusal with a sharper edge than
    // most — see `run_access`.
    DismissDoctorWarning,
    RestoreDoctorWarning,

    // Task 023's two, and they are what closes ADR-0021's *named* gap rather
    // than another instance of its rule — see `run_access`.
    PlanTaskStrategy,
    PlanTasksStrategy,

    /// Task 022's one. The *write* pair has no tool at all — see `run_access`
    /// and seam-contract D25.
    GetRepositoryCredentialStatus,

    // Task 024's three. Reads and reconfigures the installation.
    GetAnalytics,
    GetSubscriptionCost,
    SetSubscriptionCost,

    // Task 013's seven. Every one is *both* of ADR-0021 point 4's permanent
    // refusals at once: a schedule spawns runs — it is the thing that starts
    // the queue at 22:00 — and it reconfigures the installation, since an open
    // window overrides the mode and concurrency the whole queue runs under.
    // `list_timezones` rides with them rather than being `Unscoped` on the
    // grounds that it reads nothing: it exists only to fill in a field of the
    // four tools above it, so a run that may not use those has no use for it,
    // and a surface is easier to reason about when a feature is in or out
    // whole.
    ListSchedules,
    CreateSchedule,
    UpdateSchedule,
    SetScheduleEnabled,
    DeleteSchedule,
    PreviewSchedulePreflight,
    ListTimezones,
    // Task 016. The *reads* and the policy setting only — the three cleanup
    // commands that actually delete a worktree have no tool at all, on
    // ADR-0021 point 5's `delete_task` reasoning. Seam-contract D20 records it.
    ListWorktrees,
    GetWorktreeAutoCleanup,
    SetWorktreeAutoCleanup,
    // Task 030. ADR-0025 point 8: archiving is *reversible*, which is the
    // property ADR-0021 point 5's `delete_task` exception is drawn along, so
    // these do get tools where the three cleanup commands above do not.
    ArchiveTask,
    ArchiveTasks,
    UnarchiveTask,
    SetRepositoryOnArchive,

    // Task 034. The three verdicts a morning review ends in, the two reads that
    // inform them, and the digest marker. All six are refused to a run — see
    // `run_access`.
    ApproveTask,
    RejectTask,
    RequestTaskChanges,
    GetTaskDependents,
    GetReviewDigest,
    MarkReviewDigestSeen,

    // Task 035. The findings a review run reports and a fix run resolves, and
    // the operator's read of them. The two writes are a run's output and are
    // refused to the operator — see `is_run_output`.
    RecordReviewFindings,
    ResolveReviewFinding,
    ListReviewFindings,

    // Task 021. The review loop's configuration, at three levels. All four
    // are refused to a run — see `run_access`.
    GetReviewSettings,
    SetReviewSettings,
    SetRepositoryReviewConfig,
    SetTaskReview,

    // Task 037. The operator's read of a task's review loops. Refused to a
    // run, as `ListReviewFindings` is: a fix run is handed its findings in its
    // prompt, and a review's history is not its own to read.
    GetReviewHistory,
    // Read with the other review configuration: refused to every run.
    GetReviewLevel,

    // Task 045. Assignment, acceptance, the team ceiling and the consent read
    // (ADR-0032). All four are refused to a run — see `run_access`.
    AssignTask,
    AcceptContent,
    SetRepositoryUnattendedCeiling,
    GetTaskConsent,

    // Task 072. This runner's strategy ceiling, a runner setting (ADR-0032
    // point 3). Both are refused to a run — see `run_access`.
    GetStrategyCeiling,
    SetStrategyCeiling,
}

/// What a [`RunScope::Run`] may do with one tool — ADR-0006's amendment table,
/// as a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAccess {
    /// Allowed, and only against the task the token was minted for.
    OwnTaskOnly,
    /// Allowed. There is no task to scope it to.
    Unscoped,
    /// Refused outright.
    Refused,
}

impl Tool {
    /// Every tool with a recorded decision, so a test can walk the table.
    pub const ALL: [Tool; 70] = [
        Tool::AddTaskLink,
        Tool::CreateTask,
        Tool::GetBaseInstructions,
        Tool::GetTask,
        Tool::ListRepositories,
        Tool::ListTasks,
        Tool::MoveTask,
        Tool::RemoveTaskLink,
        Tool::SetTaskDependencies,
        Tool::SetTaskStrategy,
        Tool::UpdateTask,
        Tool::AcceptTaskStrategy,
        Tool::ClearTaskStrategy,
        Tool::GetStrategyApproval,
        Tool::GetStrategyCatalogue,
        Tool::GetStrategyDefaults,
        Tool::SetStrategyApproval,
        Tool::SetStrategyCatalogue,
        Tool::SetStrategyDefaults,
        Tool::GetRunCapacity,
        Tool::SetScheduleMode,
        Tool::SetMaxConcurrency,
        Tool::SetRepositoryMaxConcurrency,
        Tool::ListCheckouts,
        Tool::GiveUpOnTask,
        Tool::RunDoctor,
        Tool::DismissOnboarding,
        Tool::DismissDoctorWarning,
        Tool::RestoreDoctorWarning,
        Tool::PlanTaskStrategy,
        Tool::PlanTasksStrategy,
        Tool::GetRepositoryCredentialStatus,
        Tool::GetAnalytics,
        Tool::GetSubscriptionCost,
        Tool::SetSubscriptionCost,
        Tool::ListSchedules,
        Tool::CreateSchedule,
        Tool::UpdateSchedule,
        Tool::SetScheduleEnabled,
        Tool::DeleteSchedule,
        Tool::PreviewSchedulePreflight,
        Tool::ListTimezones,
        Tool::ListWorktrees,
        Tool::GetWorktreeAutoCleanup,
        Tool::SetWorktreeAutoCleanup,
        Tool::ArchiveTask,
        Tool::ArchiveTasks,
        Tool::UnarchiveTask,
        Tool::SetRepositoryOnArchive,
        Tool::ApproveTask,
        Tool::RejectTask,
        Tool::RequestTaskChanges,
        Tool::GetTaskDependents,
        Tool::GetReviewDigest,
        Tool::MarkReviewDigestSeen,
        Tool::RecordReviewFindings,
        Tool::ResolveReviewFinding,
        Tool::ListReviewFindings,
        Tool::GetReviewSettings,
        Tool::SetReviewSettings,
        Tool::SetRepositoryReviewConfig,
        Tool::SetTaskReview,
        Tool::GetReviewHistory,
        Tool::GetReviewLevel,
        Tool::AssignTask,
        Tool::AcceptContent,
        Tool::SetRepositoryUnattendedCeiling,
        Tool::GetTaskConsent,
        Tool::GetStrategyCeiling,
        Tool::SetStrategyCeiling,
    ];

    /// The wired name — what `tools/list` advertises and what the ADR table
    /// calls it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Tool::AddTaskLink => "add_task_link",
            Tool::CreateTask => "create_task",
            Tool::GetBaseInstructions => "get_base_instructions",
            Tool::GetTask => "get_task",
            Tool::ListRepositories => "list_repositories",
            Tool::ListTasks => "list_tasks",
            Tool::MoveTask => "move_task",
            Tool::RemoveTaskLink => "remove_task_link",
            Tool::SetTaskDependencies => "set_task_dependencies",
            Tool::SetTaskStrategy => "set_task_strategy",
            Tool::UpdateTask => "update_task",
            Tool::AcceptTaskStrategy => "accept_task_strategy",
            Tool::ClearTaskStrategy => "clear_task_strategy",
            Tool::GetStrategyApproval => "get_strategy_approval",
            Tool::GetStrategyCatalogue => "get_strategy_catalogue",
            Tool::GetStrategyDefaults => "get_strategy_defaults",
            Tool::SetStrategyApproval => "set_strategy_approval",
            Tool::SetStrategyCatalogue => "set_strategy_catalogue",
            Tool::SetStrategyDefaults => "set_strategy_defaults",
            Tool::GetRunCapacity => "get_run_capacity",
            Tool::SetScheduleMode => "set_schedule_mode",
            Tool::SetMaxConcurrency => "set_max_concurrency",
            Tool::SetRepositoryMaxConcurrency => "set_repository_max_concurrency",
            Tool::ListCheckouts => "list_checkouts",
            Tool::GiveUpOnTask => "give_up_on_task",
            Tool::RunDoctor => "run_doctor",
            Tool::DismissOnboarding => "dismiss_onboarding",
            Tool::DismissDoctorWarning => "dismiss_doctor_warning",
            Tool::RestoreDoctorWarning => "restore_doctor_warning",
            Tool::PlanTaskStrategy => "plan_task_strategy",
            Tool::PlanTasksStrategy => "plan_tasks_strategy",
            Tool::GetRepositoryCredentialStatus => "get_repository_credential_status",
            Tool::GetAnalytics => "get_analytics",
            Tool::GetSubscriptionCost => "get_subscription_cost",
            Tool::SetSubscriptionCost => "set_subscription_cost",
            Tool::ListSchedules => "list_schedules",
            Tool::CreateSchedule => "create_schedule",
            Tool::UpdateSchedule => "update_schedule",
            Tool::SetScheduleEnabled => "set_schedule_enabled",
            Tool::DeleteSchedule => "delete_schedule",
            Tool::PreviewSchedulePreflight => "preview_schedule_preflight",
            Tool::ListTimezones => "list_timezones",
            Tool::ListWorktrees => "list_worktrees",
            Tool::GetWorktreeAutoCleanup => "get_worktree_auto_cleanup",
            Tool::SetWorktreeAutoCleanup => "set_worktree_auto_cleanup",
            Tool::ArchiveTask => "archive_task",
            Tool::ArchiveTasks => "archive_tasks",
            Tool::UnarchiveTask => "unarchive_task",
            Tool::SetRepositoryOnArchive => "set_repository_on_archive",
            Tool::ApproveTask => "approve_task",
            Tool::RejectTask => "reject_task",
            Tool::RequestTaskChanges => "request_task_changes",
            Tool::GetTaskDependents => "get_task_dependents",
            Tool::GetReviewDigest => "get_review_digest",
            Tool::MarkReviewDigestSeen => "mark_review_digest_seen",
            Tool::RecordReviewFindings => "record_review_findings",
            Tool::ResolveReviewFinding => "resolve_review_finding",
            Tool::ListReviewFindings => "list_review_findings",
            Tool::GetReviewSettings => "get_review_settings",
            Tool::SetReviewSettings => "set_review_settings",
            Tool::SetRepositoryReviewConfig => "set_repository_review_config",
            Tool::SetTaskReview => "set_task_review",
            Tool::GetReviewHistory => "get_review_history",
            Tool::GetReviewLevel => "get_review_level",
            Tool::AssignTask => "assign_task",
            Tool::AcceptContent => "accept_content",
            Tool::SetRepositoryUnattendedCeiling => "set_repository_unattended_ceiling",
            Tool::GetTaskConsent => "get_task_consent",
            Tool::GetStrategyCeiling => "get_strategy_ceiling",
            Tool::SetStrategyCeiling => "set_strategy_ceiling",
        }
    }

    /// The decision for a name off the wire, or `None` when nobody has taken
    /// one.
    ///
    /// `tests/mcp_scope.rs` is where `None` costs something: a registered tool
    /// that lands here is a tool whose run-scope decision was never made.
    pub fn from_name(name: &str) -> Option<Self> {
        Tool::ALL.into_iter().find(|tool| tool.as_str() == name)
    }

    /// Whether this tool writes something only a run produces.
    ///
    /// [`RunScope::authorize`] refuses these on the operator's door, the first
    /// thing that door has ever been refused: a finding the operator wrote
    /// would look exactly like a reviewer's in the morning (D30 point 5). No UI
    /// command writes a finding either, so ADR-0021's parity is not affected.
    pub const fn is_run_output(self) -> bool {
        matches!(
            self,
            Tool::RecordReviewFindings | Tool::ResolveReviewFinding
        )
    }

    /// ADR-0006's amendment table as D30 point 5 extends it per grant, and the
    /// only copy of it in code.
    pub const fn run_access(self, grant: GrantKind) -> RunAccess {
        match self {
            // Every run may read the card it was started for.
            Tool::GetTask => RunAccess::OwnTaskOnly,

            // The planner may amend the card it is planning, as task 020 let
            // it. A reviewer or a fixer may not: a fixer rewriting the plan to
            // match what it did, or a reviewer choosing the next model, is
            // marking its own homework (D30 point 5).
            Tool::AddTaskLink | Tool::RemoveTaskLink | Tool::SetTaskStrategy | Tool::UpdateTask => {
                match grant {
                    GrantKind::Strategy => RunAccess::OwnTaskOnly,
                    GrantKind::Review | GrantKind::Fix => RunAccess::Refused,
                }
            }

            // Each loop run writes exactly its own output, against its own
            // task, under the run id its grant carries.
            Tool::RecordReviewFindings => match grant {
                GrantKind::Review => RunAccess::OwnTaskOnly,
                GrantKind::Strategy | GrantKind::Fix => RunAccess::Refused,
            },
            Tool::ResolveReviewFinding => match grant {
                GrantKind::Fix => RunAccess::OwnTaskOnly,
                GrantKind::Strategy | GrantKind::Review => RunAccess::Refused,
            },
            // D30's "everything else" row, applied as written. A fix run is
            // handed its findings in its prompt (task 021) and does not go
            // looking for them.
            Tool::ListReviewFindings => RunAccess::Refused,

            // Task 037, D30's "everything else" row again: the history is the
            // operator's read of what the loop could not fix, and it holds
            // every finding of every loop, not only the run's own.
            Tool::GetReviewHistory => RunAccess::Refused,

            // Neither takes a task, and a run has a legitimate use for both:
            // the standing instructions it is working under, and the names of
            // the repositories it may be looking at.
            Tool::GetBaseInstructions | Tool::ListRepositories => RunAccess::Unscoped,

            // `move_task` because the runner owns where a card lands when a run
            // finishes, and a run moving its own card to `done` would be
            // marking its own homework. `list_tasks` because a run has no
            // business enumerating someone's board. `create_task` and
            // `set_task_dependencies` because a run spawning or reordering work
            // is orchestration, which ADR-0016 declines to build.
            Tool::CreateTask | Tool::ListTasks | Tool::MoveTask | Tool::SetTaskDependencies => {
                RunAccess::Refused
            }

            // ADR-0021 point 4's second permanent refusal: these reconfigure
            // the installation. A run editing the settings
            // that govern runs — which model the planner uses, what a
            // repository defaults to, whether a proposal needs approval — is a
            // loop nobody asked for, and it would let one task's agent change
            // what every later task costs.
            //
            // `accept_task_strategy` is here rather than under `OwnTaskOnly`
            // for a subtler reason: accepting flips `strategy_source` from
            // `planner` to `user`, and a planner accepting its own proposal is
            // the card claiming a human signed off on it. That is the one thing
            // the field exists to distinguish.
            Tool::AcceptTaskStrategy
            | Tool::ClearTaskStrategy
            | Tool::GetStrategyApproval
            | Tool::GetStrategyCatalogue
            | Tool::GetStrategyDefaults
            | Tool::SetStrategyApproval
            | Tool::SetStrategyCatalogue
            | Tool::SetStrategyDefaults
            // Task 012's four, and the decision needs no new argument — it is
            // the same permanent refusal one layer out. How many runs this
            // installation starts at once, and how many of them one repository
            // will hold, are properties of the *run configuration* (ADR-0010),
            // which is exactly what point 4 names. A run raising the limit is a
            // run deciding how much the night costs; a run raising its own
            // repository's cap is a run turning off the thing that keeps a
            // second agent out of its ports and test databases. The read is
            // refused with the writes rather than allowed alongside
            // `get_base_instructions`, because a run has no use for the answer:
            // it cannot act on it, and it would only be useful for deciding
            // whether the write is worth attempting.
            | Tool::GetRunCapacity
            | Tool::SetScheduleMode
            | Tool::SetMaxConcurrency
            | Tool::SetRepositoryMaxConcurrency
            // Task 066's, refused with the caps it reports: where every clone
            // lives on this machine, each one's cap and this runner's consent
            // are the same run configuration, and the paths are the
            // reconnaissance `run_doctor` is refused for below.
            | Tool::ListCheckouts
            // Task 014's, and it is the *first* kind of permanent refusal
            // ADR-0021 point 4 names rather than the second: giving up on a
            // task ends a retry loop, and a run that could end its own would
            // be marking its own homework in the other direction — abandoning
            // the work it was started to do and reporting it as settled. The
            // operator endpoint keeps it in full.
            | Tool::GiveUpOnTask => RunAccess::Refused,

            // Task 018, and the same clause of ADR-0021 point 4 read one step
            // wider: these are about the *installation*, not about any task.
            // `run_doctor` reports which binaries are on the operator's PATH,
            // whether they are signed in, and where every registered repository
            // lives on disk — a reconnaissance surface a run has no business
            // reading, and one whose only actionable remediations are things
            // only a human standing at the machine can do. `dismiss_onboarding`
            // writes a preference about the operator's own window, which is
            // nothing a run inside a worktree has an opinion about.
            //
            // Task 027's two are the same clause with the sharpest edge on the
            // table: a run that could dismiss a doctor warning could silence
            // the report on the environment it is itself running in, and the
            // next night's operator would read a clean panel about a machine
            // that is not. `restore_doctor_warning` is refused with it rather
            // than allowed as a harmless un-hide, because the pair is one
            // feature and a surface is easier to reason about when a feature is
            // in or out whole — the argument task 013's seven already make.
            Tool::RunDoctor
            | Tool::DismissOnboarding
            | Tool::DismissDoctorWarning
            | Tool::RestoreDoctorWarning => RunAccess::Refused,

            // Task 013's seven. See the enum for why every one of them is
            // refused rather than only the four that write.
            Tool::ListSchedules
            | Tool::CreateSchedule
            | Tool::UpdateSchedule
            | Tool::SetScheduleEnabled
            | Tool::DeleteSchedule
            | Tool::PreviewSchedulePreflight
            | Tool::ListTimezones => RunAccess::Refused,

            // Task 016, and the same ADR-0021 point 4 clause: worktrees are
            // installation state. `set_worktree_auto_cleanup` reconfigures what
            // every *later* task's directory is worth, which is the loop that
            // clause names. `list_worktrees` and `get_worktree_auto_cleanup`
            // are refused for a narrower reason of their own — a run's
            // entitlement is its own task, and an inventory is by construction
            // an enumeration of everybody else's, exactly `list_tasks`'s
            // objection. A run has no business knowing what else is on the
            // disk, still less that its own directory is the one due to be
            // reclaimed.
            Tool::ListWorktrees | Tool::GetWorktreeAutoCleanup | Tool::SetWorktreeAutoCleanup => {
                RunAccess::Refused
            }

            // Task 030, and refused on both of ADR-0021 point 4's clauses at
            // once rather than on `delete_task`'s destructiveness ground —
            // which is why these have tools at all (ADR-0025 point 8) while the
            // three cleanup commands above have none.
            //
            // `set_repository_on_archive` is "reconfigures the installation"
            // verbatim: it decides what every *later* archive in that
            // repository deletes, and the `script` mode decides which program
            // Rimaia will execute. The three archive calls are refused on the
            // narrower ground that archiving is not a board edit — it fires
            // whatever the repository configured, and a run-scoped agent can
            // reach its own card, so `OwnTaskOnly` would be a run able to
            // delete the worktree it is standing in.
            Tool::ArchiveTask
            | Tool::ArchiveTasks
            | Tool::UnarchiveTask
            | Tool::SetRepositoryOnArchive => RunAccess::Refused,

            // Task 023's two, and this is the arm ADR-0021 point 4's *first*
            // permanent refusal was written for: both spawn a `claude`
            // process. `plan_task_strategy` was left off the tool surface
            // entirely until now — not because the decision was hard, but
            // because "is this task already in flight" lived in `src-tauri`
            // and the server could not reach it (seam-contract D19 moved it).
            // The decision itself was never in doubt: a run that could spawn
            // planners could spend the night's budget on deciding rather than
            // doing, and `plan_tasks_strategy` could do it N times in one call.
            Tool::PlanTaskStrategy | Tool::PlanTasksStrategy => RunAccess::Refused,

            // Task 024's three, and it is ADR-0021 point 4's second permanent
            // refusal read the way `run_doctor` reads it: these describe and
            // configure the *installation*. `get_analytics` in particular is
            // an inventory of every task this machine has ever attempted and
            // what each one cost — `list_tasks`'s objection, with a price list
            // attached — and the subscription figure is a fact about the
            // operator's own billing that no run has a use for.
            Tool::GetAnalytics | Tool::GetSubscriptionCost | Tool::SetSubscriptionCost => {
                RunAccess::Refused
            }

            // Task 022's one, and the same clause again: whether a repository
            // has its own forge token, and whose it is, is a fact about the
            // *installation's* access, not about any task. It carries the
            // login, the label and the date and never the secret — but a run
            // that could enumerate which repositories carry credentials and
            // which account they belong to has been handed a map of the
            // operator's access for no use it has.
            Tool::GetRepositoryCredentialStatus => RunAccess::Refused,

            // Task 034 (ADR-0021 point 3). Approve, reject and request changes
            // are a run deciding a review, which is a run marking its own
            // homework (D30 point 5). The digest and dependents reads
            // enumerate other tasks, which is D16.6's objection and the reason
            // `list_worktrees` is refused. The marker write reconfigures what
            // the installation shows its operator (ADR-0021 point 4).
            Tool::ApproveTask
            | Tool::RejectTask
            | Tool::RequestTaskChanges
            | Tool::GetTaskDependents
            | Tool::GetReviewDigest
            | Tool::MarkReviewDigestSeen => RunAccess::Refused,

            // Task 021, and ADR-0021 point 4's "reconfigures the installation"
            // at three levels. A run that could enable its own loop would be
            // spending on its own authority, and a fixer that could rewrite its
            // own review instructions would be marking its own homework. The
            // read goes with the writes: a run has no use for the answer but
            // deciding whether a write is worth attempting.
            Tool::GetReviewSettings
            | Tool::GetReviewLevel
            | Tool::SetReviewSettings
            | Tool::SetRepositoryReviewConfig
            | Tool::SetTaskReview => RunAccess::Refused,

            // Task 045 (ADR-0032), and each speaks for a person. `accept_content`
            // is the one that matters: a run that could accept would launder
            // consent through its own handle, its owner's acceptance of content
            // the run itself just wrote. Assigning chooses whose machine runs a
            // card, the ceiling is a team owner's decision about every runner,
            // and the consent read is a person's own record, which a run has no
            // use for but deciding what to accept.
            Tool::AssignTask
            | Tool::AcceptContent
            | Tool::SetRepositoryUnattendedCeiling
            | Tool::GetTaskConsent => RunAccess::Refused,

            // Task 072, ADR-0021 point 4's second permanent refusal verbatim:
            // the strategy ceiling is the run configuration of this machine. A
            // run that raised it would be deciding what its own owner's
            // subscription pays for, and one that lowered it would refuse every
            // later run on the machine. The read is refused with the write, as
            // `get_run_capacity` is: a run cannot act on the answer.
            Tool::GetStrategyCeiling | Tool::SetStrategyCeiling => RunAccess::Refused,
        }
    }
}

impl RunScope {
    /// The run a review or fix handle was minted for. `None` on the
    /// operator's door and for the planner.
    pub fn run_id(&self) -> Option<&str> {
        match self {
            RunScope::Operator => None,
            RunScope::Run { grant, .. } => grant.run_id(),
        }
    }

    /// The single decision point. **Every handler's first statement.**
    ///
    /// `target_task_id` is the task the call would touch — `None` for the two
    /// tools that take none. A handler whose task id is not in the request
    /// resolves it first and authorizes against the answer;
    /// `remove_task_link` is the one that does.
    ///
    /// The refusal is a plain [`Error::Invalid`], so it reaches the caller as
    /// the same `{ code, message }` payload as every other refusal on either
    /// door (`mcp::error`). A scope check that invented its own shape would be
    /// the one refusal an agent could not handle like the rest.
    pub fn authorize(&self, tool: Tool, target_task_id: Option<&str>) -> Result<()> {
        let RunScope::Run { task_id, grant } = self else {
            // The operator's door is ADR-0006's table in full, less what only
            // a run may write.
            if tool.is_run_output() {
                return Err(Error::invalid(format!(
                    "{tool} is not available here: only the run a finding belongs to writes it, \
                     through its own run-scoped handle.",
                    tool = tool.as_str(),
                )));
            }
            return Ok(());
        };

        match tool.run_access(grant.kind()) {
            RunAccess::Unscoped => Ok(()),

            RunAccess::Refused => Err(Error::invalid(format!(
                "{tool} is not available to a run: this handle is scoped to task {task_id}, and a \
                 run may only read and amend its own task.",
                tool = tool.as_str(),
            ))),

            RunAccess::OwnTaskOnly => match target_task_id {
                Some(target) if target == task_id => Ok(()),
                Some(target) => Err(Error::invalid(format!(
                    "this handle is scoped to task {task_id}, so {tool} cannot be called against \
                     task {target}.",
                    tool = tool.as_str(),
                ))),
                // Not reachable from a request: a tool that is scoped to a task
                // always has one to name by the time it authorizes. It is a
                // wiring mistake, so it reads as one rather than as a refusal
                // the agent could act on.
                None => Err(Error::internal(format!(
                    "{tool} was authorized with no task to scope it to",
                    tool = tool.as_str(),
                ))),
            },
        }
    }
}

/// The live run-scoped endpoints: where the server is listening, and which
/// token means which task.
///
/// Cheap to clone; every clone mints, resolves and revokes against the same
/// table. The shell builds one before either subsystem and hands it to both,
/// which is what removes the ordering constraint between `rimaia_runner::queue::build` and
/// [`mcp::build`](crate::mcp::build) — neither has to exist before the other
/// for the runner to have somewhere to mint tokens.
#[derive(Clone, Default)]
pub struct RunHandles {
    shared: Arc<Mutex<Table>>,
}

#[derive(Default)]
struct Table {
    /// `http://127.0.0.1:4517` — origin only. The path is this module's
    /// business, and `None` means nothing is listening.
    endpoint: Option<String>,
    /// Token → what it was minted for. One entry per live [`RunGrant`].
    granted: HashMap<String, Granted>,
}

/// One live token's entry: the task, the task's team and the grant.
struct Granted {
    task_id: String,
    /// Read from the task's row by whoever minted the grant, so the scoped
    /// route serves every call under a context of this one team, whatever the
    /// runner serving it can reach (ADR-0029 point 5).
    team_id: TeamId,
    grant: Grant,
}

impl RunHandles {
    /// Records where the server is actually listening, or that it is not.
    ///
    /// Called by [`mcp::build`](crate::mcp::build) on **every** bind, including
    /// the rebind `set_mcp_port` performs at runtime. Shared mutable state
    /// rather than a URL copied once at startup precisely because of that
    /// rebind: a captured URL goes stale the moment the operator changes the
    /// port, and the next run would be handed an endpoint nothing answers.
    pub fn set_endpoint(&self, base_url: Option<String>) {
        self.lock().endpoint = base_url;
    }

    /// Where the server is listening, or `None` when nothing is.
    pub fn endpoint(&self) -> Option<String> {
        self.lock().endpoint.clone()
    }

    /// Mints a token for one run, for the purpose `grant` names, valid until
    /// the returned [`RunGrant`] is dropped.
    ///
    /// `team_id` is the task's own team, from the task's row: the route serves
    /// the handle under a context scoped to that one team, so a handle never
    /// sees more than one team even on a runner that serves several.
    ///
    /// A hyphenated v4 UUID, like every other id in this app. Unguessable by
    /// accident rather than by an adversary — see this module's header on what
    /// the token is for.
    pub fn grant(&self, task_id: &str, team_id: &str, grant: Grant) -> RunGrant {
        let token = new_id();
        self.lock().granted.insert(
            token.clone(),
            Granted {
                task_id: task_id.to_string(),
                team_id: team_id.to_string(),
                grant,
            },
        );

        RunGrant {
            token,
            task_id: task_id.to_string(),
            handles: self.clone(),
        }
    }

    /// The scope a token names, or `None` when it is unknown or revoked.
    ///
    /// The two are deliberately indistinguishable: the route answers both with
    /// a bare 404, so this surface is not an oracle for which tokens exist.
    pub fn resolve(&self, token: &str) -> Option<RunScope> {
        self.resolve_with_team(token).map(|(scope, _)| scope)
    }

    /// [`resolve`](Self::resolve), with the team the scoped route narrows its
    /// context to.
    pub fn resolve_with_team(&self, token: &str) -> Option<(RunScope, TeamId)> {
        self.lock().granted.get(token).map(|granted| {
            (
                RunScope::Run {
                    task_id: granted.task_id.clone(),
                    grant: granted.grant.clone(),
                },
                granted.team_id.clone(),
            )
        })
    }

    /// The scoped URL a run holding `grant` reaches Rimaia on, or `None` when
    /// nothing is listening.
    ///
    /// **A URL, not a document** (ADR-0026 point 2, seam-contract D27.4). Minting
    /// the address is Rimaia's; shaping it into something an agent can call — an
    /// inline JSON config, a file in a config home, an environment variable — is
    /// the provider's, because those are not the same thing on two providers and
    /// a `String` of one provider's JSON cannot become the other.
    ///
    /// It takes the grant rather than a bare token so that a URL can only be
    /// built by whoever holds the grant, which is the same thing as saying it
    /// cannot outlive the token it names. `None` has exactly one cause — no
    /// endpoint bound, the busy-port case seam-contract D16.7 makes non-fatal —
    /// so the caller can say "see Settings → MCP" without qualifying it.
    pub fn endpoint_for(&self, grant: &RunGrant) -> Option<String> {
        let endpoint = self.endpoint()?;
        Some(format!(
            "{endpoint}{MCP_PATH}{RUN_ROUTE_PREFIX}{token}",
            token = grant.token
        ))
    }

    fn revoke(&self, token: &str) {
        self.lock().granted.remove(token);
    }

    /// `std::sync::Mutex` rather than tokio's, for the reason
    /// the runner loop's `Shared` gives at its own: it is only ever held
    /// across a hash-map operation, never across an `await`. A poisoned lock is
    /// recovered rather than propagated — a panic somewhere else must not turn
    /// every later run's handle into a panic of its own.
    fn lock(&self) -> MutexGuard<'_, Table> {
        self.shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One run's live token. **Dropping it revokes the token**, so a cancelled or
/// panicking run cannot leave a live handle to a task behind.
///
/// Deliberately not `Clone`: with two owners, revocation would have to happen
/// on the first drop or the last, and neither is a rule worth having. One run,
/// one grant, and the compiler says so.
pub struct RunGrant {
    token: String,
    task_id: String,
    handles: RunHandles,
}

impl RunGrant {
    /// The minted token. Mostly for a test that wants to assert the URL a run
    /// was handed; the runner itself should ask for
    /// [`endpoint_for`](RunHandles::endpoint_for).
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The task this grant is scoped to.
    pub fn task_id(&self) -> &str {
        &self.task_id
    }
}

impl Drop for RunGrant {
    fn drop(&mut self) {
        self.handles.revoke(&self.token);
    }
}

impl std::fmt::Debug for RunGrant {
    /// Hand-written to keep the token out of a log line. It is not a secret,
    /// but it is also not something a `?` on a struct holding a grant should
    /// print by accident.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunGrant")
            .field("task_id", &self.task_id)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for RunHandles {
    /// Hand-written for [`RunGrant`]'s reason, one level up: `RunnerConfig`
    /// derives `Debug` and holds one of these, so a `?` on the runner's config
    /// would otherwise print every live token. The count is the part that is
    /// useful in a log line; the tokens are the part that is not.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let table = self.lock();
        f.debug_struct("RunHandles")
            .field("endpoint", &table.endpoint)
            .field("granted", &table.granted.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const TEAM: &str = "3f2b1c00-0000-4000-8000-0000000000a1";

    fn bound() -> RunHandles {
        let handles = RunHandles::default();
        handles.set_endpoint(Some("http://127.0.0.1:4517".to_string()));
        handles
    }

    #[test]
    fn a_granted_token_resolves_to_its_own_task() {
        let handles = bound();

        let grant = handles.grant("task-1", TEAM, Grant::Strategy);

        assert_eq!(
            handles.resolve(grant.token()),
            Some(RunScope::Run {
                task_id: "task-1".to_string(),
                grant: Grant::Strategy,
            })
        );
        assert_eq!(handles.resolve("not-a-token"), None);
    }

    #[test]
    fn a_token_resolves_to_the_grant_it_was_minted_for() {
        let handles = bound();

        let review = handles.grant(
            "task-1",
            TEAM,
            Grant::Review {
                run_id: "run-7".to_string(),
            },
        );

        let scope = handles.resolve(review.token()).expect("a live token");
        assert_eq!(scope.run_id(), Some("run-7"));
        assert_eq!(
            scope,
            RunScope::Run {
                task_id: "task-1".to_string(),
                grant: Grant::Review {
                    run_id: "run-7".to_string()
                },
            }
        );
        assert_eq!(RunScope::Operator.run_id(), None);
    }

    #[test]
    fn dropping_a_grant_revokes_its_token() {
        // The RAII half of seam-contract D17.4: a run that is cancelled or
        // panics unwinds through this, so there is no path that leaves a live
        // handle to a task behind.
        let handles = bound();
        let token = {
            let grant = handles.grant("task-1", TEAM, Grant::Strategy);
            grant.token().to_string()
        };

        assert_eq!(handles.resolve(&token), None);
    }

    #[test]
    fn two_runs_get_two_tokens_for_the_same_task() {
        // Task 012's parallel queue is coming, and a token that collided would
        // be revoked by whichever run finished first.
        let handles = bound();

        let first = handles.grant("task-1", TEAM, Grant::Strategy);
        let second = handles.grant("task-1", TEAM, Grant::Strategy);

        assert_ne!(first.token(), second.token());
        assert!(handles.resolve(first.token()).is_some());
        assert!(handles.resolve(second.token()).is_some());
    }

    #[test]
    fn the_scoped_endpoint_names_the_bound_port_and_the_run_s_own_token() {
        let handles = bound();
        let grant = handles.grant("task-1", TEAM, Grant::Strategy);

        assert_eq!(
            handles.endpoint_for(&grant),
            Some(format!(
                "http://127.0.0.1:4517/mcp/run/{token}",
                token = grant.token()
            ))
        );
    }

    #[test]
    fn an_unbound_endpoint_yields_no_scoped_url_at_all() {
        // Seam-contract D16.7's busy port, reaching the runner: the run is handed
        // no handle, and the caller refuses to start a planner rather than
        // starting one that cannot answer.
        let handles = RunHandles::default();
        let grant = handles.grant("task-1", TEAM, Grant::Strategy);

        assert_eq!(handles.endpoint(), None);
        assert_eq!(handles.endpoint_for(&grant), None);
    }

    #[test]
    fn rebinding_the_server_moves_the_endpoint_a_run_would_be_handed() {
        // Why this is shared mutable state and not a `String` copied at
        // startup: `commands::mcp::set_mcp_port` rebinds at runtime, and a
        // captured URL would send the next planner at a dead port.
        let handles = bound();
        let grant = handles.grant("task-1", TEAM, Grant::Strategy);

        handles.set_endpoint(Some("http://127.0.0.1:4600".to_string()));

        assert!(handles
            .endpoint_for(&grant)
            .expect("an endpoint is bound")
            .starts_with("http://127.0.0.1:4600/mcp/run/"));
    }

    #[test]
    fn a_token_resolves_to_the_team_its_task_belongs_to() {
        let handles = bound();

        let grant = handles.grant("task-1", TEAM, Grant::Strategy);

        assert_eq!(
            handles
                .resolve_with_team(grant.token())
                .map(|(_, team_id)| team_id),
            Some(TEAM.to_string())
        );
    }

    #[test]
    fn every_tool_name_round_trips_through_its_decision() {
        for tool in Tool::ALL {
            assert_eq!(Tool::from_name(tool.as_str()), Some(tool));
        }
        assert_eq!(Tool::from_name("delete_task"), None);
    }
}
