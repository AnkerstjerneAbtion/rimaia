//! Execution-strategy commands (task 020; ADR-0016, seam-contract D17).
//!
//! Thin, like every other handler module, and here that is load-bearing rather
//! than stylistic: the precedence chain, what an absent settings key means, what
//! a catalogue that will not parse falls back to, whether a planner may
//! overwrite a strategy a human took over — every one of those lives in
//! `crate::strategy` or `crate::tasks`, because the MCP server's
//! `set_task_strategy` tool reaches the same rules without passing through this
//! file (ADR-0006).
//!
//! **The mode is not here.** `strategy_mode` is a field on `TaskPatch` and
//! travels through the existing `update_task`, so setting it over MCP and
//! setting it from the panel are the same write — including D17.6's rule that a
//! model or effort arriving as a value flips the mode to `manual`. A command of
//! its own would have been a second door onto a rule that has to hold at both.
//!
//! Planning stays local in the shell: it claims on this machine's runner and
//! spawns a planner (D32's amendment on the planning commands).

use serde::{Deserialize, Serialize};

use super::{ByTaskId, NoArgs};
use crate::api::BoardRequest;
use crate::db::Task;
use crate::strategy::{
    catalogue, settings as strategy_settings, Catalogue, StrategyApproval, StrategyDefaults,
};
use crate::{tasks, Error, Result};

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
/// - `default_json` is what "Restore defaults" writes: the host provider's own
///   [`default_catalogue`](crate::runner::provider::ProviderProfile::default_catalogue),
///   serialized here rather than retyped in the frontend — a second copy of
///   the default list is a second thing to update when a model is added.
/// - `provider_info` is carried here, rather than through a new command, so
///   the origin label and the "No model — … chooses" placeholders (task 032)
///   have the active provider's name without a new registry row: every
///   component that needs it already reads this payload.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrategyCatalogueView {
    pub catalogue: Catalogue,
    pub json: String,
    pub default_json: String,
    pub provider_info: ProviderInfoView,
}

/// The name a `--model chooses for you` sentence interpolates. Not
/// [`ProviderId`](crate::runner::provider::ProviderId) itself: that identity is
/// frozen for storage, and this is free prose (task 032).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfoView {
    pub id: String,
    pub display_name: String,
}

/// The catalogue as stored, as parsed, and as it would be if restored.
///
/// `json` falls back to the host provider's own default when the key is
/// absent — the unseeded case, which is every fresh install. The textarea then
/// opens on the bytes an unconfigured Rimaia is actually using rather than on
/// an empty box the user would have to fill in from the documentation.
pub async fn get_strategy_catalogue(
    request: BoardRequest,
    _: NoArgs,
) -> Result<StrategyCatalogueView> {
    catalogue_view(&request).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetStrategyCatalogue {
    pub value: String,
}

/// Stores an edited catalogue, or refuses it with the parser's own message.
///
/// Answers with the new view so the panel never has to re-read to find out what
/// it stored — and so it renders the *trimmed* text `set_catalogue` actually
/// wrote rather than the textarea's own contents, which is the one case where
/// the two differ.
pub async fn set_strategy_catalogue(
    request: BoardRequest,
    args: SetStrategyCatalogue,
) -> Result<StrategyCatalogueView> {
    catalogue::set_catalogue(&request.ctx, &args.value).await?;

    catalogue_view(&request).await
}

async fn catalogue_view(request: &BoardRequest) -> Result<StrategyCatalogueView> {
    // The text as stored comes from the module that owns its meaning (D3), so
    // there is no second reader of the key here to drift from that module's
    // own reader and writer.
    let stored = catalogue::stored_text(&request.ctx).await?;

    let provider = &request.host.provider;
    let default_json = serde_json::to_string_pretty(&provider.default_catalogue)
        .map_err(|error| Error::internal(error.to_string()))?;

    Ok(StrategyCatalogueView {
        catalogue: catalogue::catalogue(&request.ctx, provider).await?,
        json: stored.unwrap_or_else(|| default_json.clone()),
        default_json,
        provider_info: ProviderInfoView {
            id: provider.id.as_str().to_string(),
            display_name: provider.display_name.to_string(),
        },
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetStrategyDefaults {
    pub repository_id: Option<String>,
}

/// One repository's default strategy, or the global one when `repository_id` is
/// absent.
///
/// One command for both levels because there is one struct, one parser and one
/// precedence chain behind them (`strategy::settings`' own argument). A
/// `get_global_strategy_defaults` beside a `get_repository_strategy_defaults`
/// would be two adapters over one function.
pub async fn get_strategy_defaults(
    request: BoardRequest,
    args: GetStrategyDefaults,
) -> Result<StrategyDefaults> {
    match args.repository_id.as_deref() {
        Some(repository_id) => {
            strategy_settings::repository_default(&request.ctx, repository_id).await
        }
        None => strategy_settings::global_default(&request.ctx).await,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetStrategyDefaults {
    pub repository_id: Option<String>,
    pub value: StrategyDefaults,
}

/// Writes one repository's default strategy, or the global one.
///
/// There is no "clear": a [`StrategyDefaults`] whose mode is `default` and whose
/// model and effort are absent *is* "no opinion", and it is what an absent key
/// already reads as. A repository row that says nothing and one that has never
/// been written are the same answer, so the panel needs only one control.
pub async fn set_strategy_defaults(request: BoardRequest, args: SetStrategyDefaults) -> Result<()> {
    match args.repository_id.as_deref() {
        Some(repository_id) => {
            strategy_settings::set_repository_default(&request.ctx, repository_id, &args.value)
                .await
        }
        None => strategy_settings::set_global_default(&request.ctx, &args.value).await,
    }
}

/// Whether a proposal runs on its own or waits for a human. Absent reads as
/// `automatic`.
pub async fn get_strategy_approval(request: BoardRequest, _: NoArgs) -> Result<StrategyApproval> {
    strategy_settings::approval(&request.ctx).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetStrategyApproval {
    pub value: StrategyApproval,
}

/// Stores the approval setting. **Nothing reads it yet** — the gate itself lands
/// after tasks 011 and 012, so that it does not contend with their `selection`
/// restructure. Stored now because Settings renders the control now, and a radio
/// group that forgets its answer on relaunch is worse than no radio group.
pub async fn set_strategy_approval(request: BoardRequest, args: SetStrategyApproval) -> Result<()> {
    strategy_settings::set_approval(&request.ctx, args.value).await
}

/// Takes authorship of the proposal on `task_id`: `strategy_source` flips from
/// `planner` to `user` (D17.7).
///
/// The proposal itself is not rewritten and no column is added — accepting,
/// editing and overriding are the same claim of authorship with different
/// payloads, and the two that carry a payload are `update_task`'s.
pub async fn accept_task_strategy(request: BoardRequest, args: ByTaskId) -> Result<Task> {
    tasks::accept_task_strategy(&request.ctx, &args.task_id).await
}

/// Clears the recorded proposal, which is the only thing that lifts the
/// re-plan guard (D17.8).
///
/// A `planned` task with no `strategy_plan` is planned again on its next run;
/// with one — successful *or* failed — it is not. That asymmetry is what stops a
/// broken planner from being paid for on every queue pass all night, and it is
/// why "Re-plan" is a deliberate button rather than a side effect of editing the
/// plan text.
pub async fn clear_task_strategy(request: BoardRequest, args: ByTaskId) -> Result<Task> {
    tasks::clear_task_strategy(&request.ctx, &args.task_id).await
}
