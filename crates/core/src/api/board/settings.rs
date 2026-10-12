//! The team's base instructions and what runs have cost (task 006, ADR-0009,
//! seam-contract D3).
//!
//! Every rule — what an absent key means, whose team a setting is — lives in
//! `crate::db::settings` and `crate::runner::outcome`. The run environment, the
//! strategy ceiling and the prompt preview are this machine's, and stay local
//! commands in the shell.

use serde::Deserialize;

use super::NoArgs;
use crate::api::BoardRequest;
use crate::db::settings;
use crate::runner::outcome::{self, RunCostSummary};
use crate::Result;

pub async fn get_base_instructions(request: BoardRequest, _: NoArgs) -> Result<String> {
    settings::base_instructions(&request.ctx).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBaseInstructions {
    pub value: String,
}

/// Replaces the team's base instructions. Never touches a run already
/// composed and stored: `runs.prompt` holds its own copy per ADR-0009, and
/// composition is pure, so there is no path from this write to a prompt
/// already on a `runs` row.
pub async fn set_base_instructions(request: BoardRequest, args: SetBaseInstructions) -> Result<()> {
    settings::set_base_instructions(&request.ctx, &args.value).await
}

/// What the caller's teams' runs have actually cost, so the environment toggle
/// can state its overhead as a share of real work rather than as the spike's
/// ratio. The provider's figure and name come from the host's profile.
///
/// A separate command rather than a field on `get_run_environment`: the answer
/// changes every time a run finishes, where the setting changes when a human
/// edits it, and folding them together would make the cheap read as volatile
/// as the expensive one.
pub async fn get_run_cost_summary(request: BoardRequest, _: NoArgs) -> Result<RunCostSummary> {
    outcome::observed_run_cost(&request.ctx, &request.host.provider).await
}
