//! The run reads every client sees, and giving up on a retry loop (task 015,
//! ADR-0013; seam-contract D29).
//!
//! Starting, cancelling, retrying, tailing and the transcript reads spawn,
//! signal or read on this machine, and stay local commands in the shell until
//! the tasks D32's appendix names flip them.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{ByRunId, ByTaskId};
use crate::api::BoardRequest;
use crate::db::{Run, RunKind, RunStatus};
use crate::runs::{self, RunDetail, RunFilter, RunListEntry};
use crate::scheduler;
use crate::Result;

/// Ends a task's retry loop: `waiting_retry -> failed`.
///
/// Thin over [`scheduler::give_up`], which is where the rule lives — the MCP
/// tool of the same name calls the identical function, which is what ADR-0006
/// asks for and ADR-0021 makes a rule.
pub async fn give_up_on_task(request: BoardRequest, args: ByTaskId) -> Result<()> {
    scheduler::give_up(&request.ctx, &args.task_id).await
}

/// Every run of `task_id`, newest attempt first — the task detail panel's
/// history list.
pub async fn list_runs_for_task(request: BoardRequest, args: ByTaskId) -> Result<Vec<Run>> {
    runs::list_runs_for_task(&request.ctx, &args.task_id).await
}

/// What the frontend sends [`list_runs`]. Mirrors [`RunFilter`] — a field left
/// out matches everything, the same contract
/// [`TaskFilterInput`](super::tasks::TaskFilterInput) states for the board.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunFilterInput {
    #[serde(default)]
    pub repository_id: Option<String>,
    #[serde(default)]
    pub status: Option<RunStatus>,
    #[serde(default)]
    pub kind: Option<RunKind>,
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    #[serde(default)]
    pub until: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListRuns {
    pub filter: RunFilterInput,
}

/// The global Runs view's history: every run matching `filter`, newest
/// first, with its task's title and repository name for a list that spans
/// every repository. `logAvailable` is read under the host's paths.
pub async fn list_runs(request: BoardRequest, args: ListRuns) -> Result<Vec<RunListEntry>> {
    let filter = args.filter;
    runs::list_runs(
        &request.ctx,
        &request.host.paths,
        RunFilter {
            repository_id: filter.repository_id,
            status: filter.status,
            kind: filter.kind,
            since: filter.since,
            until: filter.until,
        },
    )
    .await
}

/// One run's full detail: its own outcome, the review its finish recorded
/// (ADR-0013's ordering), and whether its transcript file still resolves
/// under the host's paths. It runs no git (seam-contract D32's appendix).
pub async fn get_run(request: BoardRequest, args: ByRunId) -> Result<RunDetail> {
    runs::get_run(&request.ctx, &request.host.paths, &args.run_id).await
}
