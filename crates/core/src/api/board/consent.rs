//! Acceptance and the consent read (task 045, ADR-0032).
//!
//! Thin adapters over `crate::consent`, which the MCP tools of the same names
//! call too (ADR-0021 parity). Assignment lives with the other task writes in
//! `tasks.rs`, and the team ceiling with the repository commands.

use serde::Deserialize;

use crate::api::BoardRequest;
use crate::consent::{self, pieces::ContentKind, TaskConsent};
use crate::Result;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptContent {
    pub team_id: String,
    pub task_id: Option<String>,
    pub kind: ContentKind,
    pub revision: String,
}

/// Records that the user read `revision` of one piece of content and accepts
/// running it. `task_id` is absent exactly for the team's base and review
/// instructions; only the current revision is accepted.
pub async fn accept_content(request: BoardRequest, args: AcceptContent) -> Result<()> {
    consent::accept(
        &request.ctx,
        &args.team_id,
        args.task_id.as_deref(),
        args.kind,
        &args.revision,
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTaskConsent {
    pub task_id: String,
    pub runner_id: String,
}

/// Why `runner_id`, one of the user's own runners, would or would not take
/// `task_id`: eligibility, the pin, the team ceiling and every missing piece.
pub async fn get_task_consent(request: BoardRequest, args: GetTaskConsent) -> Result<TaskConsent> {
    consent::status(&request.ctx, &args.task_id, &args.runner_id).await
}
