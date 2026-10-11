//! The board handlers: one `async fn(BoardRequest, Args) -> Result<O>` per
//! board row of the registry (seam-contract D32 point 1), one file per shell
//! module each came from.
//!
//! Deliberately thin: deserialize, make one call into the service the command
//! always called, return. No branching on business rules, because the MCP
//! server (ADR-0006) calls those same services without passing through here —
//! a rule enforced in only one of the two paths is a bug.
//!
//! **A handler reads nothing but its [`BoardRequest`](super::BoardRequest).**
//! The provider profile and the paths come off `request.host`, and seven
//! handlers pass `request.machine` on to the core function that reacts on this
//! machine when there is one (task 046 point 3a). None reaches the runner's
//! config, the in-flight registry, the tails or the runner store: a handler
//! that needs one of those is a local command.
//!
//! Every top-level argument struct is `#[serde(rename_all = "camelCase")]` and
//! ignores unknown keys, which is exactly how Tauri derived a command's
//! parameters, so the frontend's invoke payloads deserialize unchanged.

use serde::Deserialize;

pub mod account;
pub mod analytics;
pub mod consent;
pub mod repositories;
pub mod review;
pub mod runs;
pub mod settings;
pub mod strategy;
pub mod tasks;

/// A command that takes no arguments. Any keys sent are ignored, as Tauri
/// ignored them.
#[derive(Debug, Deserialize)]
pub struct NoArgs {}

/// `{ id }`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ById {
    pub id: String,
}

/// `{ taskId }`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByTaskId {
    pub task_id: String,
}

/// `{ linkId }`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByLinkId {
    pub link_id: String,
}

/// `{ runId }`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByRunId {
    pub run_id: String,
}
