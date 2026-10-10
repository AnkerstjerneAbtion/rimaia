//! An operator's action on a task that holds no lease: ending its retry loop
//! by hand (ADR-0011).
//!
//! Who owns a task is no longer decided here. The claim, its release and its
//! interleaving argument moved to `board::lease` with task 043, where the
//! edges and the runner lease are written in one transaction: an expected
//! current state in `tasks::run_state::transition`'s `WHERE` closes both
//! windows the two-edge claim used to document, the crash between its two
//! commits and the task selected as `idle` that reached `failed` in between.
//! [`give_up`] stays, because it acts on a waiting task, which no runner
//! holds.

use crate::context::ServiceContext;
use crate::db::RunState;
use crate::error::{Error, Result};
use crate::tasks::{run_state_spelling, set_run_state};

/// Ends a task's retry loop by hand: `waiting_retry -> failed`.
///
/// The operator's half of ADR-0011's cap. The policy gives up on its own when
/// the budget runs out; this is the person who has read the error and knows the
/// next four attempts will hit the same wall, and it is a legal edge in
/// ADR-0007's machine already — "retries exhausted", taken early.
///
/// Lives here rather than in a Tauri command because ADR-0006 makes a rule
/// enforced in one adapter and not the other a defect: the button and the MCP
/// tool call this same function, so the refusal below is one sentence rather
/// than two similar ones.
///
/// Refuses anything that is not waiting, with a sentence rather than a state
/// machine error. "Give up" on a task that is running means cancel, on a task
/// that is idle means nothing at all, and answering either with "illegal
/// transition WaitingRetry -> Failed" would tell the user about our internals
/// instead of about their card.
pub async fn give_up(ctx: &ServiceContext, task_id: &str) -> Result<()> {
    match current_run_state(ctx, task_id).await? {
        Some(RunState::WaitingRetry) => {
            set_run_state(ctx, task_id, RunState::Failed).await?;
            tracing::info!(%task_id, "the operator ended this task's retry loop");
            Ok(())
        }
        Some(other) => Err(Error::invalid(format!(
            "this task is not waiting to be retried (it is {}), so there is nothing to give up on. \
             A run in flight is stopped with Cancel.",
            run_state_spelling(other),
        ))),
        None => Err(Error::not_found(format!("no task with id {task_id}"))),
    }
}

/// A read, not a second writer: the scheduler never issues an `UPDATE tasks`
/// (see this module's parent). `None` for a task that no longer exists, or
/// that the context's scope does not hold: the two are answered alike.
async fn current_run_state(ctx: &ServiceContext, task_id: &str) -> Result<Option<RunState>> {
    let scope = ctx.scope.json();
    let run_state = sqlx::query_scalar!(
        r#"SELECT run_state AS "run_state: RunState" FROM tasks
            WHERE id = ?1 AND team_id IN (SELECT value FROM json_each(?2))"#,
        task_id,
        scope,
    )
    .fetch_optional(&ctx.pool)
    .await?;
    Ok(run_state)
}
