//! A task's recorded strategy: the `strategy_plan` envelope, and the single
//! writer of the four columns that carry it (ADR-0016, seam-contract D17.3,
//! D17.7, D17.8).
//!
//! [`crate::strategy`] decides *what* a run should spawn with — the catalogue,
//! the defaults, the precedence chain — and holds no opinion about any
//! particular card. This module is the other half: it writes a decision onto
//! one, and it is the only thing that does. `strategy_plan`, `strategy_source`
//! and `strategy_updated_at` are written here and nowhere else, and `model` and
//! `effort` are written here as well as by
//! [`update_task`](crate::tasks::update_task) — one door for a planner, one for
//! a human, and no third.
//!
//! That matters more than tidiness. Three callers reach these columns: the
//! `set_task_strategy` MCP tool (a planner writing its proposal back), the
//! strategy run itself (recording that its planner failed), and the detail
//! panel (accepting or clearing one). Every invariant below — the mode guard,
//! the version stamp, the clock, the publication — would have to be restated in
//! each of them, and ADR-0006 exists because the restatement is where they stop
//! agreeing.
//!
//! # The envelope is text, and that is a decision
//!
//! [`crate::db::Task::strategy_plan`]'s own comment defers the choice to task
//! 020: the workspace `sqlx` is `default-features = false` without the `json`
//! feature, so neither `sqlx::types::Json` nor `#[sqlx(json)]` exists here, and
//! the column is either `TEXT` parsed with `serde_json` or a feature flag. It
//! stays `TEXT`, parsed with `serde_json` (D17.3). Turning the feature on would
//! buy nothing — SQLite stores JSON as text either way — and would cost a
//! rebuild of every query the macros already cache.
//!
//! Because it is a document rather than a message, [`StrategyPlan`] is
//! **tolerant on the way in**: unknown keys are kept out of the way rather than
//! rejected, so a version 2 envelope written by a later Rimaia does not make a
//! version 1 one unreadable, and a `failed` envelope may omit everything it has
//! no answer for. Task 021 reads this document, which is why D17.3 spells it
//! out in the seam contract rather than leaving it to this file.

use serde::{Deserialize, Serialize};

use crate::context::ServiceContext;
use crate::db::{StrategyMode, StrategySource, Task};
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::strategy::settings as strategy_settings;
use crate::strategy::{effective_strategy, StrategyDefaults};
use crate::tasks::service::fetch_task_row;

/// The envelope version this build writes. Stamped by
/// [`set_task_strategy`], never taken from the caller — a proposal that
/// arrived over MCP has no business claiming to be a shape this code does not
/// produce.
pub const STRATEGY_PLAN_VERSION: u32 = 1;

/// Whether the planner produced a proposal or failed trying.
///
/// A [`Failed`](StrategyPlanStatus::Failed) envelope is written on *every*
/// planner failure — non-zero exit, `max_turns`, a usage limit, a stream with
/// no `result`, or a run that finished without calling the tool — because the
/// re-plan guard reads the presence of the column and not its status
/// ([`needs_planning`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyPlanStatus {
    Proposed,
    Failed,
}

/// Whether the planner thinks the work fans out.
///
/// Rimaia never runs the phases (ADR-0016). The guidance is injected into the
/// implementation prompt and the agent runs them itself, in one session, with
/// its own subagents — which is why this is a hint on a document rather than a
/// field the scheduler reads.
///
/// `JsonSchema` for the reason [`crate::db::BoardColumn`] carries one: the
/// `set_task_strategy` tool takes this off the wire, and a tool advertising
/// `workflow: string` is a tool that gets `"parallel"` sent to it. Deriving it
/// here rather than mirroring the two values in [`crate::mcp::requests`] keeps
/// one list; the derive is inert everywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StrategyWorkflow {
    SingleAgent,
    MultiAgent,
}

/// One phase of a multi-agent proposal.
///
/// `model` and `effort` are per-phase suggestions for the *agent* to honour;
/// nothing here reaches `--model`. The flags a run spawns with are the
/// envelope's own top-level pair, resolved by [`crate::strategy::resolve`]
/// against the columns [`set_task_strategy`] wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyPhase {
    pub name: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    /// How many agents the phase wants. Defaults to one so that a planner that
    /// omits the key describes a phase rather than nothing.
    #[serde(default = "one_agent")]
    pub agents: u32,
    #[serde(default)]
    pub summary: String,
}

fn one_agent() -> u32 {
    1
}

/// The planner's own accounting.
///
/// It rides inside the envelope because a strategy run deliberately gets **no
/// `runs` row** (D17.5) — the panel still renders "Planner: 4 turns, $0.03",
/// and there is no row to read it off. `error` is the sentence the panel shows
/// on a failure, so it is written for a human and not for a parser.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StrategyPlanRun {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub num_turns: Option<i64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// Why the planner failed. `None` on a proposal.
    #[serde(default)]
    pub error: Option<String>,
}

/// The `strategy_plan` document, version 1, exactly as seam-contract D17.3
/// fixes it.
///
/// Field names are the *stored* spelling — `session_id`, `num_turns` — and not
/// this boundary's usual camelCase, because the document is written once and
/// read by two other things: the panel, which `JSON.parse`s the column
/// verbatim, and task 021. Renaming keys on the way through would make Settings
/// and the `sqlite3` CLI disagree about what is in the row.
///
/// Deliberately **not** `deny_unknown_fields`, unlike every request DTO in
/// [`crate::mcp::requests`]. A request is a message from a caller who can be
/// told they got it wrong; this is a document that a later version of Rimaia,
/// or a curious user with `sqlite3`, may have added a key to, and refusing to
/// read it would turn a forward-compatible document into a broken card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyPlan {
    /// Always [`STRATEGY_PLAN_VERSION`] on the way out — see that constant.
    /// Defaulted on the way in so that a hand-written envelope without one is
    /// still readable.
    #[serde(default = "current_version")]
    pub version: u32,
    pub status: StrategyPlanStatus,
    /// What the planner chose, and what [`set_task_strategy`] copies onto
    /// `tasks.model`. `None` on a failure, which is what makes the task fall
    /// back through the default chain.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub workflow: Option<StrategyWorkflow>,
    #[serde(default)]
    pub phases: Vec<StrategyPhase>,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub run: Option<StrategyPlanRun>,
}

fn current_version() -> u32 {
    STRATEGY_PLAN_VERSION
}

impl StrategyPlan {
    /// A proposal, with nothing recorded about the run that produced it yet.
    ///
    /// The planner calls the tool mid-run, so its turn count and cost are not
    /// known when the proposal is written; the strategy run amends the envelope
    /// with [`StrategyPlan::run`] once its process has exited.
    pub fn proposed(model: Option<String>, effort: Option<String>) -> Self {
        Self {
            version: STRATEGY_PLAN_VERSION,
            status: StrategyPlanStatus::Proposed,
            model,
            effort,
            workflow: None,
            phases: Vec::new(),
            rationale: None,
            run: None,
        }
    }

    /// A failure, carrying the sentence the panel renders.
    ///
    /// It names no model and no effort on purpose: written through
    /// [`set_task_strategy`] it nulls both columns, and a `planned` task with
    /// neither resolves to exactly what the plan calls "the values the
    /// `default` chain gives".
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            version: STRATEGY_PLAN_VERSION,
            status: StrategyPlanStatus::Failed,
            model: None,
            effort: None,
            workflow: None,
            phases: Vec::new(),
            rationale: None,
            run: Some(StrategyPlanRun {
                error: Some(error.into()),
                ..StrategyPlanRun::default()
            }),
        }
    }

    /// Reads what is stored on a task, tolerating anything that will not parse.
    ///
    /// Pass [`Task::strategy_plan`] straight in:
    /// `StrategyPlan::from_stored(task.strategy_plan.as_deref())`.
    ///
    /// Absent and unreadable collapse to the same `None`, which is the same
    /// tolerance the frontend's own `parseStrategyPlan` applies and the same
    /// rule `RunEnvironment::from_stored` states — a hand-edited row must not
    /// take a panel or a queue down (ADR-0003 makes the user a supported writer
    /// of this file). **[`needs_planning`] deliberately does not use this**: it
    /// reads the column, so an unreadable envelope still suppresses a re-plan.
    /// Repairs a proposal a planner serialized badly, before it is stored.
    ///
    /// # Why this exists
    ///
    /// Observed on a real run, not imagined. A planner decomposed a task into
    /// four phases and meant to say `multi_agent`, but emitted a corrupted
    /// closing tag for the `rationale` parameter — `</anionale>` — so the
    /// `workflow` parameter that followed was never parsed as one. It arrived
    /// as literal text *inside* the rationale, and `workflow` arrived as
    /// `None`. Nothing rejected the result: the card recorded four phases, no
    /// workflow, and a rationale ending in tool-call debris.
    ///
    /// A malformed tool call is a thing models do occasionally, and the
    /// planner is the one place in this product where a model's output is
    /// stored verbatim and later shown to a human and fed to another agent. So
    /// the repair belongs here, in the single writer, where the board and the
    /// MCP server both reach it (ADR-0006) — not in `mcp::requests`, where only
    /// one door would get it.
    fn repaired(mut self) -> Self {
        // Phases *are* the description of a fan-out. A proposal that names four
        // of them and no workflow is contradictory, and the contradiction is
        // costly in one direction only: `StrategyGuidance` renders the phase
        // list either way, but withholds "This work fans out, run it with
        // subagents" unless the workflow says so. The run would get the
        // decomposition and no instruction to use it.
        //
        // Only `None` is inferred. An explicit `single_agent` with phases is a
        // different statement — sequential steps for one agent — and is left
        // alone.
        if self.workflow.is_none() && !self.phases.is_empty() {
            self.workflow = Some(StrategyWorkflow::MultiAgent);
        }

        self.rationale = self.rationale.map(|text| sanitize_rationale(&text));

        self
    }

    pub fn from_stored(stored: Option<&str>) -> Option<Self> {
        let stored = stored?;
        match serde_json::from_str::<Self>(stored) {
            Ok(plan) => Some(plan),
            Err(error) => {
                tracing::warn!(
                    error = error.to_string(),
                    "unreadable strategy_plan; treating the task as having no proposal to render",
                );
                None
            }
        }
    }

    /// The bytes that go in the column, with [`STRATEGY_PLAN_VERSION`] stamped
    /// over whatever the caller had in the field.
    fn to_stored(&self) -> Result<String> {
        let stamped = Self {
            version: STRATEGY_PLAN_VERSION,
            ..self.clone()
        };
        serde_json::to_string(&stamped).map_err(|error| {
            Error::internal(format!("the strategy plan did not serialize: {error}"))
        })
    }
}

/// Whether this task's next run has to be preceded by a planner run.
///
/// **Safety-critical, and the reason it is one line with a long comment.** A
/// recorded proposal suppresses further planning *whether it succeeded or
/// failed* (D17.8). Without that asymmetry, a `planned` task whose planner
/// fails is replanned on every queue pass, forever — paying for the same
/// failure all night, which is the precise shape of overnight loss this product
/// exists to prevent. Editing the plan text does not re-trigger it;
/// [`clear_task_strategy`] is the only thing that lifts it, which is why
/// "Re-plan" is a button and not a side effect.
///
/// It reads the *column*, not a parse of it, for the same reason: an envelope
/// that will not deserialize is still a recorded answer, and treating it as
/// absent would put the loop back.
///
/// `mode` is the **resolved** mode — [`crate::strategy::effective_strategy`]'s,
/// not `task.strategy_mode`. A repository set to
/// [`Planned`](StrategyMode::Planned) plans every untouched card in it (D17.6),
/// and a version of this that read the row would never see that. It is a
/// parameter rather than a lookup so the function stays pure and the caller,
/// which has already resolved the strategy to decide what to spawn, does not
/// resolve it twice.
pub fn needs_planning(task: &Task, mode: StrategyMode) -> bool {
    mode == StrategyMode::Planned && task.strategy_plan.is_none()
}

/// Records a strategy on a task. **The single writer** of `strategy_plan`,
/// `strategy_source` and `strategy_updated_at`.
///
/// Writes `model` and `effort` from the envelope as well, because they are what
/// the implementation run actually spawns with: a proposal that lived only in
/// the JSON document would need every reader of the two columns to parse it
/// too. A `failed` envelope names neither, so both columns end up NULL and the
/// task falls back through the default chain — the fallback is a consequence of
/// the write rather than a separate code path that could disagree with it.
///
/// `strategy_mode` is **not** written. The mode may be inherited from the
/// repository or the global default (D17.6), and stamping the resolved value
/// onto the row would silently pin a card to a decision nobody made about it.
///
/// # The guard
///
/// A write whose `source` is [`StrategySource::Planner`] is refused unless the
/// task's *resolved* mode is [`Planned`](StrategyMode::Planned). That is
/// task 020's acceptance criterion 6, and D17.7's "what `set_task_strategy`
/// checks before letting a planner overwrite a strategy a human has taken
/// over": taking over means choosing a model or an effort, and
/// [`update_task`](crate::tasks::update_task) turns that into
/// [`Manual`](StrategyMode::Manual) on the same write. So the check is one
/// comparison rather than a second flag, and it holds for the run-scoped MCP
/// tool and for the runner's own failure annotation alike.
///
/// A [`User`](StrategySource::User) write is not guarded: the human is allowed
/// to record whatever they like about their own card.
#[tracing::instrument(
    skip_all,
    fields(source = ctx.source.as_str(), task_id = %task_id, status = ?plan.status)
)]
pub async fn set_task_strategy(
    ctx: &ServiceContext,
    task_id: &str,
    plan: StrategyPlan,
    source: StrategySource,
) -> Result<Task> {
    let plan = plan.repaired();
    let stored = plan.to_stored()?;

    // Read outside the transaction: `strategy::settings` takes the pool, and
    // holding a write transaction open across two more reads to defend against
    // a settings change landing in the same millisecond would cost more than
    // the race is worth. The defaults are configuration a human edits, not a
    // value another writer moves under us.
    let defaults = resolved_defaults(ctx, task_id).await?;

    let mut tx = ctx.pool.begin().await?;
    let current = fetch_task_row(&mut *tx, task_id).await?;

    if source == StrategySource::Planner {
        let mode = effective_strategy(&current, &defaults.repository, &defaults.global).mode;
        if mode != StrategyMode::Planned {
            return Err(Error::invalid(format!(
                "cannot record a planner's strategy for \"{title}\": it is in {mode} mode, \
                 so its strategy is the user's",
                title = current.title,
                mode = mode_word(mode),
            )));
        }
    }

    let now = ctx.clock.now();
    sqlx::query!(
        r#"UPDATE tasks SET strategy_plan = ?1, strategy_source = ?2, strategy_updated_at = ?3,
            model = ?4, effort = ?5, updated_at = ?3 WHERE id = ?6"#,
        stored,
        source,
        now,
        plan.model,
        plan.effort,
        task_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // Publish before the read-back, exactly as `create_task` does and for its
    // stated reason: the row is committed, so a failed re-read must not cost
    // the notification for a mutation that already happened (ADR-0018). It is
    // also how the panel learns a planner wrote back mid-run, which is the only
    // signal there is — the strategy run has no `runs` row to watch.
    ctx.publish(ChangeEvent::tasks([task_id.to_string()]));
    fetch_task_row(&ctx.pool, task_id).await
}

/// Takes authorship of a recorded proposal: `strategy_source` flips
/// [`Planner`](StrategySource::Planner) → [`User`](StrategySource::User)
/// (D17.7).
///
/// There is no `accepted` column and no `approved_at`. Accepting, editing and
/// overriding a proposal are the same claim of authorship with different
/// payloads — the two that carry a payload are
/// [`update_task`](crate::tasks::update_task)'s — and a column that only ever
/// said what `strategy_source` already says would be a second thing to keep in
/// step.
///
/// The proposal itself is untouched, so the panel keeps rendering the
/// rationale and the phases after it has been accepted; what changes is who
/// the run is executing on behalf of.
#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), task_id = %task_id))]
pub async fn accept_task_strategy(ctx: &ServiceContext, task_id: &str) -> Result<Task> {
    let mut tx = ctx.pool.begin().await?;
    let current = fetch_task_row(&mut *tx, task_id).await?;

    if current.strategy_plan.is_none() {
        return Err(Error::invalid(format!(
            "there is no proposal to accept on \"{title}\"",
            title = current.title,
        )));
    }

    let now = ctx.clock.now();
    sqlx::query!(
        r#"UPDATE tasks SET strategy_source = ?1, strategy_updated_at = ?2, updated_at = ?2
           WHERE id = ?3"#,
        StrategySource::User,
        now,
        task_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    ctx.publish(ChangeEvent::tasks([task_id.to_string()]));
    fetch_task_row(&ctx.pool, task_id).await
}

/// Forgets the recorded proposal — the panel's "Re-plan", and the **only**
/// thing that lifts [`needs_planning`]'s guard (D17.8).
///
/// `strategy_source` goes with it: a source naming who decided, on a row where
/// nothing is decided, is a card that renders "the planner owns this" with
/// nothing for it to own.
///
/// `model` and `effort` deliberately stay. They are still what this task runs
/// on until the next planner writes, and clearing them here would change what a
/// "Run now" pressed in the next second spawns with — a side effect of asking
/// for a re-plan, which is not what the button says. The next planner
/// overwrites both through [`set_task_strategy`] whether it succeeds or fails.
///
/// Idempotent: clearing a task that has nothing recorded is not an error, only
/// a no-op with a publication.
#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), task_id = %task_id))]
pub async fn clear_task_strategy(ctx: &ServiceContext, task_id: &str) -> Result<Task> {
    let mut tx = ctx.pool.begin().await?;
    fetch_task_row(&mut *tx, task_id).await?;

    let now = ctx.clock.now();
    sqlx::query!(
        r#"UPDATE tasks SET strategy_plan = NULL, strategy_source = NULL,
            strategy_updated_at = ?1, updated_at = ?1 WHERE id = ?2"#,
        now,
        task_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    ctx.publish(ChangeEvent::tasks([task_id.to_string()]));
    fetch_task_row(&ctx.pool, task_id).await
}

/// The two levels of default that stand behind one task, read together.
///
/// A struct rather than a tuple because the two are trivially swappable at a
/// call site and [`effective_strategy`] would resolve a global default as a
/// repository one without complaint.
pub(super) struct ResolvedDefaults {
    pub(super) repository: StrategyDefaults,
    pub(super) global: StrategyDefaults,
}

/// Reads the repository and global defaults that apply to `task_id`.
///
/// Takes a task id rather than a [`Task`] so the caller does not have to have
/// the row in hand before it starts its transaction — which is the order
/// [`set_task_strategy`] needs, since the settings accessors take the pool.
async fn resolved_defaults(ctx: &ServiceContext, task_id: &str) -> Result<ResolvedDefaults> {
    let task = fetch_task_row(&ctx.pool, task_id).await?;
    defaults_for_repository(ctx, &task.repository_id).await
}

/// The same, for a repository already known — the board read's path, where one
/// query answered for fifty cards and the repository ids are on them.
pub(super) async fn defaults_for_repository(
    ctx: &ServiceContext,
    repository_id: &str,
) -> Result<ResolvedDefaults> {
    Ok(ResolvedDefaults {
        repository: strategy_settings::repository_default(&ctx.pool, repository_id).await?,
        global: strategy_settings::global_default(&ctx.pool).await?,
    })
}

/// Cuts a rationale off at the first sign of tool-call debris.
///
/// The rationale is prose a human reads in the panel and, indirectly, context
/// another agent works from. When a tool call is serialized badly the tail of
/// this field is where the wreckage lands — see [`StrategyPlan::repaired`].
///
/// Two markers, both chosen because they are close to impossible in a sentence
/// about why a model was picked: `<parameter` opens a tool-call argument, and
/// `</` opens a closing tag. Everything from the earlier of them is dropped.
///
/// **This is a heuristic and it can cost a real sentence** — a rationale that
/// legitimately mentioned `</Suspense>` would be truncated at it. That trade is
/// deliberate: losing the tail of an explanation is a smaller harm than
/// rendering a half-parsed tool call to the user as if the planner had written
/// it. If it ever bites, the fix is a narrower marker, not a wider one.
fn sanitize_rationale(text: &str) -> String {
    let cut = ["<parameter", "</"]
        .iter()
        .filter_map(|marker| text.find(marker))
        .min();

    match cut {
        Some(at) => text[..at].trim_end().to_string(),
        None => text.to_string(),
    }
}

/// How a mode reads inside a refusal. The stored spelling, so the sentence and
/// the row agree.
fn mode_word(mode: StrategyMode) -> &'static str {
    match mode {
        StrategyMode::Default => "default",
        StrategyMode::Manual => "manual",
        StrategyMode::Planned => "planned",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Repairing a badly serialized proposal
    //
    // The literal below is what a real planner actually wrote on 2026-09-01,
    // trimmed. It meant `multi_agent`; a corrupted `</rationale>` closing tag
    // swallowed the parameter that would have said so.
    // -----------------------------------------------------------------------

    const MALFORMED_RATIONALE: &str = "Sonnet is sufficient for the full-stack work. \
The plan explicitly identifies four independent parts, enabling efficient parallel \
execution.</anionale>\n<parameter name=\"workflow\">multi_agent";

    fn proposal_with_phases(workflow: Option<StrategyWorkflow>) -> StrategyPlan {
        StrategyPlan {
            workflow,
            phases: vec![StrategyPhase {
                name: "Transcript viewer".to_string(),
                model: None,
                effort: None,
                agents: 1,
                summary: "Virtualized JSONL rendering".to_string(),
            }],
            ..StrategyPlan::proposed(Some("sonnet".to_string()), Some("high".to_string()))
        }
    }

    #[test]
    fn phases_with_no_workflow_are_read_as_a_fan_out() {
        // The costly half of the contradiction: `StrategyGuidance` renders the
        // phase list either way, but withholds "run it with subagents" unless
        // the workflow says so — so the run would get the decomposition and no
        // instruction to use it.
        let repaired = proposal_with_phases(None).repaired();

        assert_eq!(repaired.workflow, Some(StrategyWorkflow::MultiAgent));
    }

    #[test]
    fn an_explicit_single_agent_keeps_its_phases_without_becoming_a_fan_out() {
        // Sequential steps for one agent is a different statement, and a legal
        // one. Only `None` is inferred.
        let repaired = proposal_with_phases(Some(StrategyWorkflow::SingleAgent)).repaired();

        assert_eq!(repaired.workflow, Some(StrategyWorkflow::SingleAgent));
        assert_eq!(repaired.phases.len(), 1);
    }

    #[test]
    fn a_proposal_with_no_phases_is_left_alone() {
        let repaired = StrategyPlan::proposed(Some("haiku".to_string()), None).repaired();

        assert_eq!(repaired.workflow, None);
    }

    #[test]
    fn tool_call_debris_is_cut_off_the_end_of_a_rationale() {
        let repaired = StrategyPlan {
            rationale: Some(MALFORMED_RATIONALE.to_string()),
            ..proposal_with_phases(None)
        }
        .repaired();

        assert_eq!(
            repaired.rationale.as_deref(),
            Some(
                "Sonnet is sufficient for the full-stack work. The plan explicitly identifies \
four independent parts, enabling efficient parallel execution."
            ),
            "the sentence survives; the half-parsed tool call does not",
        );
    }

    #[test]
    fn a_clean_rationale_is_returned_unchanged() {
        let clean = "Straightforward metadata edits in one file; no architectural decisions.";
        let repaired = StrategyPlan {
            rationale: Some(clean.to_string()),
            ..StrategyPlan::proposed(None, None)
        }
        .repaired();

        assert_eq!(repaired.rationale.as_deref(), Some(clean));
    }
    use pretty_assertions::assert_eq;
    use serde_json::json;

    /// The document task 021 parses and the panel `JSON.parse`s, pinned key by
    /// key. `db::models` pins `Task`'s wire keys the same way and for the same
    /// reason: a renamed field would typecheck on both sides and render
    /// `undefined`.
    #[test]
    fn a_proposal_serializes_to_the_envelope_the_seam_contract_documents() {
        let plan = StrategyPlan {
            version: STRATEGY_PLAN_VERSION,
            status: StrategyPlanStatus::Proposed,
            model: Some("sonnet".to_string()),
            effort: Some("high".to_string()),
            workflow: Some(StrategyWorkflow::MultiAgent),
            phases: vec![StrategyPhase {
                name: "Schema".to_string(),
                model: Some("sonnet".to_string()),
                effort: Some("medium".to_string()),
                agents: 1,
                summary: "the migration".to_string(),
            }],
            rationale: Some("The plan names a migration and a command surface.".to_string()),
            run: Some(StrategyPlanRun {
                session_id: Some("session-1".to_string()),
                num_turns: Some(4),
                cost_usd: Some(0.031),
                error: None,
            }),
        };

        let wire: serde_json::Value =
            serde_json::from_str(&plan.to_stored().expect("a document must serialize"))
                .expect("what we just wrote must parse");

        assert_eq!(
            wire,
            json!({
                "version": 1,
                "status": "proposed",
                "model": "sonnet",
                "effort": "high",
                "workflow": "multi_agent",
                "phases": [{
                    "name": "Schema",
                    "model": "sonnet",
                    "effort": "medium",
                    "agents": 1,
                    "summary": "the migration",
                }],
                "rationale": "The plan names a migration and a command surface.",
                "run": {
                    "session_id": "session-1",
                    "num_turns": 4,
                    "cost_usd": 0.031,
                    "error": null,
                },
            })
        );
    }

    #[test]
    fn the_stored_version_is_this_builds_and_never_the_callers() {
        // A proposal arrives over MCP, where the caller is a language model.
        // Nothing it says about the envelope's version can be true of a
        // document it is not the one writing.
        let mut plan = StrategyPlan::proposed(Some("opus".to_string()), None);
        plan.version = 99;

        let wire: serde_json::Value =
            serde_json::from_str(&plan.to_stored().expect("a document must serialize"))
                .expect("what we just wrote must parse");

        assert_eq!(wire["version"], json!(1));
    }

    #[test]
    fn a_failure_names_no_model_so_the_task_falls_back_to_the_default_chain() {
        let plan = StrategyPlan::failed("stopped at max_turns without calling set_task_strategy");

        assert_eq!(plan.status, StrategyPlanStatus::Failed);
        assert_eq!(plan.model, None);
        assert_eq!(plan.effort, None);
        assert_eq!(
            plan.run.expect("a failure records its own reason").error,
            Some("stopped at max_turns without calling set_task_strategy".to_string()),
        );
    }

    #[test]
    fn an_envelope_from_a_later_version_still_reads_rather_than_failing() {
        // Forward compatibility is the whole reason this is not
        // `deny_unknown_fields`: task 021 or a later Rimaia adding a key must
        // not make today's cards unreadable.
        let plan = StrategyPlan::from_stored(Some(
            r#"{"version":2,"status":"proposed","model":"opus","reviewer":"claude"}"#,
        ))
        .expect("an unknown key is not a parse failure");

        assert_eq!(plan.version, 2, "read back as stored, not as this build's");
        assert_eq!(plan.model.as_deref(), Some("opus"));
    }

    #[test]
    fn a_hand_edited_envelope_that_is_not_json_reads_as_no_proposal_rather_than_failing() {
        assert_eq!(StrategyPlan::from_stored(Some("{ not json")), None);
        assert_eq!(StrategyPlan::from_stored(None), None);
    }

    #[test]
    fn a_phase_that_omits_its_agent_count_describes_one_agent() {
        let plan = StrategyPlan::from_stored(Some(
            r#"{"status":"proposed","phases":[{"name":"Schema"}]}"#,
        ))
        .expect("a sparse phase is still a phase");

        assert_eq!(
            plan.version, 1,
            "an envelope without a version is version 1"
        );
        assert_eq!(plan.phases[0].agents, 1);
        assert_eq!(plan.phases[0].summary, "");
    }

    // -----------------------------------------------------------------------
    // The re-plan guard (D17.8) — the one function in this file whose failure
    // mode is money.
    // -----------------------------------------------------------------------

    #[test]
    fn a_planned_task_with_nothing_recorded_is_planned() {
        assert!(needs_planning(&task(None), StrategyMode::Planned));
    }

    #[test]
    fn a_recorded_proposal_stops_the_task_being_planned_again() {
        assert!(!needs_planning(
            &task(Some(
                r#"{"version":1,"status":"proposed","model":"sonnet"}"#
            )),
            StrategyMode::Planned,
        ));
    }

    #[test]
    fn a_recorded_failure_also_stops_the_task_being_planned_again() {
        // The asymmetry that costs money if it is missing: without it, a
        // `planned` task whose planner fails is replanned on every queue pass
        // all night.
        assert!(!needs_planning(
            &task(Some(r#"{"version":1,"status":"failed"}"#)),
            StrategyMode::Planned,
        ));
    }

    #[test]
    fn an_unreadable_envelope_still_stops_the_task_being_planned_again() {
        // It reads the column, never a parse of it. An envelope somebody
        // corrupted with `sqlite3` is still a recorded answer, and treating it
        // as absent would put the loop straight back.
        assert!(!needs_planning(
            &task(Some("{ not json")),
            StrategyMode::Planned
        ));
    }

    #[test]
    fn a_task_that_is_not_planned_is_never_planned_however_empty_its_columns_are() {
        assert!(!needs_planning(&task(None), StrategyMode::Default));
        assert!(!needs_planning(&task(None), StrategyMode::Manual));
    }

    /// A row with everything [`needs_planning`] does not read left at its most
    /// boring value.
    fn task(strategy_plan: Option<&str>) -> Task {
        use crate::db::{BoardColumn, MutationSource, RunState};
        use crate::testing::test_epoch;

        Task {
            id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
            repository_id: "3f2b1c00-0000-4000-8000-000000000002".to_string(),
            title: "Wire the board to the store".to_string(),
            plan: None,
            extra_instructions: None,
            column: BoardColumn::Ready,
            position: 1.0,
            run_state: RunState::Idle,
            branch: None,
            worktree_path: None,
            strategy_mode: StrategyMode::Planned,
            model: None,
            effort: None,
            strategy_plan: strategy_plan.map(str::to_string),
            strategy_source: None,
            strategy_updated_at: None,
            created_at: test_epoch(),
            updated_at: test_epoch(),
            source: MutationSource::Ui,
        }
    }
}
