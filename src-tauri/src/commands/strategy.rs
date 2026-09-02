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

use rimaia_core::db::{settings, Task};
use rimaia_core::runner::{probe_cli, strategy as runner_strategy};
use rimaia_core::scheduler::LeaseOwner;
use rimaia_core::strategy::{
    catalogue, settings as strategy_settings, Catalogue, StrategyApproval, StrategyDefaults,
    DEFAULT_CATALOGUE_JSON,
};
use rimaia_core::{repo, tasks, Error, Result};
use serde::Serialize;
use tauri::State;

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
/// — the hazard task 023 names in its Notes, closed here as a consequence of
/// there being one registry rather than two.
#[tauri::command]
pub async fn plan_task_strategy(state: State<'_, AppState>, task_id: String) -> Result<()> {
    let context = state.context.clone();
    let paths = state.paths.clone();
    let config = state.runner.clone();

    // Read before the lease: a lease is taken per repository, and this is the
    // only thing that knows which one. Both reads are read-only.
    let detail = tasks::get_task(&context, &task_id).await?;
    let repository = repo::get(&context, &detail.task.repository_id).await?;

    // `Manual`, because "Plan now" is a button: a Stop pressed on the queue
    // must not kill a planner the operator started deliberately.
    let lease = state
        .in_flight
        .acquire_unbounded(&task_id, &repository.id, LeaseOwner::Manual)
        .map_err(|refused| Error::invalid(refused.message()))?;
    let cancel = lease.cancel_signal();

    repo::ensure_unattended_runs_allowed(&repository)?;
    probe_cli(&config.program).await?;

    tauri::async_runtime::spawn(async move {
        let _lease = lease;
        if let Err(error) =
            runner_strategy::plan_task(&context, &paths, &config, &task_id, cancel).await
        {
            tracing::error!(
                %task_id, %error,
                "a strategy run could not be started or supervised",
            );
        }
    });

    Ok(())
}
