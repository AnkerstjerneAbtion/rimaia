//! Execution-strategy commands (task 020; ADR-0016, seam-contract D17).
//!
//! Thin, like every other command module, and here that is load-bearing rather
//! than stylistic: the precedence chain, what an absent settings key means, what
//! a catalogue that will not parse falls back to, whether a planner may
//! overwrite a strategy a human took over — every one of those lives in
//! `rimaia_core::strategy` or `rimaia_core::tasks`, because the MCP server's
//! `set_task_strategy` tool reaches the same rules without passing through this
//! file (ADR-0006).
//!
//! **The mode is not here.** `strategy_mode` is a field on `TaskPatch` and
//! travels through the existing `update_task`, so setting it over MCP and
//! setting it from the panel are the same write — including D17.6's rule that a
//! model or effort arriving as a value flips the mode to `manual`. A command of
//! its own would have been a second door onto a rule that has to hold at both.

use rimaia_core::db::{settings, BoardColumn, Task};
use rimaia_core::runner::strategy::{
    PlanOutcome, PlanPass, PlanProgress, PlanResult, PlanSelection,
};
use rimaia_core::runner::{probe_cli, strategy as runner_strategy, CancelSignal};
use rimaia_core::scheduler::LeaseOwner;
use rimaia_core::strategy::{
    catalogue, settings as strategy_settings, Catalogue, StrategyApproval, StrategyDefaults,
    DEFAULT_CATALOGUE_JSON,
};
use rimaia_core::{tasks, Error, Result};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};

use crate::state::AppState;

/// Everything Settings' catalogue editor and every strategy dropdown need, in
/// one read.
///
/// Three fields rather than one because the panel and the Settings textarea want
/// different things out of the same key, and asking twice would let them
/// disagree:
///
/// - `catalogue` is the *parsed* value, so the dropdowns render whatever the
///   tolerant reader made of the stored text — including the built-in list when
///   a hand-edited row will not parse. Parsing it in TypeScript instead would
///   put that fallback rule in two languages.
/// - `json` is the stored text **verbatim**, because
///   [`catalogue::set_catalogue`] deliberately stores what the user typed: their
///   key order and indentation are what they should see when they open Settings
///   again.
/// - `default_json` is what "Restore defaults" writes. It crosses the boundary
///   rather than being retyped in the frontend for the reason
///   [`DEFAULT_CATALOGUE_JSON`] is exported at all — a second copy of the
///   default list is a second thing to update when a model is added.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrategyCatalogueView {
    pub catalogue: Catalogue,
    pub json: String,
    pub default_json: String,
}

/// The catalogue as stored, as parsed, and as it would be if restored.
///
/// `json` falls back to [`DEFAULT_CATALOGUE_JSON`] when the key is absent — the
/// unseeded case, which is every fresh install. The textarea then opens on the
/// bytes an unconfigured Rimaia is actually using rather than on an empty box
/// the user would have to fill in from the documentation.
#[tauri::command]
pub async fn get_strategy_catalogue(state: State<'_, AppState>) -> Result<StrategyCatalogueView> {
    let pool = &state.context.pool;
    // The key constant comes from the module that owns its meaning (D3), so
    // there is no second spelling of it here to drift from that module's own
    // reader and writer.
    let stored = settings::get(pool, catalogue::STRATEGY_CATALOGUE).await?;

    Ok(StrategyCatalogueView {
        catalogue: catalogue::catalogue(pool).await?,
        json: stored.unwrap_or_else(|| DEFAULT_CATALOGUE_JSON.to_string()),
        default_json: DEFAULT_CATALOGUE_JSON.to_string(),
    })
}

/// Stores an edited catalogue, or refuses it with the parser's own message.
///
/// Answers with the new view so the panel never has to re-read to find out what
/// it stored — and so it renders the *trimmed* text `set_catalogue` actually
/// wrote rather than the textarea's own contents, which is the one case where
/// the two differ.
#[tauri::command]
pub async fn set_strategy_catalogue(
    state: State<'_, AppState>,
    value: String,
) -> Result<StrategyCatalogueView> {
    catalogue::set_catalogue(&state.context, &value).await?;

    get_strategy_catalogue(state).await
}

/// One repository's default strategy, or the global one when `repository_id` is
/// absent.
///
/// One command for both levels because there is one struct, one parser and one
/// precedence chain behind them (`strategy::settings`' own argument). A
/// `get_global_strategy_defaults` beside a `get_repository_strategy_defaults`
/// would be two adapters over one function.
#[tauri::command]
pub async fn get_strategy_defaults(
    state: State<'_, AppState>,
    repository_id: Option<String>,
) -> Result<StrategyDefaults> {
    match repository_id.as_deref() {
        Some(repository_id) => {
            strategy_settings::repository_default(&state.context.pool, repository_id).await
        }
        None => strategy_settings::global_default(&state.context.pool).await,
    }
}

/// Writes one repository's default strategy, or the global one.
///
/// There is no "clear": a [`StrategyDefaults`] whose mode is `default` and whose
/// model and effort are absent *is* "no opinion", and it is what an absent key
/// already reads as. A repository row that says nothing and one that has never
/// been written are the same answer, so the panel needs only one control.
#[tauri::command]
pub async fn set_strategy_defaults(
    state: State<'_, AppState>,
    repository_id: Option<String>,
    value: StrategyDefaults,
) -> Result<()> {
    match repository_id.as_deref() {
        Some(repository_id) => {
            strategy_settings::set_repository_default(&state.context, repository_id, &value).await
        }
        None => strategy_settings::set_global_default(&state.context, &value).await,
    }
}

/// Whether a proposal runs on its own or waits for a human. Absent reads as
/// `automatic`.
#[tauri::command]
pub async fn get_strategy_approval(state: State<'_, AppState>) -> Result<StrategyApproval> {
    strategy_settings::approval(&state.context.pool).await
}

/// Stores the approval setting. **Nothing reads it yet** — the gate itself lands
/// after tasks 011 and 012, so that it does not contend with their `selection`
/// restructure. Stored now because Settings renders the control now, and a radio
/// group that forgets its answer on relaunch is worse than no radio group.
#[tauri::command]
pub async fn set_strategy_approval(
    state: State<'_, AppState>,
    value: StrategyApproval,
) -> Result<()> {
    strategy_settings::set_approval(&state.context, value).await
}

/// Takes authorship of the proposal on `task_id`: `strategy_source` flips from
/// `planner` to `user` (D17.7).
///
/// The proposal itself is not rewritten and no column is added — accepting,
/// editing and overriding are the same claim of authorship with different
/// payloads, and the two that carry a payload are `update_task`'s.
#[tauri::command]
pub async fn accept_task_strategy(state: State<'_, AppState>, task_id: String) -> Result<Task> {
    tasks::accept_task_strategy(&state.context, &task_id).await
}

/// Clears the recorded proposal, which is the only thing that lifts the
/// re-plan guard (D17.8).
///
/// A `planned` task with no `strategy_plan` is planned again on its next run;
/// with one — successful *or* failed — it is not. That asymmetry is what stops a
/// broken planner from being paid for on every queue pass all night, and it is
/// why "Re-plan" is a deliberate button rather than a side effect of editing the
/// plan text.
#[tauri::command]
pub async fn clear_task_strategy(state: State<'_, AppState>, task_id: String) -> Result<Task> {
    tasks::clear_task_strategy(&state.context, &task_id).await
}

/// Runs the planner for `task_id` now, and returns as soon as it is under
/// way — **not once it finishes.**
///
/// The panel's "Plan now", for a task whose planner failed or whose proposal was
/// cleared. Shaped exactly like [`start_task_run`](super::runs::start_task_run)
/// and for the same reasons, which that function's doc gives in full: a real
/// `claude` process runs for however long it takes, so awaiting it here would
/// hold the button's promise open for the whole planner run, and the caller
/// learns how it went the way every other view does — `tasks:changed` once
/// `set_task_strategy` writes the proposal onto the card (ADR-0018).
///
/// The two refusals that can be settled before anything is spawned — ADR-0012's
/// repository opt-in and the `claude` prerequisite — are awaited here so a click
/// that cannot possibly plan anything gets an error the button can render,
/// rather than one that only reaches `tracing::error!` inside a detached task
/// nobody is watching. Both are read-only, and the planner performs them again
/// as part of its own contract.
///
/// The in-flight lease is what a second click fails against, and it is also
/// what makes "Plan now" and "Run now" refuse each other: they take the same
/// registry entry, so a planner in flight cannot be joined by an implementation
/// run in the same worktree.
///
/// That registry is now `rimaia_core::scheduler::InFlight` rather than a map in
/// `src-tauri`, and the queue takes its leases from the same value. Before
/// that, the queue claimed on the database row and the planner claimed in the
/// shell, so a planner and a queued run genuinely could both start for one task
/// — the hazard task 023 names in its Notes, closed as a consequence of there
/// being one registry rather than two.
///
/// Every rule this used to hold — the opt-in, the resolved mode, the lease
/// itself — is now `runner_strategy::claim_for_planning`'s, so this and the
/// `plan_task_strategy` MCP tool are two adapters over one function (ADR-0006).
/// The split into a claim and a run is what lets this one answer as soon as the
/// slot is taken while the tool awaits the whole planner.
#[tauri::command]
pub async fn plan_task_strategy(state: State<'_, AppState>, task_id: String) -> Result<()> {
    let context = state.context.clone();
    let paths = state.paths.clone();
    let config = state.runner.clone();

    // Awaited here so a click that cannot possibly plan anything gets an error
    // the button can render, rather than one that only reaches `tracing::error!`
    // inside a detached task nobody is watching.
    let claim = runner_strategy::claim_for_planning(
        &context,
        &state.in_flight,
        &task_id,
        LeaseOwner::Manual,
    )
    .await?
    .map_err(|skip| Error::invalid(skip.message()))?;

    probe_cli(&config.program).await?;

    tauri::async_runtime::spawn(async move {
        if let Err(error) = runner_strategy::plan_claimed(&context, &paths, &config, claim).await {
            tracing::error!(
                %task_id, %error,
                "a strategy run could not be started or supervised",
            );
        }
    });

    Ok(())
}

/// Plans a whole selection — a column, a repository, or a hand-picked set —
/// one planner at a time, and answers with the summary (task 023).
///
/// **Awaited, unlike [`plan_task_strategy`].** A pass is the thing the user
/// stays to watch: they chose to spend the money and the summary is the reason
/// they ran it. Live progress arrives as `plan-pass:progress` events while this
/// is outstanding (seam-contract D7), and the resolved value is the end-of-pass
/// summary.
///
/// One pass at a time. A second call while one is running is refused rather
/// than queued: two passes would be the fan-out task 023's Notes exist to
/// refuse, arrived at by a different route.
#[tauri::command]
pub async fn plan_tasks_strategy(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    selection: PlanSelectionInput,
) -> Result<PlanPassView> {
    let cancel = CancelSignal::new();
    {
        let mut current = state
            .plan_pass
            .lock()
            .expect("the plan-pass mutex is poisoned");
        if current
            .as_ref()
            .is_some_and(|signal| !signal.is_cancelled())
        {
            return Err(Error::invalid(
                "a planning pass is already running; wait for it to finish or cancel it",
            ));
        }
        *current = Some(cancel.clone());
    }

    probe_cli(&state.runner.program).await?;

    let pass = runner_strategy::plan_all(
        &state.context,
        &state.paths,
        &state.runner,
        &state.in_flight,
        &selection.into(),
        &cancel,
        // The stream half of "streamed or collected". A failure to emit is not
        // a reason to abandon a pass the user is paying for — the summary still
        // arrives when it resolves.
        &move |progress| {
            if let Err(error) =
                app.emit(PLAN_PASS_PROGRESS_EVENT, PlanProgressView::from(&progress))
            {
                tracing::warn!(%error, "could not emit the planning pass progress");
            }
        },
    )
    .await;

    state
        .plan_pass
        .lock()
        .expect("the plan-pass mutex is poisoned")
        .take();

    Ok(PlanPassView::from(&pass?))
}

/// Stops the pass before its next planner, leaving every proposal already
/// written in place.
///
/// The planner currently running is asked to stop too — the pass's signal is
/// the one `plan_all` checks between cards, and each card's own lease carries
/// its own. A pass that has already finished is a no-op rather than an error:
/// the user pressed Cancel a second too late, which is not a mistake to report.
#[tauri::command]
pub async fn cancel_plan_pass(state: State<'_, AppState>) -> Result<()> {
    if let Some(signal) = state
        .plan_pass
        .lock()
        .expect("the plan-pass mutex is poisoned")
        .as_ref()
    {
        signal.cancel();
    }
    Ok(())
}

/// What the board sends [`plan_tasks_strategy`].
///
/// Mirrors [`PlanSelection`] and exists only to case its fields the way this
/// boundary does — the core type is what both surfaces resolve through, so the
/// board and the MCP tool cannot disagree about what "the ready column" means.
/// The same split [`crate::commands::worktree::RemovalAuthorizationInput`]
/// makes, for the same reason.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct PlanSelectionInput {
    pub column: Option<BoardColumn>,
    pub repository_id: Option<String>,
    pub task_ids: Vec<String>,
}

impl From<PlanSelectionInput> for PlanSelection {
    fn from(input: PlanSelectionInput) -> Self {
        PlanSelection {
            column: input.column,
            repository_id: input.repository_id,
            task_ids: input.task_ids,
        }
    }
}

/// The Tauri event carrying live pass progress (seam-contract D7).
pub const PLAN_PASS_PROGRESS_EVENT: &str = "plan-pass:progress";

/// One card's line, `camelCase` for this boundary.
///
/// A projection rather than [`PlanResult`] re-serialized, for the reason
/// `mcp::responses` gives: the wire shape is the client's contract, and a core
/// enum reshaped by serde would make every rename a breaking change to the
/// window.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanResultView {
    pub task_id: String,
    pub title: String,
    /// `planned`, `skipped`, `failed` or `cancelled`.
    pub outcome: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub rationale: Option<String>,
    pub cost_usd: Option<f64>,
    pub skip: Option<String>,
    pub reason: Option<String>,
}

impl From<&PlanResult> for PlanResultView {
    fn from(result: &PlanResult) -> Self {
        let mut view = Self {
            task_id: result.task_id.clone(),
            title: result.title.clone(),
            outcome: "cancelled".to_string(),
            model: None,
            effort: None,
            rationale: None,
            cost_usd: None,
            skip: None,
            reason: None,
        };
        match &result.outcome {
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

/// The end-of-pass summary.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanPassView {
    pub results: Vec<PlanResultView>,
    pub planned: usize,
    pub skipped: usize,
    pub spent_usd: f64,
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

/// What one `plan-pass:progress` event carries: the card that just finished,
/// how far through the pass it was, and what has been spent so far.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanProgressView {
    /// 1-based, because it is rendered as "3 of 10".
    pub completed: usize,
    pub total: usize,
    pub spent_usd: f64,
    pub result: PlanResultView,
}

impl From<&PlanProgress<'_>> for PlanProgressView {
    fn from(progress: &PlanProgress<'_>) -> Self {
        Self {
            completed: progress.index + 1,
            total: progress.total,
            spent_usd: progress.spent_usd,
            result: PlanResultView::from(progress.result),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The board's half of task 023's "the same selection through the MCP tool
    /// and through the button produces the same set of tasks".
    ///
    /// `crates/core/tests/runner_strategy.rs` owns the other half — it drives
    /// the MCP wire shape through its real schema and resolves the set — and
    /// the two meet on one [`PlanSelection`], which is the only thing
    /// `selected_tasks` ever sees. This pins that the toolbar's `camelCase`
    /// payload lands on exactly that value.
    #[test]
    fn the_boards_payload_converts_to_the_same_selection_the_mcp_request_does() {
        let input: PlanSelectionInput = serde_json::from_value(serde_json::json!({
            "column": "ready",
            "repositoryId": "repo-1",
        }))
        .expect("the toolbar's payload deserializes");

        assert_eq!(
            PlanSelection::from(input),
            PlanSelection {
                column: Some(BoardColumn::Ready),
                repository_id: Some("repo-1".to_string()),
                task_ids: Vec::new(),
            },
        );
    }

    #[test]
    fn a_hand_picked_set_survives_the_boundary_in_the_order_it_was_sent() {
        let input: PlanSelectionInput = serde_json::from_value(serde_json::json!({
            "taskIds": ["b", "a"],
        }))
        .expect("a hand-picked selection deserializes");

        assert_eq!(
            PlanSelection::from(input),
            PlanSelection {
                column: None,
                repository_id: None,
                task_ids: vec!["b".to_string(), "a".to_string()],
            },
        );
    }

    /// Every field defaults, so an omitted one is not a deserialization error —
    /// but *all* of them omitted is refused by `selected_tasks`, in core, where
    /// both surfaces meet it.
    #[test]
    fn an_omitted_field_is_absent_rather_than_a_deserialization_error() {
        let input: PlanSelectionInput =
            serde_json::from_value(serde_json::json!({})).expect("an empty selection deserializes");

        assert_eq!(PlanSelection::from(input), PlanSelection::default());
    }
}
