//! Runner leases: who holds a task, decided in one transaction, and every
//! report fenced by the generation it was granted (ADR-0031, ADR-0010,
//! seam-contract D31's 043 amendment).
//!
//! The code behind `board::service`'s lease-bearing functions. It is not a port
//! method of its own: both adapters reach it through the same service
//! functions, so a rule here holds in process and over HTTP alike.
//!
//! # The claim is one conditional write
//!
//! ADR-0010 requires selection and the transition to `running` to happen in
//! one transaction "so the UI, the MCP server, and the scheduler cannot
//! double-claim a task", and ADR-0031 point 1 adds the lease to the same
//! transaction. [`claim`] is that transaction: `BEGIN IMMEDIATE`, the task
//! re-read, [`eligible`], both run-state edges through
//! [`transition`](crate::tasks::run_state::transition), the task's lease
//! generation incremented and the `runner_leases` row inserted, then one
//! commit. Every failure before the commit rolls all of it back, so a claim
//! either wrote everything or nothing.
//!
//! `transition` writes only if the task is still in the state the claim read,
//! and the lease's primary key admits one holder per task, so every
//! interleaving of two claimers ends with exactly one of them holding the
//! task:
//!
//! | Interleaving | Winner | Loser sees |
//! | --- | --- | --- |
//! | A commits, then B takes the write lock | A | a lease row, or no edge from `running` |
//! | B waits on A's write lock (`busy_timeout`), then reads | A | the same, after A's commit |
//! | Both read before either writes | impossible | `BEGIN IMMEDIATE` serialises the reads |
//!
//! The loser is [`Lost`](Granted) — `None` through the port — never an error:
//! a queue that lost a race moves on to the next entry, and a button says the
//! sentence it always said. The expected state in `transition`'s `WHERE` is
//! also what closes the two windows the old two-edge claim documented: a task
//! stranded at `queued` by a crash between two commits cannot happen when
//! there is one commit, and a task selected as `idle` that reached `failed`
//! in between is claimed from the state it is actually in, inside the same
//! transaction that read it.
//!
//! [`selection::plan`](crate::scheduler::selection::plan) is a *ranking*, not
//! the selection ADR-0010 means, and it stays outside the transaction because
//! "between tasks, re-read the board" requires a fresh query every pass. The
//! selection the transaction protects — "is this task still claimable by this
//! runner?" — is the re-read, [`eligible`] and the edge inside it.
//!
//! # The claim goes all the way to `running`, on purpose
//!
//! Stopping at `queued` would leave a window where a manual "Run now" could
//! take `queued -> running` out from under a queue that had already committed
//! to the task, and a task stranded at `queued` if the run failed to start —
//! a state the queue's own selection then skips forever. So a fresh start
//! takes `idle`, `failed` or `cancelled` to `queued` and on to `running` in
//! the one commit, and a resume takes `waiting_retry -> running`. A `queued`
//! task with no lease row takes the one edge it has left: only a build older
//! than 043 or task 057's `release_pin` leaves a task there.
//!
//! # The fence
//!
//! [`current`] reads the live lease inside the caller's transaction, the
//! transaction of the first write it guards, never as a separate read before
//! it: a check followed by a write in a second transaction is the
//! double-claim bug `set_run_state`'s doc measured, moved into the fence. A
//! task the lease's team does not hold is `NotFound`, in the sentence a
//! never-issued task gets (D31 point 3); a task with no lease, or with a lease
//! of another generation or another runner, is `Conflict`, which means only
//! "this lease is not the current one".
//!
//! # Pins
//!
//! Only this module writes `tasks.pinned_runner_id` (ADR-0031 point 4). A
//! finish that interrupted its run or left the task waiting to retry pins the
//! task to the lease's holder, because only that runner has the worktree and
//! the agent session a resume needs. A finish by the pinned runner that lands
//! anywhere else clears it. A `Continue`, `give_up`, a cancel, a card edit and
//! a move leave it where it is: the board never moves a pinned task on its
//! own. Task 053's expiry and task 057's `release_pin` are the other writers.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

use crate::context::{ScopedTx, ServiceContext};
use crate::db::{Run, RunKind, RunState, RunStatus, StrategyMode};
use crate::error::{Error, ErrorCode, Result};
use crate::events::{ChangeEvent, RunnerId};
use crate::review_loop::{Decision, Landing};
use crate::runner::outcome::{self, RunOutcome};
use crate::tasks::run_state::transition;
use crate::tasks::service::{fetch_task_row, team_of_task};
use crate::tasks::strategy::needs_planning;

use crate::consent::ceiling::{self, PhaseStrategy, StrategyCeiling};
use crate::consent::pieces::{pieces_composed, Composes};
pub use crate::consent::Route;
use crate::consent::{self, Composition, Ineligible, TeamCeiling};
use crate::runner::provider::ProviderId;
use crate::strategy::catalogue::{runs_on, Catalogue};
use crate::strategy::StrategyOrigin;

use super::types::{Heartbeat, LeasePurpose, LeaseRef, NextStep, RunContext};

/// How long a renewable lease lives without a heartbeat (ADR-0031 point 3).
///
/// Defined here and nowhere else: task 052's server and task 053's expiry use
/// `LeaseTerm::Renewable(LEASE_LIFETIME)`.
pub const LEASE_LIFETIME: Duration = Duration::from_secs(3 * 60);

/// Whether a board's leases expire, which is a property of the board and not
/// of a lease: `InProcessBoard::new` takes it, and the solo host passes
/// [`Never`](Self::Never) (ADR-0031 point 5, D31 point 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseTerm {
    /// `expires_at` NULL. In solo the board and the runner are one process, so
    /// a lease cannot outlive the thing holding it.
    Never,
    /// `expires_at` is the claim's or the last heartbeat's instant plus this.
    Renewable(Duration),
}

impl LeaseTerm {
    /// When a lease granted or renewed at `now` expires, or `None` for one
    /// that never does.
    pub fn expires_at(self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            LeaseTerm::Never => None,
            LeaseTerm::Renewable(lifetime) => Some(
                chrono::Duration::from_std(lifetime)
                    .ok()
                    .and_then(|lifetime| now.checked_add_signed(lifetime))
                    .unwrap_or(DateTime::<Utc>::MAX_UTC),
            ),
        }
    }
}

/// One `runner_leases` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub task_id: String,
    pub purpose: LeasePurpose,
    /// The run the lease is open for, once `start_run` has reported it.
    pub run_id: Option<String>,
    pub runner_id: RunnerId,
    pub generation: i64,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// What the board records about a task's leases: the live lease if there is
/// one, the counter that fences it, and the pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseState {
    pub lease: Option<Lease>,
    /// The task's lease generation: the generation of its last claim, never
    /// decremented or reset (D31 point 3).
    pub generation: i64,
    pub pinned_runner_id: Option<RunnerId>,
}

/// A task's lease, generation and pin, read in the context's scope.
///
/// A read for the reconcile and the tests; 061 renders the same facts.
pub async fn state_of(ctx: &ServiceContext, task_id: &str) -> Result<LeaseState> {
    let scope = ctx.scope.json();
    let row = sqlx::query!(
        r#"SELECT t.lease_generation, t.pinned_runner_id,
                  l.purpose AS "purpose?: LeasePurpose", l.run_id, l.runner_id AS "runner_id?",
                  l.generation AS "generation?", l.acquired_at AS "acquired_at?: DateTime<Utc>",
                  l.expires_at AS "expires_at: DateTime<Utc>"
             FROM tasks t LEFT JOIN runner_leases l ON l.task_id = t.id
            WHERE t.id = ?1 AND t.team_id IN (SELECT value FROM json_each(?2))"#,
        task_id,
        scope,
    )
    .fetch_optional(&ctx.pool)
    .await?
    .ok_or_else(|| Error::not_found(format!("no task with id {task_id}")))?;

    let lease = match (row.purpose, row.runner_id, row.generation, row.acquired_at) {
        (Some(purpose), Some(runner_id), Some(generation), Some(acquired_at)) => Some(Lease {
            task_id: task_id.to_string(),
            purpose,
            run_id: row.run_id,
            runner_id,
            generation,
            acquired_at,
            expires_at: row.expires_at,
        }),
        _ => None,
    };
    Ok(LeaseState {
        lease,
        generation: row.lease_generation,
        pinned_runner_id: row.pinned_runner_id,
    })
}

/// Every lease the board records for `runner_id`, in the context's scope,
/// ordered by task id: what the solo reconcile compares the runner's own
/// record against (`scheduler::reconcile::reconcile_unrecorded`).
pub async fn held_by(ctx: &ServiceContext, runner_id: &str) -> Result<Vec<(Lease, String)>> {
    let scope = ctx.scope.json();
    let rows = sqlx::query!(
        r#"SELECT l.task_id, l.purpose AS "purpose: LeasePurpose", l.run_id, l.runner_id,
                  l.generation, l.acquired_at AS "acquired_at: DateTime<Utc>",
                  l.expires_at AS "expires_at: DateTime<Utc>", t.team_id
             FROM runner_leases l JOIN tasks t ON t.id = l.task_id
            WHERE l.runner_id = ?1 AND t.team_id IN (SELECT value FROM json_each(?2))
            ORDER BY l.task_id"#,
        runner_id,
        scope,
    )
    .fetch_all(&ctx.pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            (
                Lease {
                    task_id: row.task_id,
                    purpose: row.purpose,
                    run_id: row.run_id,
                    runner_id: row.runner_id,
                    generation: row.generation,
                    acquired_at: row.acquired_at,
                    expires_at: row.expires_at,
                },
                row.team_id,
            )
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Eligibility
// ---------------------------------------------------------------------------

/// Whether a runner may take a task, as [`eligible`] answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The runner may take it, on this footing.
    Eligible(Route),
    /// The first rule that refuses it.
    Ineligible(Ineligible),
}

impl Verdict {
    /// The sentence a person reads when a named claim is refused, or `None`
    /// when the runner may take the task.
    pub fn refusal(&self) -> Option<String> {
        match self {
            Verdict::Eligible(_) => None,
            Verdict::Ineligible(ineligible) => Some(ineligible.refusal()),
        }
    }
}

/// The models and efforts a task's phases would spawn with, read with the
/// claim's context before any transaction opens (tasks 067 and 045).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhaseModels {
    /// The effective strategy's model (ADR-0016): what an implementation and
    /// a fix spawn with.
    pub strategy: Option<String>,
    /// The review loop's `review_model`, when one names it (task 021).
    pub review: Option<String>,
    /// The effective strategy's effort.
    pub effort: Option<String>,
    /// The review loop's `review_effort`, when one names it.
    pub review_effort: Option<String>,
}

impl PhaseModels {
    /// What `context`'s phases would spawn with, as `run_task` resolves it.
    pub fn of(context: &RunContext) -> Self {
        let config = context.review.as_ref().map(|review| &review.config);
        Self {
            strategy: context.strategy.model.clone(),
            review: config.and_then(|config| config.review_model.clone()),
            effort: context.strategy.effort.clone(),
            review_effort: config.and_then(|config| config.review_effort.clone()),
        }
    }

    /// The model a phase of `purpose` would spawn with, or `None` when it
    /// names none or is exempt from the model rule.
    ///
    /// - `implementation` and `fix`: the effective strategy's model;
    /// - `review`: `review_model` when it names one, otherwise the effective
    ///   strategy's model, as task 021 resolves it;
    /// - `strategy`: exempt, for a `Plan` claim and for the inline planner
    ///   alike, because a planner chooses from the runner's own catalogue
    ///   (D17).
    pub fn for_purpose(&self, purpose: LeasePurpose) -> Option<&str> {
        match purpose {
            LeasePurpose::Implementation | LeasePurpose::Fix => self.strategy.as_deref(),
            LeasePurpose::Review => self.review.as_deref().or(self.strategy.as_deref()),
            LeasePurpose::Strategy => None,
        }
    }

    /// What the strategy ceiling judges for `purpose` (task 045): the
    /// effective strategy for implementation and fix, 021's review model and
    /// effort for review, and the planner's budget for strategy, which the
    /// model rule exempts and the ceiling does not.
    pub fn strategy_for(&self, purpose: LeasePurpose, catalogue: &Catalogue) -> PhaseStrategy {
        let (model, effort) = match purpose {
            LeasePurpose::Implementation | LeasePurpose::Fix => {
                (self.strategy.clone(), self.effort.clone())
            }
            LeasePurpose::Review => (
                self.review.clone().or_else(|| self.strategy.clone()),
                self.review_effort.clone().or_else(|| self.effort.clone()),
            ),
            LeasePurpose::Strategy => (
                catalogue.planner.model.clone(),
                catalogue.planner.effort.clone(),
            ),
        };
        let origin = |value: &Option<String>| match value {
            Some(_) => StrategyOrigin::Task,
            None => StrategyOrigin::ClaudeCode,
        };
        PhaseStrategy {
            model_origin: origin(&model),
            effort_origin: origin(&effort),
            model,
            effort,
        }
    }
}

/// A runner as a candidate for one task: who it is, what its provider runs,
/// what the task's phases would spawn with, the runner's strategy ceiling
/// and what the composers would include. Everything [`eligible`] needs that
/// is not read inside the asking transaction.
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub runner_id: &'a str,
    /// The claiming runner's provider: the in-process adapter's (D31 point 9),
    /// or the one a runner sends over HTTP (D31 point 10).
    pub provider: ProviderId,
    /// The catalogue the board resolves for `provider` in the task's team.
    pub catalogue: &'a Catalogue,
    pub models: &'a PhaseModels,
    /// The ceiling the claim carried (task 045). The board only refuses with
    /// it; the runner fills from it at spawn.
    pub ceiling: &'a StrategyCeiling,
    /// Which findings and which base commit the composers would include.
    pub composition: &'a Composition,
}

/// Whether `candidate` may take `task_id` to compose what `composes` names:
/// the one place the board decides that *this* runner may take *this* task.
/// The purpose the lease is held as is `composes.purpose()`.
///
/// Six rules, in this order, and the first that refuses is the answer:
///
/// 1. **Pinning** (task 043): a task pinned to another runner is not this
///    runner's, for every purpose, `strategy` included.
/// 2. **Eligibility** (task 045, ADR-0032 point 2): assigned to the runner's
///    owner, unassigned in their personal team, or in a pool the runner
///    takes. A task pinned here and then reassigned is nobody's.
/// 3. **The team ceiling** (ADR-0032 point 4), for every purpose, except in a
///    personal team, where the runner's own consent is the whole decision.
/// 4. **The model rule** (task 067, ADR-0031 point 1). `strategy` is exempt
///    ([`PhaseModels::for_purpose`]).
/// 5. **The strategy ceiling** ([`ceiling::judge`]), using only its refusal.
/// 6. **Consent** (ADR-0032 points 3 and 6) to every piece
///    [`pieces_composed`] lists for `composes`: for ADR-0016's inline
///    planner, the implementation's pieces as well as the planner's.
///
/// Nothing adds a second call: selection's plan, the claim transaction, a
/// `Continue` and `run_context` all ask this. Selection's own rules
/// (ADR-0010's order, dependencies, D21's cap) are not restated here.
///
/// Takes the connection of whichever transaction asks, so everything above
/// is read in the transaction that acts on it. A task that does not exist is
/// `Eligible`: whether it exists is the caller's question, answered in its
/// own words.
pub async fn eligible(
    conn: &mut SqliteConnection,
    task_id: &str,
    candidate: &Candidate<'_>,
    composes: Composes,
) -> Result<Verdict> {
    let refuse = |ineligible| Ok(Verdict::Ineligible(ineligible));
    let purpose = composes.purpose();
    let Some(task) = consent::task_row(conn, task_id).await? else {
        return Ok(Verdict::Eligible(Route::Assigned));
    };

    if let Some(pinned) = &task.pinned_runner_id {
        if pinned != candidate.runner_id {
            return refuse(Ineligible::PinnedElsewhere {
                label: task.pinned_label.clone().unwrap_or_else(|| pinned.clone()),
            });
        }
    }

    // A runner the board has no row for has no owner to be eligible or to
    // consent: only a plan drawn for a view, never a claim, whose lease row
    // would name it, can ask about one. It is judged on the other four rules.
    let runner = consent::runner_row(conn, candidate.runner_id).await?;
    let route = match &runner {
        Some(row) => match consent::decide(&task, Some(row), candidate.runner_id) {
            Ok(route) => route,
            Err(ineligible) => return refuse(ineligible),
        },
        None => Route::Assigned,
    };

    if task.team_ceiling() == TeamCeiling::Forbidden {
        return refuse(Ineligible::ForbiddenByTeam {
            repository: task.repository_name.clone(),
        });
    }

    if let Some(model) = candidate.models.for_purpose(purpose) {
        if !runs_on(model, candidate.provider, candidate.catalogue) {
            return refuse(Ineligible::ModelNotOffered {
                model: model.to_string(),
                provider: candidate.provider,
            });
        }
    }

    let strategy = candidate.models.strategy_for(purpose, candidate.catalogue);
    if let Err(exceeded) = ceiling::judge(&strategy, candidate.ceiling, candidate.catalogue) {
        return refuse(Ineligible::CeilingExceeded {
            label: runner.as_ref().map_or_else(
                || candidate.runner_id.to_string(),
                |runner| runner.label.clone(),
            ),
            exceeded,
        });
    }

    if let Some(runner) = &runner {
        if let Some(refused) = refuse_consent(
            conn,
            task_id,
            candidate.runner_id,
            &runner.owner,
            &task.team_id,
            composes,
            candidate.composition,
        )
        .await?
        {
            return refuse(refused);
        }
    }

    Ok(Verdict::Eligible(route))
}

/// [`eligible`]'s sixth rule alone: whether `runner_id`'s owner still
/// consents to everything `composes` would execute of `task_id`. What
/// `run_context` re-checks before a composition (D31 point 6).
pub(crate) async fn consent_refusal(
    conn: &mut SqliteConnection,
    task_id: &str,
    runner_id: &str,
    composes: Composes,
    composition: &Composition,
) -> Result<Option<Ineligible>> {
    let Some(task) = consent::task_row(conn, task_id).await? else {
        return Ok(None);
    };
    let Some(runner) = consent::runner_row(conn, runner_id).await? else {
        return Ok(None);
    };
    let owner = runner.owner;
    refuse_consent(
        conn,
        task_id,
        runner_id,
        &owner,
        &task.team_id,
        composes,
        composition,
    )
    .await
}

/// What a held lease composes from here on: its purpose's composer, or for a
/// `strategy` lease on a `running` task, ADR-0016's inline planner and the
/// implementation after it.
///
/// Only a fresh start takes a task to `running` with a `strategy` lease; a
/// `Plan` claim takes no edge (D31's purposes), which is the distinction
/// [`release`] keys on too. A `Continue` has already moved the lease to the
/// next phase's purpose ([`land`]), so every other lease names its own
/// composer.
pub(crate) fn composes(purpose: LeasePurpose, run_state: RunState) -> Composes {
    match (purpose, run_state) {
        (LeasePurpose::Strategy, RunState::Running) => Composes::PlannerThenImplementation,
        (purpose, _) => Composes::Phase(purpose),
    }
}

async fn refuse_consent(
    conn: &mut SqliteConnection,
    task_id: &str,
    runner_id: &str,
    owner: &str,
    team_id: &str,
    composes: Composes,
    composition: &Composition,
) -> Result<Option<Ineligible>> {
    let Some(inputs) = consent::inputs(conn, task_id, runner_id, composition).await? else {
        return Ok(None);
    };
    let pieces = pieces_composed(composes, &inputs);
    let missing = consent::missing(conn, owner, team_id, &pieces).await?;
    match missing.into_iter().next() {
        Some((piece, reason)) => Ok(Some(consent::refusal_for(conn, &piece, reason).await?)),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// The claim
// ---------------------------------------------------------------------------

/// Which edges a claim takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edges {
    /// Run now, or a fresh start from `Next`: `idle`, `failed` or `cancelled`
    /// to `queued` to `running`, or `queued` to `running` for a task with no
    /// lease row.
    Fresh,
    /// Retry now, or a due retry from `Next`: `waiting_retry` to `running`,
    /// as the purpose of the kind that was waiting (D29 point 3).
    Resume { kind: Option<RunKind> },
    /// D17's planner: a lease and no edge, purpose `strategy`.
    Plan,
}

/// Who asked, which decides how an ineligible task is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Door {
    /// `Run` or `Plan` named the task: a refusal a person reads, an `Err`
    /// (D31 point 4).
    Named,
    /// `Next` chose it: passed over, as a lost race is, and the next entry
    /// tried.
    Next,
}

/// What [`claim`] asks for.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClaimRequest<'a> {
    pub task_id: &'a str,
    /// The claiming runner, and what the task's phases would spawn with.
    pub candidate: Candidate<'a>,
    pub term: LeaseTerm,
    pub edges: Edges,
    pub door: Door,
    /// The task's resolved strategy mode, read with the claim's context: a
    /// fresh start of a task that needs planning is leased as `strategy`,
    /// ADR-0016's inline planner.
    pub mode: StrategyMode,
}

/// A claim the transaction granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Granted {
    pub generation: i64,
    pub purpose: LeasePurpose,
}

/// The claim transaction: eligibility, the edges, the generation and the
/// lease, in one commit, then one change event. `None` is a claim lost to
/// another starter, or a task that is gone; nothing was written.
pub(crate) async fn claim(
    ctx: &ServiceContext,
    request: ClaimRequest<'_>,
) -> Result<Option<Granted>> {
    let ClaimRequest {
        task_id,
        candidate,
        term,
        edges,
        door,
        mode,
    } = request;
    let clock = ctx.clock.as_ref();

    let mut tx = ctx.begin_immediate().await?;
    let task = match fetch_task_row(&mut tx, task_id).await {
        Ok(task) => task,
        Err(error) if error.code() == ErrorCode::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };

    let composes = match edges {
        Edges::Plan => Composes::Phase(LeasePurpose::Strategy),
        Edges::Resume { kind } => {
            Composes::Phase(kind.map_or(LeasePurpose::Implementation, Into::into))
        }
        // ADR-0016's inline planner, which ADR-0031 point 1 leases as
        // `strategy`; `start_run` moves it to the implementation's kind, after
        // the implementation was composed under this claim.
        Edges::Fresh if needs_planning(&task, mode) => Composes::PlannerThenImplementation,
        Edges::Fresh => Composes::Phase(LeasePurpose::Implementation),
    };
    let purpose = composes.purpose();

    let runner_id = candidate.runner_id;
    if let Some(refusal) = eligible(&mut tx, task_id, &candidate, composes)
        .await?
        .refusal()
    {
        return match door {
            Door::Named => Err(Error::invalid(refusal)),
            Door::Next => Ok(None),
        };
    }

    let leased = sqlx::query_scalar!(
        "SELECT generation FROM runner_leases WHERE task_id = ?1",
        task_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    if leased.is_some() {
        return Ok(None);
    }

    let moved = match (edges, task.run_state) {
        (Edges::Fresh, from @ (RunState::Idle | RunState::Failed | RunState::Cancelled)) => {
            transition(&mut tx, clock, task_id, from, RunState::Queued).await?
                && transition(&mut tx, clock, task_id, RunState::Queued, RunState::Running).await?
        }
        (Edges::Fresh, RunState::Queued) => {
            transition(&mut tx, clock, task_id, RunState::Queued, RunState::Running).await?
        }
        (Edges::Resume { .. }, RunState::WaitingRetry) => {
            transition(
                &mut tx,
                clock,
                task_id,
                RunState::WaitingRetry,
                RunState::Running,
            )
            .await?
        }
        (Edges::Plan, _) => true,
        _ => false,
    };
    if !moved {
        return Ok(None);
    }

    let generation = sqlx::query_scalar!(
        "UPDATE tasks SET lease_generation = lease_generation + 1 WHERE id = ?1
         RETURNING lease_generation",
        task_id,
    )
    .fetch_one(&mut *tx)
    .await?;

    let now = clock.now();
    let expires_at = term.expires_at(now);
    sqlx::query!(
        r#"INSERT INTO runner_leases
            (task_id, purpose, run_id, runner_id, generation, acquired_at, expires_at)
           VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6)"#,
        task_id,
        purpose,
        runner_id,
        generation,
        now,
        expires_at,
    )
    .execute(&mut *tx)
    .await?;

    let team_id = team_of_task(&mut tx, task_id).await?;
    tx.commit().await?;
    ctx.publish(ChangeEvent::tasks(team_id, [task_id.to_string()]));

    Ok(Some(Granted {
        generation,
        purpose,
    }))
}

// ---------------------------------------------------------------------------
// The fence
// ---------------------------------------------------------------------------

/// The live lease `lease` names, read inside the caller's transaction, or why
/// it is not this runner's to report under.
///
/// - `NotFound` for a task that does not exist, or that is not in the lease's
///   team or the transaction's scope, in the sentence a never-issued task
///   gets (D31 point 3);
/// - `Conflict` for a task with no lease row, or whose lease has another
///   generation or another runner.
pub(crate) async fn current(tx: &mut ScopedTx, lease: &LeaseRef, runner_id: &str) -> Result<Lease> {
    let scope = tx.scope().json();
    let row = sqlx::query!(
        r#"SELECT l.purpose AS "purpose?: LeasePurpose", l.run_id, l.runner_id AS "runner_id?",
                  l.generation AS "generation?", l.acquired_at AS "acquired_at?: DateTime<Utc>",
                  l.expires_at AS "expires_at: DateTime<Utc>"
             FROM tasks t LEFT JOIN runner_leases l ON l.task_id = t.id
            WHERE t.id = ?1 AND t.team_id = ?2
              AND t.team_id IN (SELECT value FROM json_each(?3))"#,
        lease.task_id,
        lease.team_id,
        scope,
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| Error::not_found(format!("no task with id {}", lease.task_id)))?;

    match (row.purpose, row.runner_id, row.generation, row.acquired_at) {
        (Some(purpose), Some(holder), Some(generation), Some(acquired_at))
            if generation == lease.generation && holder == runner_id =>
        {
            Ok(Lease {
                task_id: lease.task_id.clone(),
                purpose,
                run_id: row.run_id,
                runner_id: holder,
                generation,
                acquired_at,
                expires_at: row.expires_at,
            })
        }
        _ => Err(Error::conflict(format!(
            "this runner's lease on task {} (generation {}) is no longer the current one",
            lease.task_id, lease.generation
        ))),
    }
}

/// Renews every lease in `held` that is current for `runner_id`, in one
/// transaction, and answers the others in `fenced` (D31 point 4): one stale
/// lease does not cost the rest their renewal. A lease that never expires
/// stays that way. `cancel` is always empty in process.
pub(crate) async fn heartbeat(
    ctx: &ServiceContext,
    runner_id: &str,
    term: LeaseTerm,
    held: &[LeaseRef],
) -> Result<Heartbeat> {
    let expires_at = term.expires_at(ctx.clock.now());
    let mut fenced = Vec::new();

    let mut tx = ctx.begin_immediate().await?;
    for lease in held {
        match current(&mut tx, lease, runner_id).await {
            Ok(_) => {
                if let Some(expires_at) = expires_at {
                    sqlx::query!(
                        "UPDATE runner_leases SET expires_at = ?1
                          WHERE task_id = ?2 AND generation = ?3 AND expires_at IS NOT NULL",
                        expires_at,
                        lease.task_id,
                        lease.generation,
                    )
                    .execute(&mut *tx)
                    .await?;
                }
            }
            Err(error) if matches!(error.code(), ErrorCode::Conflict | ErrorCode::NotFound) => {
                fenced.push(lease.clone());
            }
            Err(error) => return Err(error),
        }
    }
    tx.commit().await?;

    Ok(Heartbeat {
        fenced,
        cancel: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// The lease's life after the claim
// ---------------------------------------------------------------------------

/// `start_run`'s half: the lease is now open for `run_id`, with that run's
/// kind as its purpose, under the same generation (D29 point 1's invariant).
/// Inside the transaction that opened the row.
pub(crate) async fn open_run(
    tx: &mut ScopedTx,
    lease: &LeaseRef,
    run_id: &str,
    kind: RunKind,
) -> Result<()> {
    let purpose = LeasePurpose::from(kind);
    sqlx::query!(
        "UPDATE runner_leases SET run_id = ?1, purpose = ?2
          WHERE task_id = ?3 AND generation = ?4",
        run_id,
        purpose,
        lease.task_id,
        lease.generation,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Ends a claim no `finish_run` ended, in one transaction: the fence, a task
/// still `running` taken to `failed` whatever the purpose, and the lease
/// deleted.
///
/// Keyed on the run state, not the purpose: a `Plan` claim never took an
/// edge, so its task is not `running` and is left alone (D31 point 4), while
/// an inline planner's task is, and lands `failed` as the implementation it
/// would have become does. A verdict already written (`waiting_retry`) is
/// kept. The pin is not touched.
pub(crate) async fn release(ctx: &ServiceContext, runner_id: &str, lease: &LeaseRef) -> Result<()> {
    let mut tx = ctx.begin_immediate().await?;
    current(&mut tx, lease, runner_id).await?;
    end_within(ctx, &mut tx, lease).await?;

    let team_id = team_of_task(&mut tx, &lease.task_id).await?;
    tx.commit().await?;
    ctx.publish(ChangeEvent::tasks(team_id, [lease.task_id.clone()]));
    Ok(())
}

/// [`release`]'s landing, inside a transaction that has already fenced
/// `lease`: a task still `running` taken to `failed`, and the lease deleted.
/// `run_context` ends a lease whose consent was lost with exactly this.
pub(crate) async fn end_within(
    ctx: &ServiceContext,
    tx: &mut ScopedTx,
    lease: &LeaseRef,
) -> Result<()> {
    let task = fetch_task_row(tx, &lease.task_id).await?;
    if task.run_state == RunState::Running {
        transition(
            tx,
            ctx.clock.as_ref(),
            &lease.task_id,
            RunState::Running,
            RunState::Failed,
        )
        .await?;
    }
    delete(tx, lease).await
}

/// `finish_run`'s landing, inside the transaction that lands the task: the
/// fence again, the next phase's eligibility, the task landed, and the lease
/// deleted or kept, with the pin set or cleared beside it.
///
/// - **`Continue`** keeps the lease, after [`eligible`] for the next phase's
///   purpose, which applies the model rule to the next phase's model. A
///   refusal turns it into `Released`, and the task lands as a finish that
///   does not continue lands it, in `in_review` (D31 point 4). A kept lease
///   is moved to the next phase's purpose with no run, as a claim leaves it,
///   so `run_context` judges consent on the composer the runner calls next
///   and not on the one that just finished (D29 point 1's invariant holds:
///   with `run_id` unset, the purpose names no run's kind).
/// - **`Released`** deletes the lease. The task is pinned to the holder when
///   the run was interrupted or the task lands `waiting_retry`, and a pin the
///   holder had is cleared otherwise.
///
/// Answers the step and the ids a rebalance of `in_review` renumbered.
pub(crate) async fn land(
    ctx: &ServiceContext,
    tx: &mut ScopedTx,
    candidate: &Candidate<'_>,
    lease: &LeaseRef,
    run: &Run,
    outcome: &RunOutcome,
    decision: Decision,
) -> Result<(NextStep, Vec<String>)> {
    let runner_id = candidate.runner_id;
    current(tx, lease, runner_id).await?;
    let task_id = lease.task_id.as_str();

    let mut decision = decision;
    if let NextStep::Continue { kind } = decision.next {
        let next = eligible(tx, task_id, candidate, Composes::Phase(kind.into())).await?;
        if let Some(refusal) = next.refusal() {
            tracing::info!(
                %task_id,
                run_id = %run.id,
                refusal,
                "the next phase is not this runner's; the loop ends here",
            );
            decision = Decision {
                next: NextStep::Released { resume_after: None },
                landing: Landing::InReview,
            };
        }
    }

    let rebalanced = outcome::land_within(ctx, tx, task_id, decision.landing).await?;

    if let NextStep::Continue { kind } = decision.next {
        let next = LeasePurpose::from(kind);
        sqlx::query!(
            "UPDATE runner_leases SET run_id = NULL, purpose = ?1
              WHERE task_id = ?2 AND generation = ?3",
            next,
            lease.task_id,
            lease.generation,
        )
        .execute(&mut **tx)
        .await?;
    }

    if let NextStep::Released { .. } = decision.next {
        delete(tx, lease).await?;
        let pins =
            outcome.status == RunStatus::Interrupted || decision.landing == Landing::WaitingRetry;
        if pins {
            pin(tx, task_id, Some(runner_id)).await?;
        } else {
            sqlx::query!(
                "UPDATE tasks SET pinned_runner_id = NULL
                  WHERE id = ?1 AND pinned_runner_id = ?2",
                task_id,
                runner_id,
            )
            .execute(&mut **tx)
            .await?;
        }
    }

    Ok((decision.next, rebalanced))
}

/// Writes the pin. The one statement that sets `tasks.pinned_runner_id`.
async fn pin(tx: &mut ScopedTx, task_id: &str, runner_id: Option<&str>) -> Result<()> {
    sqlx::query!(
        "UPDATE tasks SET pinned_runner_id = ?1 WHERE id = ?2",
        runner_id,
        task_id,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn delete(tx: &mut ScopedTx, lease: &LeaseRef) -> Result<()> {
    sqlx::query!(
        "DELETE FROM runner_leases WHERE task_id = ?1 AND generation = ?2",
        lease.task_id,
        lease.generation,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Pins `task_id` to `runner_id`, or unpins it with `None`, outside any
/// lease: the testing helper the contract and lease tests arrange a pin with,
/// and nothing in production.
#[cfg(any(test, feature = "testing"))]
pub async fn pin_for_test(
    ctx: &ServiceContext,
    task_id: &str,
    runner_id: Option<&str>,
) -> Result<()> {
    let mut tx = ctx.begin_immediate().await?;
    fetch_task_row(&mut tx, task_id).await?;
    pin(&mut tx, task_id, runner_id).await?;
    tx.commit().await
}

/// A lease on `task_id` for `runner_id` with no edge and no run, outside any
/// claim: what a test that only wants a worktree prepared holds while it
/// prepares one, so its task's run state does not move. Ended with
/// [`end_for_test`]. Nothing in production grants a lease this way.
#[cfg(any(test, feature = "testing"))]
pub async fn grant_for_test(
    ctx: &ServiceContext,
    task_id: &str,
    runner_id: &str,
) -> Result<LeaseRef> {
    let now = ctx.clock.now();
    let mut tx = ctx.begin_immediate().await?;
    fetch_task_row(&mut tx, task_id).await?;
    let generation = sqlx::query_scalar!(
        "UPDATE tasks SET lease_generation = lease_generation + 1 WHERE id = ?1
         RETURNING lease_generation",
        task_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        r#"INSERT INTO runner_leases
            (task_id, purpose, run_id, runner_id, generation, acquired_at, expires_at)
           VALUES (?1, 'strategy', NULL, ?2, ?3, ?4, NULL)"#,
        task_id,
        runner_id,
        generation,
        now,
    )
    .execute(&mut *tx)
    .await?;
    let team_id = team_of_task(&mut tx, task_id).await?;
    tx.commit().await?;
    Ok(LeaseRef::new(task_id, generation, team_id))
}

/// Ends a lease [`grant_for_test`] granted, touching nothing else.
#[cfg(any(test, feature = "testing"))]
pub async fn end_for_test(ctx: &ServiceContext, lease: &LeaseRef) -> Result<()> {
    let mut tx = ctx.begin_immediate().await?;
    delete(&mut tx, lease).await?;
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::PlannerBudget;
    use pretty_assertions::assert_eq;

    fn named(model: Option<&str>, effort: Option<&str>) -> (Option<String>, Option<String>) {
        (model.map(str::to_string), effort.map(str::to_string))
    }

    fn judged(models: &PhaseModels, purpose: LeasePurpose) -> (Option<String>, Option<String>) {
        let catalogue = Catalogue {
            planner: PlannerBudget {
                model: Some("haiku".to_string()),
                effort: Some("low".to_string()),
                ..PlannerBudget::default()
            },
            ..Catalogue::default()
        };
        let strategy = models.strategy_for(purpose, &catalogue);
        (strategy.model, strategy.effort)
    }

    #[test]
    fn the_ceiling_judges_each_purpose_on_what_that_phase_spawns_with() {
        let models = PhaseModels {
            strategy: Some("opus".to_string()),
            review: Some("sonnet".to_string()),
            effort: Some("high".to_string()),
            review_effort: Some("medium".to_string()),
        };

        assert_eq!(
            judged(&models, LeasePurpose::Implementation),
            named(Some("opus"), Some("high"))
        );
        assert_eq!(
            judged(&models, LeasePurpose::Fix),
            named(Some("opus"), Some("high"))
        );
        assert_eq!(
            judged(&models, LeasePurpose::Review),
            named(Some("sonnet"), Some("medium"))
        );
        assert_eq!(
            judged(&models, LeasePurpose::Strategy),
            named(Some("haiku"), Some("low")),
            "the planner's budget, never the card's choice"
        );
    }

    #[test]
    fn a_review_that_names_nothing_is_judged_on_the_tasks_own_strategy() {
        let models = PhaseModels {
            strategy: Some("opus".to_string()),
            review: None,
            effort: Some("high".to_string()),
            review_effort: None,
        };
        assert_eq!(
            judged(&models, LeasePurpose::Review),
            named(Some("opus"), Some("high"))
        );

        // Each half falls back on its own.
        let models = PhaseModels {
            review: Some("sonnet".to_string()),
            ..models
        };
        assert_eq!(
            judged(&models, LeasePurpose::Review),
            named(Some("sonnet"), Some("high"))
        );
    }
}
