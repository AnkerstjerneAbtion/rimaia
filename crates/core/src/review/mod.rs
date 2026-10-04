//! The review half of the board: the three decisions a morning review ends in,
//! what depends on the task being decided, the overnight digest (ADR-0006,
//! ADR-0007, ADR-0008, ADR-0021; task 034), and what a review run found
//! (ADR-0017; task 035).
//!
//! Every rule is written once, here. The Tauri commands, the MCP tools and, in
//! team mode, the HTTP handlers are thin adapters over these functions, so the
//! window, the server and the web client cannot disagree about a refusal, a
//! note or the order of a digest.

pub mod actions;
pub mod digest;
pub mod findings;
pub mod note;

use serde::Serialize;
use sqlx::SqliteConnection;

use crate::context::ServiceContext;
use crate::db::{BoardColumn, RunState, Task};
use crate::error::Result;
use crate::tasks::dependencies::dependents_in;
use crate::tasks::service::fetch_task_row;
use chrono::{DateTime, Utc};

pub use actions::{approve, reject, request_changes, ReviewOutcome};
pub use digest::{digest, mark_seen, Digest, DigestEntry, DigestLoop, DigestOutcome, DigestTotals};
pub use findings::{
    FindingResolution, FindingSeverity, FindingStatus, NewReviewFinding, ReviewFinding,
};
pub use note::Verdict;

/// One task that depends directly on the task under review.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dependent {
    pub id: String,
    pub title: String,
    pub column: BoardColumn,
    pub run_state: RunState,
    pub archived_at: Option<DateTime<Utc>>,
    /// At least one of the dependent's runs started from this task's work. A
    /// dependent that has not run yet, or that chained from a different
    /// dependency, is `false`: rejecting or sending back this task costs it
    /// nothing but a wait.
    pub built_on: bool,
}

/// Every direct dependent of `task_id`, archived ones included, in ADR-0008's
/// order, each marked with whether it already built on this task's work.
pub async fn dependents(ctx: &ServiceContext, task_id: &str) -> Result<Vec<Dependent>> {
    let task = fetch_task_row(&ctx.pool, task_id).await?;
    let mut conn = ctx.pool.acquire().await?;
    dependents_of_task(&mut conn, &task).await
}

/// [`dependents`] over a connection the caller holds, so a review action reads
/// them in the transaction that still has `task.branch` naming the reviewed
/// branch.
pub(crate) async fn dependents_of_task(
    conn: &mut SqliteConnection,
    task: &Task,
) -> Result<Vec<Dependent>> {
    let rows = dependents_in(&mut *conn, &task.id).await?;

    let mut dependents = Vec::with_capacity(rows.len());
    for row in rows {
        // Two clauses, because the evidence changed shape in task 033. A run
        // from before it has no `base_sha`, but it did record the branch it
        // started from. Since it, `base_sha` matching a `head_sha` of this
        // task's own runs survives a reject renaming the branch, and survives
        // task 044 basing dependents on `head_sha`.
        //
        // `head_sha <> base_sha` keeps only runs whose branch carried commits:
        // 033 records `head_sha` even for a run that committed nothing, and
        // then it equals its fork point, a default-branch commit that an
        // unrelated dependent may also have forked from.
        let built_on = sqlx::query_scalar!(
            r#"SELECT EXISTS(
                 SELECT 1 FROM runs r
                  WHERE r.task_id = ?1
                    AND (r.base_ref = ?2
                         OR r.base_sha IN (SELECT head_sha FROM runs
                                            WHERE task_id = ?3
                                              AND head_sha IS NOT NULL
                                              AND base_sha IS NOT NULL
                                              AND head_sha <> base_sha))
               ) AS "built_on!: bool""#,
            row.id,
            task.branch,
            task.id,
        )
        .fetch_one(&mut *conn)
        .await?;

        dependents.push(Dependent {
            id: row.id,
            title: row.title,
            column: row.column,
            run_state: row.run_state,
            archived_at: row.archived_at,
            built_on,
        });
    }
    Ok(dependents)
}
