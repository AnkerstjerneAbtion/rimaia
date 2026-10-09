//! Closing a row without landing its task, for a test that arranges history
//! rather than exercising the loop (task 035).
//!
//! `runner::outcome::finish_run` closes every kind since task 021 and lands
//! the task by ADR-0017's exits. A test that needs a review or fix row closed
//! *and* the task left where the test put it — a row in the middle of a
//! history it is building — writes it here, directly. Nothing here is a
//! second way to finish a run.
//!
//! This writes the row only. A test that needs the task waiting then calls
//! `tasks::set_run_state(ctx, task_id, RunState::WaitingRetry)`, which is
//! ADR-0011's legal `running → waiting_retry` edge, so the task side still goes
//! through the one function that owns it.

use chrono::{DateTime, Utc};

use crate::context::ServiceContext;
use crate::db::{ExitClass, RunStatus};

/// Closes `run_id` as ended now (from `ctx.clock`), with `status`,
/// `exit_class` and `resume_after`. Panics on a run that does not exist.
pub async fn close_run(
    ctx: &ServiceContext,
    run_id: &str,
    status: RunStatus,
    exit_class: ExitClass,
    resume_after: Option<DateTime<Utc>>,
) {
    let ended_at = ctx.clock.now();
    let closed = sqlx::query!(
        r#"UPDATE runs SET ended_at = ?1, status = ?2, exit_class = ?3, resume_after = ?4
            WHERE id = ?5"#,
        ended_at,
        status,
        exit_class,
        resume_after,
        run_id,
    )
    .execute(&ctx.pool)
    .await
    .expect("closing a run row");
    assert_eq!(closed.rows_affected(), 1, "no run with id {run_id}");
}
