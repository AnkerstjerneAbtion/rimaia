//! The planning commands (task 020, task 023; ADR-0016, seam-contract D17).
//!
//! Local for good (D32's amendment on the planning commands): each claims on
//! this machine's runner and spawns a planner. The catalogue, the defaults, the
//! approval setting and accepting or clearing a proposal are board commands in
//! `rimaia_core::api::board::strategy`.
//!
//! Thin, like every other command module: whether a task may be planned, and
//! the slot it takes, are `rimaia_core::runner::strategy`'s, because the MCP
//! server's planning tools reach the same rules without passing through this
//! file (ADR-0006).

use rimaia_core::api::events::{plan_progress_wire, PlanProgressView, PlanResultView};
use rimaia_core::db::BoardColumn;
use rimaia_core::runner::strategy::{PlanPass, PlanSelection};
use rimaia_core::runner::{probe_cli, strategy as runner_strategy, CancelSignal, Starter};
use rimaia_core::scheduler::SlotOwner;
use rimaia_core::{Error, Result};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};

use crate::state::AppState;

/// Runs the planner for `task_id` now, and returns as soon as it is under
/// way — **not once it finishes.**
///
/// The panel's "Plan now", for a task whose planner failed or whose proposal was
/// cleared. Shaped exactly like Run now's start
/// ([`ShellRelay`](rimaia_runner::relay::ShellRelay)) and for the same
/// reasons: a real
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
/// The in-flight slot is what a second click fails against, and it is also
/// what makes "Plan now" and "Run now" refuse each other: they take the same
/// registry entry, so a planner in flight cannot be joined by an implementation
/// run in the same worktree.
///
/// That registry is now `rimaia_core::scheduler::InFlight` rather than a map in
/// `src-tauri`, and the queue takes its slots from the same value. Before
/// that, the queue claimed on the database row and the planner claimed in the
/// shell, so a planner and a queued run genuinely could both start for one task
/// — the hazard task 023 names in its Notes, closed as a consequence of there
/// being one registry rather than two.
///
/// Every rule this used to hold — the opt-in, the resolved mode, the slot
/// itself — is now `runner_strategy::claim_for_planning`'s, so this and the
/// `plan_task_strategy` MCP tool are two adapters over one function (ADR-0006).
/// The split into a claim and a run is what lets this one answer as soon as the
/// slot is taken while the tool awaits the whole planner.
#[tauri::command]
pub async fn plan_task_strategy(state: State<'_, AppState>, task_id: String) -> Result<()> {
    let machine = state.machine.clone();
    let paths = state.paths.clone();
    let config = state.runner.clone();

    // Awaited here so a click that cannot possibly plan anything gets an error
    // the button can render, rather than one that only reaches `tracing::error!`
    // inside a detached task nobody is watching.
    let board = state.board_port.clone();
    // This machine's own runner, asked by the person at it (ADR-0031 point 7).
    let claim = runner_strategy::claim_for_planning(
        Starter::at_runner(&state.context, &state.solo.runner_id),
        board.as_ref(),
        &state.machine,
        &state.in_flight,
        &task_id,
        SlotOwner::Manual,
    )
    .await?
    .map_err(|skip| Error::invalid(skip.message()))?;

    probe_cli(config.provider.as_ref(), &config.program).await?;

    tauri::async_runtime::spawn(async move {
        if let Err(error) =
            runner_strategy::plan_claimed(board.as_ref(), &machine, &paths, &config, claim).await
        {
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

    probe_cli(state.runner.provider.as_ref(), &state.runner.program).await?;

    let pass = runner_strategy::plan_all(
        Starter::at_runner(&state.context, &state.solo.runner_id),
        state.board_port.as_ref(),
        &state.machine,
        &state.paths,
        &state.runner,
        &state.in_flight,
        &selection.into(),
        &cancel,
        // The stream half of "streamed or collected", in the wire form core
        // gives it (task 048): a local emission, straight into this window,
        // and never onto a server's stream. A failure to emit is not a reason
        // to abandon a pass the user is paying for — the summary still arrives
        // when it resolves.
        &move |progress| {
            let wire = plan_progress_wire(&PlanProgressView::from(&progress));
            if let Err(error) = app.emit(wire.name, wire.data) {
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
/// the one `plan_all` checks between cards, and each card's own slot carries
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

/// The end-of-pass summary. Each line is core's [`PlanResultView`], the one
/// `plan-pass:progress` carries too.
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
