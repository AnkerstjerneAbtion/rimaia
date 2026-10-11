//! The run reads every client sees, giving up on a retry loop (task 015,
//! ADR-0013; seam-contract D29), and asking a runner to start a run (task
//! 052).
//!
//! A start is a request to the board, which reaches the runner through the
//! host's relay (`board::relay`): the shell's own starter in solo, and the
//! runner's next claim on a server. The transcript reads read on this machine
//! and stay local commands until task 056 flips them. The tail is the host's
//! tail relay's (task 048).

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{ByRunId, ByTaskId};
use crate::api::BoardRequest;
use crate::board::relay::{self, StartRequest};
use crate::db::{Run, RunKind, RunStatus};
use crate::runner::events::RunTail;
use crate::runs::{self, RunDetail, RunFilter, RunListEntry};
use crate::scheduler;
use crate::Result;

/// What the frontend sends [`start_task_run`]: the task, and the runner to
/// start it on. A solo window sends `{ taskId }` alone, as it always has; a
/// browser names the runner (task 069 gives it the picker).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartTask {
    pub task_id: String,
    #[serde(default)]
    pub runner_id: Option<String>,
}

/// Run now: asks one runner to start the task (ADR-0031 point 7). Answers
/// once the start is relayed, never once the run finishes; the run's
/// progress arrives as `tasks:changed`, `runs:changed` and `runs:tail`.
///
/// Who may ask, and which runner it reaches, are `board::relay::request_start`'s:
/// only the runner's owner, and in solo the installation's own runner, whose
/// starter answers every refusal before this returns.
pub async fn start_task_run(request: BoardRequest, args: StartTask) -> Result<()> {
    relay::request_start(
        &request.ctx,
        &request.caller.door,
        request.host.relay.as_ref(),
        StartRequest {
            task_id: args.task_id,
            runner_id: args.runner_id,
            continue_session: false,
        },
    )
    .await
}

/// Retry now: asks one runner to resume a task that is waiting out a retry,
/// now, continuing the session it was in (ADR-0011). A retry is a claim
/// through the task's pin, so through any door but the solo window a request
/// that names no runner goes to the pinned one (D32's appendix).
pub async fn retry_task_now(request: BoardRequest, args: StartTask) -> Result<()> {
    relay::request_start(
        &request.ctx,
        &request.caller.door,
        request.host.relay.as_ref(),
        StartRequest {
            task_id: args.task_id,
            runner_id: args.runner_id,
            continue_session: true,
        },
    )
    .await
}

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

/// The latest live snapshot of `run_id`, for a client that starts watching
/// mid-run and has missed every `runs:tail` so far (seam-contract D14), or
/// `null` when none has been heard.
///
/// The run is read under the caller's scope first, so another team's run id
/// is `not_found` exactly as a never-issued one is; only then does the relay
/// answer.
pub async fn get_run_tail(request: BoardRequest, args: ByRunId) -> Result<Option<RunTail>> {
    runs::get_run_row(&request.ctx, &args.run_id).await?;
    Ok(request
        .host
        .tails
        .latest(&args.run_id)
        .filter(|tail| request.ctx.scope.contains(&tail.team_id))
        .map(|tail| tail.tail))
}
