//! The strategy run: a short, cheap planner that decides how a task should be
//! executed, before the run that executes it (ADR-0016, seam-contract D17).
//!
//! # What this is not
//!
//! It is not orchestration. Rimaia decides the *top-level* `--model` and
//! `--effort` and tells the implementation run what shape of workflow to use;
//! the run's own session does any fan-out with its native subagents. ADR-0016
//! is explicit, and ADR-0004's whole premise is that we drive the harness rather
//! than rebuild it: the moment Rimaia schedules agents itself it is a second,
//! worse agent harness running inside a desktop app.
//!
//! # Why it hangs off `run_task` and not the scheduler
//!
//! [`commands::runs::start_task_run`] and [`scheduler::queue::try_step`] both
//! call [`run_task`](super::run_task), and nothing else does. Hooking into the
//! scheduler would mean a manual "Run now" on a `planned` task silently skips
//! planning — the same class of defect as a business rule enforced in one
//! adapter and not the other, which ADR-0006 exists to prevent.
//!
//! # Why it has no `runs` row, no worktree and no branch
//!
//! No `runs` row, for three independent reasons and any one of them is enough:
//! [`finish_run`](super::outcome::finish_run) calls `apply_to_task`, which on
//! success moves the card to `in_review` — a planner that worked would end the
//! task before it started. `idx_runs_task_attempt` is `UNIQUE(task_id, attempt)`
//! and `start_run` computes `max(attempt) + 1`, so a strategy row would make
//! `attempt` mean "attempts, and also the plannings", and seam-contract D12's
//! card reads `last_run`, so the badge would show the planner's outcome instead
//! of the implementation's. Distinguishing the two needs a `runs.kind` column,
//! which is a fourth migration, which D17 exists to avoid.
//!
//! The transcript still lands on disk, because
//! [`Transcript::create`](super::events::Transcript::create) touches no
//! database: `<data>/runs/<task-id>/strategy-<uuid>.jsonl`, beside the
//! implementation transcript, which is where somebody looking at 2am will
//! actually look. Task 016's cleanup knows the prefix.
//!
//! No worktree and no branch of its own, because it borrows the one
//! [`worktree::prepare`](crate::worktree::prepare) already made for this exact
//! task and this exact base ref a moment earlier, and because it commits
//! nothing — its entire output is one MCP call. It is a step *inside* running a
//! task, not a task. Running it in the operator's own checkout is refused
//! outright: ADR-0005's premise is that Rimaia never operates there, and an
//! agent told to "understand this repository" will reach for the test suite.
//!
//! # Why `run_state` is untouched
//!
//! There is only one claim. This module never calls
//! [`set_run_state`](crate::tasks::set_run_state), has no `runs` row, and is not
//! a run in the state machine's vocabulary — the task walks
//! `Idle → Queued → Running → {Idle | WaitingRetry | Failed}` exactly once, as
//! it did before task 020. That is also the concrete reason the planner must not
//! take a claim of its own: it would need `Running → Running`, which is illegal
//! outright, or `Running → Queued`, which is not in the table at all.

use std::collections::HashSet;
use std::path::Path;

use crate::context::ServiceContext;
use crate::db::settings::RunEnvironment;
use crate::db::{new_id, BoardColumn, ExitClass, Repository, StrategyMode, StrategySource};
use crate::error::Result;
use crate::paths::AppPaths;
use crate::scheduler::{InFlight, Lease, LeaseOwner, LeaseRefused};
use crate::strategy::{self, Catalogue, EffectiveStrategy};
use crate::tasks::strategy::{StrategyPlan, StrategyPlanRun, StrategyPlanStatus};
use crate::tasks::{self, TaskDetail, TaskFilter, TaskSummary};

use super::process::{
    disallowed_tools, Attempt, CancelSignal, Invocation, PermissionMode, RunnerConfig,
};
use super::prompt::{
    compose_strategy_prompt, compose_strategy_system_append, StrategyGuidance,
    SET_TASK_STRATEGY_TOOL,
};

/// Tools a planner is denied on top of the implementation blocklist.
///
/// Denying `Bash` is what makes "runs in a worktree it will not disturb" true
/// rather than merely intended — without it, an agent asked to understand a
/// repository reaches for the test suite, in a checkout that belongs to a task
/// nobody has run yet. Read, Grep and Glob are all a planner needs to read a
/// plan and name a model.
const PLANNER_DENIED_TOOLS: [&str; 4] = ["Write", "Edit", "NotebookEdit", "Bash"];

/// The prefix a strategy transcript's synthetic id carries.
///
/// There is no `runs` row to hang it off, so this is the only thing that says
/// what the file is. Task 016's cleanup matches on it.
pub const STRATEGY_TRANSCRIPT_PREFIX: &str = "strategy-";

/// What the implementation run should be spawned with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Spawn with these. Reached whether the strategy came from the task, a
    /// default, or a planner that has just written one — and also whenever a
    /// planner failed, because a failed planner falls back rather than blocking.
    Ready {
        model: Option<String>,
        effort: Option<String>,
        guidance: Option<StrategyGuidance>,
    },
    /// The run was cancelled during planning. The caller releases its claim and
    /// spawns nothing: a planner the user just stopped must not be followed by
    /// the implementation run they were stopping.
    Cancelled,
}

/// Decides how this task should be executed, planning first when it asks for it.
///
/// **Never returns `Err` for a planner failure.** A non-zero exit, a `max_turns`
/// cut-off, a usage limit, a stream that never produced a `result`, and a run
/// that finished without calling the tool all collapse to the same three steps:
/// record a `failed` envelope on the card, warn with the reason, and return what
/// the `default` chain gives. That is ADR-0016's "failure is not fatal: a failed
/// strategy run falls back to `default` and notes it on the task rather than
/// blocking the queue" — and it is why the queue needs no knowledge of any of
/// this. `Err` is reserved for a database or filesystem failure, which is the
/// caller's problem in exactly the way it already was.
pub async fn resolve(
    ctx: &ServiceContext,
    paths: &AppPaths,
    config: &RunnerConfig,
    detail: &TaskDetail,
    repository: &Repository,
    worktree: &Path,
    cancel: &CancelSignal,
) -> Result<Resolution> {
    let effective = effective_for(ctx, detail, repository).await?;

    if !tasks::strategy::needs_planning(&detail.task, effective.mode) {
        return Ok(ready(detail, &effective));
    }

    let catalogue = strategy::catalogue::catalogue(&ctx.pool).await?;

    match plan(
        ctx, paths, config, detail, repository, worktree, cancel, &catalogue,
    )
    .await?
    {
        Planned::Wrote => {
            // Re-read rather than trusting what we sent: `set_task_strategy` is
            // the single writer, and what it *stored* — after its own
            // validation, and after copying the planner's choice onto
            // `tasks.model` and `tasks.effort` — is what the implementation run
            // must spawn with. Reading it back is also what makes the write-back
            // path real rather than assumed.
            let detail = tasks::get_task(ctx, &detail.task.id).await?;
            let effective = effective_for(ctx, &detail, repository).await?;
            Ok(ready(&detail, &effective))
        }
        Planned::Failed(reason) => {
            tracing::warn!(
                task_id = %detail.task.id,
                %reason,
                "the strategy run did not produce a strategy; falling back to the default",
            );
            record_failure(ctx, &detail.task.id, &reason).await;

            // The default chain, deliberately re-read: `record_failure` has just
            // cleared the task's own model and effort, so recomputing is what
            // turns "the planner failed" into "this task runs on the default"
            // rather than on a half-written proposal.
            let detail = tasks::get_task(ctx, &detail.task.id).await?;
            let effective = effective_for(ctx, &detail, repository).await?;
            Ok(ready(&detail, &effective))
        }
        Planned::Cancelled => Ok(Resolution::Cancelled),
    }
}

/// The task's strategy after the full precedence chain.
///
/// Task, then repository default, then global default — the one derivation, in
/// [`strategy::resolve`], reached from here so the flags a run spawns with and
/// the badge the card draws cannot disagree.
async fn effective_for(
    ctx: &ServiceContext,
    detail: &TaskDetail,
    repository: &Repository,
) -> Result<EffectiveStrategy> {
    let global = strategy::settings::global_default(&ctx.pool).await?;
    let per_repository = strategy::settings::repository_default(&ctx.pool, &repository.id).await?;

    Ok(strategy::effective_strategy(
        &detail.task,
        &per_repository,
        &global,
    ))
}

fn ready(detail: &TaskDetail, effective: &EffectiveStrategy) -> Resolution {
    // Guidance only when the resolved mode is `planned`. `resolve::rule 3`
    // already drops a stale proposal's *model and effort* for a task in
    // `default` or `manual` mode; leaving its workflow section in the prompt
    // would keep half of a proposal the user overrode — the run would be told
    // "this work fans out, use subagents" while spawning with the model the
    // user chose instead. Two halves of one decision must not disagree.
    let guidance = match effective.mode {
        StrategyMode::Planned => StrategyGuidance::for_task(detail),
        StrategyMode::Default | StrategyMode::Manual => None,
    };

    Resolution::Ready {
        model: effective.model.clone(),
        effort: effective.effort.clone(),
        guidance,
    }
}

/// How one planner attempt ended, in the vocabulary this module acts on.
enum Planned {
    /// The planner called the tool and the card now carries a proposal.
    Wrote,
    /// Anything else. The string is what goes on the card and into the log.
    Failed(String),
    Cancelled,
}

#[allow(clippy::too_many_arguments)]
async fn plan(
    ctx: &ServiceContext,
    paths: &AppPaths,
    config: &RunnerConfig,
    detail: &TaskDetail,
    repository: &Repository,
    worktree: &Path,
    cancel: &CancelSignal,
    catalogue: &Catalogue,
) -> Result<Planned> {
    let task_id = &detail.task.id;

    // Minted before anything is spawned and dropped when this function returns,
    // whichever way it returns. The grant *is* the lifetime of the run's ability
    // to address Rimaia, so there is nothing to remember to revoke.
    let grant = config.run_handles.grant(task_id);
    let Some(mcp_config) = config.run_handles.mcp_config_json(&grant) else {
        // Seam-contract D16.7 makes a busy MCP port non-fatal to startup, which
        // means a run can reach here with nothing listening. Spawning a planner
        // whose only way to answer is a server that is not there would burn a
        // run to produce nothing, so this is a failure with an address rather
        // than an attempt.
        return Ok(Planned::Failed(
            "the strategy run needs Rimaia's MCP server, which is not listening (see Settings → MCP)"
                .to_string(),
        ));
    };

    let prompt = compose_strategy_prompt(detail, repository, catalogue);
    let invocation = planner_invocation(ctx, catalogue, task_id, mcp_config).await?;

    // No `runs` row, so no run id — a synthetic one, whose only job is to name a
    // transcript beside the implementation's. See this module's header.
    let transcript_id = format!("{STRATEGY_TRANSCRIPT_PREFIX}{}", new_id());

    // Read *before* the spawn, and compared against the task's own
    // `strategy_updated_at` afterwards. This is how "did the planner actually
    // call the tool" is answered — by asking the single writer whether it wrote,
    // never by parsing what the run printed. Printed JSON would be a second
    // writer with its own parser, duplicating every invariant
    // `set_task_strategy` enforces, which is the exact ADR-0006 defect.
    let before = ctx.clock.now();

    let outcome = super::execute(
        ctx,
        paths,
        config,
        Attempt {
            task_id,
            run_id: &transcript_id,
            worktree,
            prompt: &prompt,
            invocation: &invocation,
            cancel,
        },
    )
    .await;

    let outcome = match outcome {
        Ok(outcome) => outcome,
        // Spawning or supervision itself failed. There is no row to close out —
        // that is the whole point of having none — so this is simply a planner
        // that produced nothing, handled like every other way of producing
        // nothing.
        Err(error) => return Ok(Planned::Failed(error.to_string())),
    };

    if outcome.exit_class == ExitClass::Cancelled || cancel.is_cancelled() {
        return Ok(Planned::Cancelled);
    }

    let after = tasks::get_task(ctx, task_id).await?;
    let wrote = after
        .task
        .strategy_updated_at
        .is_some_and(|stamp| stamp >= before);

    if !wrote {
        return Ok(Planned::Failed(match outcome.error_message {
            Some(message) => message,
            None => format!("the strategy run finished without calling `{SET_TASK_STRATEGY_TOOL}`"),
        }));
    }

    // The planner's own cost, recorded onto the proposal it just wrote so the
    // panel can say what the decision cost. A best-effort second write: the
    // proposal is already on the card and losing the receipt is not worth
    // failing a run that succeeded.
    stamp_run_metadata(ctx, task_id, &after, &invocation, &outcome).await;

    Ok(Planned::Wrote)
}

/// The planner's invocation — narrower than either posture ADR-0012 fixed.
///
/// Every difference from an implementation run is deliberate and is argued in
/// ADR-0004's and ADR-0012's 2026-08-28 amendments:
///
/// - **`acceptEdits`, not `bypassPermissions`.** It writes nothing, so the
///   widest posture in the app is not the one to hand it.
/// - **Write, Edit, NotebookEdit and Bash denied**, on top of the operator's
///   own blocklist.
/// - **`strict_local` forced**, whatever the `run_environment` setting says.
///   Cost is half the argument — `spike/FINDINGS.md` measures inheriting at
///   ~3.6× on the one run whose entire premise is being cheap — and the security
///   property is the other half: `--strict-mcp-config` is what guarantees the
///   only MCP server this run can reach is the scoped Rimaia handle, and not
///   whatever the operator has configured.
/// - **Bounded by `--max-turns`** from the catalogue, so a planner in a loop
///   costs cents.
async fn planner_invocation(
    ctx: &ServiceContext,
    catalogue: &Catalogue,
    task_id: &str,
    mcp_config: String,
) -> Result<Invocation> {
    let mut denied = disallowed_tools(&ctx.pool).await?;
    for tool in PLANNER_DENIED_TOOLS {
        if !denied.iter().any(|held| held == tool) {
            denied.push(tool.to_string());
        }
    }

    Ok(Invocation {
        session_id: new_id(),
        resume: false,
        permission_mode: PermissionMode::AcceptEdits,
        run_environment: RunEnvironment::StrictLocal,
        system_append: compose_strategy_system_append(task_id, SET_TASK_STRATEGY_TOOL),
        model: catalogue.planner.model.clone(),
        effort: catalogue.planner.effort.clone(),
        // The one tool the planner exists to call, pre-approved.
        //
        // Without this the run cannot work at all, and the failure is silent in
        // the worst way: `acceptEdits` auto-approves *file edits* and nothing
        // else, so an `mcp__*` call raises a permission request that an
        // unattended session has nobody to answer. The CLI refuses it, the run
        // ends looking successful, and the only trace is a tool result reading
        // "Claude requested permissions to use mcp__rimaia__set_task_strategy,
        // but you haven't granted it yet." Every planned task then falls back to
        // the default, forever.
        //
        // Naming it here rather than widening `permission_mode` to
        // `bypassPermissions` is what keeps ADR-0012's amendment honest: the
        // planner is permitted exactly its own write-back, while
        // `PLANNER_DENIED_TOOLS` still denies it every way of touching the
        // worktree it is reading.
        allowed_tools: vec![SET_TASK_STRATEGY_TOOL.to_string()],
        disallowed_tools: denied,
        mcp_config: Some(mcp_config),
        max_turns: Some(catalogue.planner.max_turns),
    })
}

/// Records a `failed` envelope on the card.
///
/// Best-effort and infallible by construction: this runs on the path where
/// something already went wrong, and failing to write the note must not turn a
/// recoverable planner failure into a failed run. The queue carries on either
/// way — that is the acceptance criterion.
async fn record_failure(ctx: &ServiceContext, task_id: &str, reason: &str) {
    let plan = StrategyPlan::failed(reason);
    if let Err(error) =
        tasks::strategy::set_task_strategy(ctx, task_id, plan, StrategySource::Planner).await
    {
        tracing::error!(%task_id, %error, "could not record the strategy failure on the task");
    }
}

/// Copies the planner's turns, cost and session id onto the proposal it wrote.
async fn stamp_run_metadata(
    ctx: &ServiceContext,
    task_id: &str,
    after: &TaskDetail,
    invocation: &Invocation,
    outcome: &super::outcome::RunOutcome,
) {
    let Some(mut plan) = StrategyPlan::from_stored(after.task.strategy_plan.as_deref()) else {
        return;
    };
    if plan.status != StrategyPlanStatus::Proposed {
        return;
    }

    plan.run = Some(StrategyPlanRun {
        session_id: Some(invocation.session_id.clone()),
        num_turns: outcome.num_turns,
        cost_usd: outcome.cost_usd,
        error: None,
    });

    if let Err(error) =
        tasks::strategy::set_task_strategy(ctx, task_id, plan, StrategySource::Planner).await
    {
        tracing::warn!(%task_id, %error, "could not record what the strategy run cost");
    }
}

// ---------------------------------------------------------------------------
// Planning on demand — one card, or a whole selection (tasks 020 and 023)
// ---------------------------------------------------------------------------

/// Everything a surface needs to be able to start a planner (task 023).
///
/// One value rather than three parameters because the three travel together and
/// because it is what the MCP server was missing. **This is ADR-0021's named
/// gap.** `plan_task_strategy` was left off the tool surface on the grounds that
/// it spawns a process and that "the MCP server does not know the shell's
/// `AppPaths`"; seam-contract D19 moved the in-flight registry into
/// `rimaia-core`, and this carries the other two, so both the single and the
/// batch form become expressible in one move rather than two.
///
/// Cheap to clone: an [`AppPaths`] of two `PathBuf`s, a [`RunnerConfig`] the
/// whole app already shares, and an [`InFlight`] that is an `Arc` inside.
#[derive(Debug, Clone)]
pub struct PlannerAccess {
    pub paths: AppPaths,
    pub runner: RunnerConfig,
    /// The one registry every door takes leases from — the queue, "Run now",
    /// "Plan now" and a pass.
    pub in_flight: InFlight,
}

/// A planner slot taken, with every refusal that can be settled before a
/// process is spawned already settled.
///
/// Two functions rather than one, because the two adapters need the split at
/// different places. `plan_task_strategy` has to know the slot is *taken*
/// before it answers, or a double-click starts two planners; the MCP tool and
/// [`plan_all`] simply await the whole thing. Splitting it here means both go
/// through one set of rules instead of each carrying its own preflight — which
/// is the ADR-0006 defect this pair exists to avoid, and the reason the body of
/// this used to live in `src-tauri`.
pub struct PlannerClaim {
    lease: Lease,
    detail: TaskDetail,
    repository: Repository,
}

impl PlannerClaim {
    pub fn task_id(&self) -> &str {
        &self.detail.task.id
    }

    pub fn title(&self) -> &str {
        &self.detail.task.title
    }

    /// The signal a Cancel or a queue Stop trips. Held by the lease, so it is
    /// released the moment the claim is dropped.
    pub fn cancel_signal(&self) -> CancelSignal {
        self.lease.cancel_signal()
    }
}

/// Takes the slot for planning one task, or says why not.
///
/// Everything here is read-only apart from the lease, and every refusal is one
/// a caller can render:
///
/// - **Already in flight** — the same registry the queue and "Run now" take
///   from ([`InFlight`], seam-contract D19). This is what closes the hazard task
///   023's Notes name: a planner and a queued run genuinely could both start for
///   one task while the queue claimed on the database row and the planner
///   claimed in the shell. One registry, not a new check.
/// - **The repository has not opted into unattended runs** (ADR-0012).
/// - **The task does not resolve to `planned` mode.** Refused before anything is
///   spawned because `set_task_strategy` will not accept a planner's write for a
///   task that is not in planned mode — without this the run happens, costs
///   money, is refused its one write, and then has its *failure note* refused by
///   the same guard, leaving no trace on the card and nothing on screen.
///
/// [`needs_planning`](crate::tasks::strategy::needs_planning) is deliberately
/// **not** consulted here: it guards the *automatic* path from replanning a task
/// that already carries a proposal, and a person pressing Plan now is saying
/// "plan it again anyway". [`plan_all`] applies it itself, because a batch pass
/// that quietly overwrote proposals the user had accepted would be the opposite
/// of a review aid (task 023's Out of scope).
pub async fn claim_for_planning(
    ctx: &ServiceContext,
    in_flight: &InFlight,
    task_id: &str,
    owner: LeaseOwner,
) -> Result<std::result::Result<PlannerClaim, PlanSkip>> {
    let detail = tasks::get_task(ctx, task_id).await?;
    let repository = crate::repo::get(ctx, &detail.task.repository_id).await?;

    if let Err(error) = crate::repo::ensure_unattended_runs_allowed(&repository) {
        return Ok(Err(PlanSkip::RepositoryNotOptedIn {
            repository: repository.name.clone(),
            reason: error.to_string(),
        }));
    }

    let mode = effective_mode(ctx, &detail, &repository).await?;
    if mode != StrategyMode::Planned {
        return Ok(Err(PlanSkip::NotPlanned { mode }));
    }

    // Taken last, so a refusal the caller could have been told about without
    // touching the registry does not briefly occupy a slot on its way to being
    // reported.
    let lease = match in_flight.acquire_unbounded(task_id, &repository.id, owner) {
        Ok(lease) => lease,
        Err(refused) => return Ok(Err(PlanSkip::InFlight(refused))),
    };

    Ok(Ok(PlannerClaim {
        lease,
        detail,
        repository,
    }))
}

/// Runs the planner the claim was taken for, and drops the claim on the way out.
///
/// **Never `Err` for a planner that failed** — the same contract [`resolve`]
/// keeps: a failure is recorded on the card and reported as
/// [`PlanOutcome::Failed`]. `Err` is for a database or filesystem failure, which
/// is the caller's problem in the way it already was.
pub async fn plan_claimed(
    ctx: &ServiceContext,
    paths: &AppPaths,
    config: &RunnerConfig,
    claim: PlannerClaim,
) -> Result<PlanOutcome> {
    let cancel = claim.cancel_signal();
    let PlannerClaim {
        lease: _lease,
        detail,
        repository,
    } = claim;
    let task_id = detail.task.id.clone();

    // The planner reads the repository, so it needs a checkout that is not the
    // operator's (ADR-0005). `prepare` is idempotent, so a task that already has
    // one is unchanged and a task that does not gets the same worktree its
    // implementation run would have used.
    let worktree = crate::worktree::prepare(ctx, &task_id).await?;
    let catalogue = strategy::catalogue::catalogue(&ctx.pool).await?;

    match plan(
        ctx,
        paths,
        config,
        &detail,
        &repository,
        Path::new(&worktree.path),
        &cancel,
        &catalogue,
    )
    .await?
    {
        Planned::Wrote => {
            // Read back rather than reported from here: `set_task_strategy` is
            // the single writer, and the summary a user reads before going home
            // has to be what is actually on the card.
            let after = tasks::get_task(ctx, &task_id).await?;
            Ok(PlanOutcome::from_stored(
                after.task.strategy_plan.as_deref(),
            ))
        }
        Planned::Cancelled => Ok(PlanOutcome::Cancelled),
        Planned::Failed(reason) => {
            tracing::warn!(%task_id, %reason, "the strategy run did not produce a strategy");
            record_failure(ctx, &task_id, &reason).await;
            Ok(PlanOutcome::Failed(reason))
        }
    }
}

/// Why a card was passed over, in the vocabulary a summary shows.
///
/// Every variant is a sentence a user can act on. Task 023's failure mode is an
/// empty column silently doing nothing, so a skip is a *result*, never an
/// omission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanSkip {
    /// The card already carries a proposal (D17.8's re-plan guard). Re-planning
    /// is per-card and deliberate; a pass that overwrote an accepted proposal
    /// would be the opposite of a review aid.
    AlreadyProposed,
    /// The resolved mode is not `planned`, so there is no strategy to plan.
    NotPlanned { mode: StrategyMode },
    /// The queue, a manual run, or another planner already holds this task —
    /// the one registry, not a second check (seam-contract D19).
    InFlight(LeaseRefused),
    /// ADR-0012's per-repository opt-in is off.
    RepositoryNotOptedIn { repository: String, reason: String },
}

impl PlanSkip {
    /// The sentence the summary and the tool response both show.
    pub fn message(&self) -> String {
        match self {
            Self::AlreadyProposed => {
                "already carries a proposal — clear it, or use Re-plan on the card".to_string()
            }
            Self::NotPlanned { mode } => format!(
                "its strategy mode resolves to {}, not planned",
                match mode {
                    StrategyMode::Default => "default",
                    StrategyMode::Manual => "manual",
                    StrategyMode::Planned => "planned",
                }
            ),
            Self::InFlight(refused) => refused.message(),
            Self::RepositoryNotOptedIn { reason, .. } => reason.clone(),
        }
    }

    /// A stable machine-readable tag, so a client can group skips without
    /// matching on prose (the argument `doctor::Check` makes for its own).
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::AlreadyProposed => "already_proposed",
            Self::NotPlanned { .. } => "not_planned",
            Self::InFlight(_) => "in_flight",
            Self::RepositoryNotOptedIn { .. } => "repository_not_opted_in",
        }
    }
}

/// How one card ended up.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanOutcome {
    /// A proposal is on the card. Read back off it, never reported from what
    /// the planner was asked for.
    Planned {
        model: Option<String>,
        effort: Option<String>,
        rationale: Option<String>,
        cost_usd: Option<f64>,
    },
    Skipped(PlanSkip),
    /// The planner ran and produced nothing. The card carries the `failed`
    /// envelope and falls back to the default chain.
    Failed(String),
    /// The pass was cancelled while this card's planner was running.
    Cancelled,
}

impl PlanOutcome {
    fn from_stored(stored: Option<&str>) -> Self {
        match StrategyPlan::from_stored(stored) {
            Some(plan) if plan.status == StrategyPlanStatus::Proposed => Self::Planned {
                model: plan.model,
                effort: plan.effort,
                rationale: plan.rationale,
                cost_usd: plan.run.and_then(|run| run.cost_usd),
            },
            // The planner wrote, and what it wrote is no longer a proposal —
            // the only realistic cause is a human editing the card in the
            // seconds between. Reported as the failure it is for this pass
            // rather than as a proposal that is not there.
            _ => Self::Failed(
                "the proposal was not on the card when the pass read it back".to_string(),
            ),
        }
    }

    fn cost_usd(&self) -> f64 {
        match self {
            Self::Planned { cost_usd, .. } => cost_usd.unwrap_or(0.0),
            _ => 0.0,
        }
    }
}

/// One card's line in the summary.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanResult {
    pub task_id: String,
    pub title: String,
    pub outcome: PlanOutcome,
}

/// What a whole pass did.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlanPass {
    pub results: Vec<PlanResult>,
    /// What the planners this pass ran actually cost, summed off the proposals
    /// they wrote. The point of the feature is that the user chose to spend it.
    pub spent_usd: f64,
    /// Whether the pass stopped early. Proposals already written stay written.
    pub cancelled: bool,
}

impl PlanPass {
    pub fn planned(&self) -> usize {
        self.results
            .iter()
            .filter(|result| matches!(result.outcome, PlanOutcome::Planned { .. }))
            .count()
    }

    pub fn skipped(&self) -> usize {
        self.results
            .iter()
            .filter(|result| matches!(result.outcome, PlanOutcome::Skipped(_)))
            .count()
    }
}

/// What a surface is told as each card finishes.
pub struct PlanProgress<'a> {
    /// 0-based index of the card that just finished.
    pub index: usize,
    pub total: usize,
    pub spent_usd: f64,
    pub result: &'a PlanResult,
}

/// Which cards a pass plans (task 023).
///
/// A **core** type, not a shape each surface builds for itself, so the board and
/// the MCP server cannot disagree about what "the ready column" means. Every
/// stated field narrows; a selection that states nothing is refused rather than
/// silently meaning the whole board, because planning everything is an expensive
/// thing nobody asked for by omission.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanSelection {
    pub column: Option<BoardColumn>,
    pub repository_id: Option<String>,
    pub task_ids: Vec<String>,
}

impl PlanSelection {
    fn is_empty(&self) -> bool {
        self.column.is_none() && self.repository_id.is_none() && self.task_ids.is_empty()
    }
}

/// The cards a selection names, in board order.
///
/// Board order rather than the order the ids arrived in: the summary is read
/// against the column the user is looking at, and a pass that walked a
/// hand-picked set in request order would report them in an order nothing on
/// screen matches.
///
/// A named id that does not exist, or that the other filters exclude, is a
/// **refusal naming the id** rather than a silent omission — the same treatment
/// `set_task_dependencies` gives an unknown dependency, and for the same reason:
/// a caller that mistyped an id should not read "0 planned" and conclude the
/// column was empty.
pub async fn selected_tasks(
    ctx: &ServiceContext,
    selection: &PlanSelection,
) -> Result<Vec<TaskSummary>> {
    if selection.is_empty() {
        return Err(crate::error::Error::invalid(
            "a planning pass needs a column, a repository or a list of tasks to work on",
        ));
    }

    let matching = tasks::list_tasks(
        ctx,
        TaskFilter {
            repository_id: selection.repository_id.clone(),
            column: selection.column,
            run_state: None,
        },
    )
    .await?;

    if selection.task_ids.is_empty() {
        return Ok(matching);
    }

    let wanted: HashSet<&str> = selection.task_ids.iter().map(String::as_str).collect();
    let selected: Vec<TaskSummary> = matching
        .into_iter()
        .filter(|summary| wanted.contains(summary.task.id.as_str()))
        .collect();

    if selected.len() != wanted.len() {
        let found: HashSet<&str> = selected
            .iter()
            .map(|summary| summary.task.id.as_str())
            .collect();
        let missing: Vec<&str> = selection
            .task_ids
            .iter()
            .map(String::as_str)
            .filter(|id| !found.contains(id))
            .collect();
        return Err(crate::error::Error::invalid(format!(
            "these tasks are not in the selection being planned: {}",
            missing.join(", "),
        )));
    }

    Ok(selected)
}

/// Plans every eligible card in `selection`, one at a time.
///
/// # Sequential, and not by accident
///
/// Ten cards at fifteen seconds is two and a half minutes, once, while the user
/// packs up. Fanning out would make the *preflight* the thing that trips the
/// usage limit the evening's real work needed — task 023's Notes are explicit,
/// and concurrency here is task 012's `max_concurrency`, not a second knob.
///
/// # Every card produces a line
///
/// Skipped, planned or failed, each one is in [`PlanPass::results`] with a
/// reason. "A column with nothing eligible reports that plainly — not a success,
/// not an error, and never a silent no-op" is the acceptance criterion, and it
/// is met by there being no path that drops a card without recording why.
///
/// # Cancelling stops before the next planner
///
/// Checked between cards and honoured by the running planner's own cancel
/// signal, so a pass stopped mid-way leaves every proposal already written in
/// place. There is nothing to roll back: each proposal is a committed write to
/// its own card.
pub async fn plan_all(
    ctx: &ServiceContext,
    paths: &AppPaths,
    config: &RunnerConfig,
    in_flight: &InFlight,
    selection: &PlanSelection,
    cancel: &CancelSignal,
    on_progress: &(dyn Fn(PlanProgress<'_>) + Send + Sync),
) -> Result<PlanPass> {
    let selected = selected_tasks(ctx, selection).await?;
    let total = selected.len();
    let mut pass = PlanPass::default();

    for (index, summary) in selected.into_iter().enumerate() {
        if cancel.is_cancelled() {
            pass.cancelled = true;
            break;
        }

        let task_id = summary.task.id.clone();
        let title = summary.task.title.clone();

        // D17.8's re-plan guard, applied by the *batch* path only. A person
        // pressing Plan now on one card means "again, anyway"; a pass that
        // overwrote proposals the user had already read would be the opposite
        // of the review aid this is.
        let outcome = if summary.task.strategy_plan.is_some() {
            PlanOutcome::Skipped(PlanSkip::AlreadyProposed)
        } else {
            // `Manual`, because a pass is a person at the machine: a Stop
            // pressed on the queue must not kill a preflight they started
            // deliberately.
            match claim_for_planning(ctx, in_flight, &task_id, LeaseOwner::Manual).await? {
                Ok(claim) => plan_claimed(ctx, paths, config, claim).await?,
                Err(skip) => PlanOutcome::Skipped(skip),
            }
        };

        pass.spent_usd += outcome.cost_usd();
        if matches!(outcome, PlanOutcome::Cancelled) {
            pass.cancelled = true;
        }

        let result = PlanResult {
            task_id,
            title,
            outcome,
        };
        on_progress(PlanProgress {
            index,
            total,
            spent_usd: pass.spent_usd,
            result: &result,
        });
        pass.results.push(result);
    }

    Ok(pass)
}

/// Whether a task would plan, without spawning anything.
///
/// The board and the "Plan now" button both need to know, and neither should
/// have to reimplement the precedence chain to find out.
pub async fn would_plan(
    ctx: &ServiceContext,
    detail: &TaskDetail,
    repository: &Repository,
) -> Result<bool> {
    let effective = effective_for(ctx, detail, repository).await?;
    Ok(tasks::strategy::needs_planning(
        &detail.task,
        effective.mode,
    ))
}

/// The mode a task resolves to after the precedence chain, for callers that
/// want the decision without the run.
pub async fn effective_mode(
    ctx: &ServiceContext,
    detail: &TaskDetail,
    repository: &Repository,
) -> Result<StrategyMode> {
    Ok(effective_for(ctx, detail, repository).await?.mode)
}
