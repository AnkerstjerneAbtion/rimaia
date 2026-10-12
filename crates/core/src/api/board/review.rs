//! The review verdicts, the two reads that inform them, the digest marker
//! (task 034, ADR-0006), the read of what review runs found (task 035), and
//! the review loop's configuration (task 021).
//!
//! One line each over `crate::review`, `review_loop::config` and
//! `tasks::dependents_of`. Every refusal, the note's format and the digest's
//! order are core's, so the MCP tools of the same names cannot disagree with
//! these.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use super::{ByTaskId, NoArgs};
use crate::api::BoardRequest;
use crate::db::Task;
use crate::review::{self, Dependent, Digest, FindingStatus, ReviewFinding, ReviewOutcome};
use crate::review_loop::config::{
    self as review_config, ReviewLevel, ReviewLevelName, ReviewSettings, TaskReview,
};
use crate::review_loop::{self, ReviewConfig, ReviewHistory};
use crate::Result;

/// `in_review` to the bottom of `done`. Given this machine, the auto-removal
/// on `done` runs after the commit, in the same call (task 046 point 3a).
pub async fn approve_task(request: BoardRequest, args: ByTaskId) -> Result<Task> {
    review::approve(&request.ctx, request.machine.as_ref(), &args.task_id).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WithNote {
    pub task_id: String,
    pub note: String,
}

/// Back to `ready` for a fresh start: the branch is set aside in git, never
/// deleted. The note is required. Given this machine, the worktree is removed
/// first, and a dirty one refuses the whole verdict (task 046 point 3a).
pub async fn reject_task(request: BoardRequest, args: WithNote) -> Result<ReviewOutcome> {
    review::reject(
        &request.ctx,
        request.machine.as_ref(),
        &args.task_id,
        &args.note,
    )
    .await
}

/// Back to `ready` to continue on the reviewed commits. The note is required.
pub async fn request_task_changes(request: BoardRequest, args: WithNote) -> Result<ReviewOutcome> {
    review::request_changes(&request.ctx, &args.task_id, &args.note).await
}

/// The tasks that depend directly on this one, and which already built on it.
pub async fn get_task_dependents(request: BoardRequest, args: ByTaskId) -> Result<Vec<Dependent>> {
    review::dependents(&request.ctx, &args.task_id).await
}

/// What the queue did since the last finished review. Given this machine, the
/// runner's consent is read off its checkouts (task 066); without one, the
/// entries skipped for this machine's unattended-run consent are left out.
pub async fn get_review_digest(request: BoardRequest, _: NoArgs) -> Result<Digest> {
    review::digest(&request.ctx, request.machine.as_ref()).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkReviewDigestSeen {
    pub through: DateTime<Utc>,
}

/// Marks the digest seen through `through`, normally the `until` it was read at.
pub async fn mark_review_digest_seen(
    request: BoardRequest,
    args: MarkReviewDigestSeen,
) -> Result<DateTime<Utc>> {
    review::mark_seen(&request.ctx, args.through).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListReviewFindings {
    pub task_id: String,
    pub status: Option<FindingStatus>,
}

/// What review runs found on `task_id`, in review order and then in the order
/// each reviewer gave them, optionally of one status. The read only: no UI
/// writes a finding (seam-contract D30 point 5), so the two write tools have
/// no command.
pub async fn list_review_findings(
    request: BoardRequest,
    args: ListReviewFindings,
) -> Result<Vec<ReviewFinding>> {
    review::findings::list(&request.ctx, &args.task_id, args.status).await
}

/// Every loop the task has had, with each review's findings, what the fix after
/// it resolved, and the loop's verdict and open counts. Grouping, what blocks
/// and the ping-pong lists are core's: the view renders this as it is returned
/// (task 037).
pub async fn get_review_history(request: BoardRequest, args: ByTaskId) -> Result<ReviewHistory> {
    review_loop::history(&request.ctx, &args.task_id).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetReviewLevel {
    pub level: ReviewLevelName,
    pub id: Option<String>,
}

/// One level of the loop's configuration next to what it inherits and what it
/// resolves to, so the interface can say `Inherit (<value>)` without working
/// the precedence out itself (task 037). `id` is the repository's or the
/// task's, and absent for `global`.
pub async fn get_review_level(request: BoardRequest, args: GetReviewLevel) -> Result<ReviewLevel> {
    review_config::get_review_level(&request.ctx, args.level, args.id.as_deref()).await
}

/// The global review instructions and loop configuration.
pub async fn get_review_settings(request: BoardRequest, _: NoArgs) -> Result<ReviewSettings> {
    review_config::get_review_settings(&request.ctx).await
}

/// `config` arrives as raw JSON and goes to the service as it is, so
/// `"enabled": true` is refused in core's sentence rather than in the
/// deserializer, the same refusal the MCP tool gives.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetReviewSettings {
    pub instructions: String,
    pub config: Value,
}

/// Replaces the global review instructions and configuration, validating the
/// models against the host provider's catalogue.
pub async fn set_review_settings(
    request: BoardRequest,
    args: SetReviewSettings,
) -> Result<ReviewSettings> {
    review_config::set_review_settings(
        &request.ctx,
        &request.host.provider,
        &args.instructions,
        args.config,
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetRepositoryReviewConfig {
    pub repository_id: String,
    pub config: Value,
}

/// Replaces one repository's loop configuration.
pub async fn set_repository_review_config(
    request: BoardRequest,
    args: SetRepositoryReviewConfig,
) -> Result<ReviewConfig> {
    review_config::set_repository_review_config(
        &request.ctx,
        &request.host.provider,
        &args.repository_id,
        args.config,
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTaskReview {
    pub task_id: String,
    pub review_instructions: Option<String>,
    pub config: Value,
}

/// Replaces one task's override of the review instructions and its loop
/// configuration.
pub async fn set_task_review(request: BoardRequest, args: SetTaskReview) -> Result<TaskReview> {
    review_config::set_task_review(
        &request.ctx,
        &request.host.provider,
        &args.task_id,
        args.review_instructions,
        args.config,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_loop::ReviewConfig;
    use crate::strategy::Catalogue;
    use crate::ErrorCode;

    /// The setters take `config` as raw JSON, so the argument struct accepts
    /// `"enabled": true` and core refuses it, in the sentence the MCP tool
    /// gives. A typed `ReviewConfig` field would have the deserializer refuse
    /// it first, naming serde's complaint instead of the rule.
    #[test]
    fn a_command_hands_true_to_the_service_which_refuses_it() {
        let args: SetReviewSettings = serde_json::from_value(serde_json::json!({
            "instructions": "",
            "config": { "enabled": true },
        }))
        .expect("the argument struct takes any config");

        let refused = ReviewConfig::from_door(args.config, &Catalogue::default())
            .expect_err("only the acknowledgement turns the loop on");

        assert_eq!(refused.code(), ErrorCode::Invalid);
        assert!(refused.to_string().contains("on_cost_acknowledged"));
    }
}
