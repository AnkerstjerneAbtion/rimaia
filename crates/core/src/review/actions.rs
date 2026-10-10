//! Approve, reject and request changes (task 034).
//!
//! Three verdicts, one pipeline: read the task once and refuse everything that
//! can be refused **before any git runs**, do the local half (reject only), then
//! write everything the board needs in one transaction, re-checking the same
//! refusals inside it because the task can change between the two reads.

use std::path::Path;

use serde::Serialize;
use sqlx::SqliteConnection;

use super::digest;
use super::note::{self, Verdict};
use super::{dependents_of_task, Dependent};
use crate::context::ServiceContext;
use crate::db::{BoardColumn, RunState, Task};
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::machine::MachineContext;
use crate::tasks::service::{
    ensure_ready_has_a_plan, fetch_task_row, move_within, task_row, team_of_task, write_plan,
    Destination,
};
use crate::worktree::{self, cleanup, ForceRemoval};

/// What a reject or a request for changes did, as the reviewer is told.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewOutcome {
    pub task: Task,
    /// Every direct dependent, read before the writes while `tasks.branch` still
    /// named the reviewed branch. Both verdicts take the task out of
    /// `in_review`, which blocks each of them (ADR-0008).
    pub dependents: Vec<Dependent>,
    /// Where the rejected work is: the branch the task had, still in git.
    /// `None` for a request for changes, which keeps the branch in use.
    pub set_aside_branch: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Approve,
    RequestChanges,
    Reject,
}

impl Action {
    fn destination(self) -> BoardColumn {
        match self {
            Action::Approve => BoardColumn::Done,
            Action::RequestChanges | Action::Reject => BoardColumn::Ready,
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Action::Approve => "approved",
            Action::RequestChanges => "sent back for changes",
            Action::Reject => "rejected",
        }
    }
}

/// `in_review` to the bottom of `done`.
///
/// Given a machine, the auto-removal policy then runs on it, as it does after
/// a drag into `done`; with `None`, a server's call, the approval is the board
/// write alone (task 041, the shape `tasks::move_task` takes).
#[tracing::instrument(
    skip_all,
    fields(
        source = ctx.source.as_str(),
        user_id = ctx.actor.as_str(),
        task_id = %id,
    )
)]
pub async fn approve(
    ctx: &ServiceContext,
    machine: Option<&MachineContext>,
    id: &str,
) -> Result<Task> {
    decide(ctx, None, id, Action::Approve, None).await?;

    // The auto-removal policy is a rule of the transition into `done`, and
    // approving is that transition (D20 point 3), so it runs here exactly as it
    // does after a drag — after the commit, and unable to fail the approval.
    if let Some(machine) = machine {
        cleanup::auto_remove_on_done(ctx, machine, id).await;
    }
    task_row(ctx, id).await
}

/// `in_review` to the bottom of `ready`, the note appended, and the worktree,
/// the branch and `tasks.branch` untouched: the next run continues on the
/// reviewed commits. It starts a fresh session, because a queue start from
/// `idle` always does; this is not a retry.
#[tracing::instrument(
    skip_all,
    fields(
        source = ctx.source.as_str(),
        user_id = ctx.actor.as_str(),
        task_id = %id,
    )
)]
pub async fn request_changes(ctx: &ServiceContext, id: &str, note: &str) -> Result<ReviewOutcome> {
    decide(ctx, None, id, Action::RequestChanges, Some(note)).await
}

/// `in_review` to the bottom of `ready`, the note appended, and the work set
/// aside, never deleted: the worktree directory goes, the branch stays in git,
/// and `tasks.branch` is cleared so the next run branches fresh from the base.
///
/// Deleting the branch is the one irreversible act here, which is why it is not
/// done (D20 point 6). Reusing its name would have the next run push onto a
/// remote branch holding the rejected commits and fail as a non-fast-forward in
/// the middle of an unattended night.
///
/// The worktree's removal is this machine's half, and comes before the
/// transaction because it can refuse a dirty worktree. It runs only given a
/// machine (task 041); with `None`, a server's call, it is skipped, and the
/// board write stands alone.
#[tracing::instrument(
    skip_all,
    fields(
        source = ctx.source.as_str(),
        user_id = ctx.actor.as_str(),
        task_id = %id,
    )
)]
pub async fn reject(
    ctx: &ServiceContext,
    machine: Option<&MachineContext>,
    id: &str,
    note: &str,
) -> Result<ReviewOutcome> {
    decide(ctx, machine, id, Action::Reject, Some(note)).await
}

/// `machine` is read only by a reject, whose worktree removal is this
/// machine's half.
async fn decide(
    ctx: &ServiceContext,
    machine: Option<&MachineContext>,
    id: &str,
    action: Action,
    note: Option<&str>,
) -> Result<ReviewOutcome> {
    let task = task_row(ctx, id).await?;
    ensure_decidable(&task, action, note)?;

    if let (Action::Reject, Some(machine)) = (action, machine) {
        set_aside_worktree(ctx, machine, &task).await?;
    }

    // Point 6's mark for the note's plan revision, read over the pool before
    // the write lock (ADR-0032 point 3). Approve writes no note.
    let during_run = match note {
        Some(_) => crate::consent::written_during_run(ctx, &ctx.actor).await?,
        None => false,
    };

    // `BEGIN IMMEDIATE` for `move_into_column`'s reason: the reads below decide
    // what the writes do.
    let mut tx = ctx.begin_immediate().await?;
    let task = fetch_task_row(&mut tx, id).await?;
    ensure_decidable(&task, action, note)?;
    // The verdict names an entity, so its team is the task's own, never one
    // asked of the scope (ADR-0035 point 2).
    let team_id = team_of_task(&mut tx, id).await?;

    let dependents = match action {
        Action::Approve => Vec::new(),
        Action::RequestChanges | Action::Reject => dependents_of_task(&mut tx, &task).await?,
    };

    if let Some(text) = note {
        let verdict = match action {
            Action::Reject => Verdict::Rejected,
            _ => Verdict::ChangesRequested,
        };
        let combined = note::append(task.extra_instructions.as_deref(), verdict, text);
        write_extra_instructions(&mut tx, &task, &combined, &ctx.actor, during_run).await?;
    }
    if action == Action::Reject {
        sqlx::query!("UPDATE tasks SET branch = NULL WHERE id = ?1", id)
            .execute(&mut *tx)
            .await?;
    }

    let rebalanced =
        move_within(ctx, &mut tx, id, action.destination(), Destination::Bottom).await?;

    // Only a verdict that leaves nothing awaiting review means "the review is
    // finished". A drag or an archive does the same to the column and does not
    // count: the loop and an MCP client make those moves at 3 a.m.
    //
    // The queue counted is the acted-on task's team's (task 034's "caller's
    // team's queue"): another team's `in_review` is neither counted nor
    // revealed, whatever teams the context reaches.
    let in_review = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!: i64" FROM tasks
            WHERE team_id = ?1 AND board_column = 'in_review' AND archived_at IS NULL"#,
        team_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    let advanced_marker = in_review == 0;
    // The marker is the context's actor's, written on this transaction so the
    // move and the marker commit together or not at all.
    if advanced_marker {
        digest::advance_marker(ctx, &mut tx, ctx.clock.now()).await?;
    }

    tx.commit().await?;

    ctx.publish(ChangeEvent::tasks(
        team_id.clone(),
        std::iter::once(id.to_string()).chain(rebalanced.into_iter().filter(|rid| rid != id)),
    ));
    // The marker is a user setting, and a change event needs a team until task
    // 048's `Audience::User`: it names the acted-on task's team, the one its
    // `Tasks` event names, so a verdict needs no sole team.
    if advanced_marker {
        ctx.publish(ChangeEvent::settings(team_id));
    }

    let set_aside_branch = (action == Action::Reject)
        .then(|| task.branch.clone())
        .flatten();
    Ok(ReviewOutcome {
        task: task_row(ctx, id).await?,
        dependents,
        set_aside_branch,
    })
}

/// Every refusal in task 034's table that needs no disk, from one read of the
/// task. Identical for every door because there is only this function.
fn ensure_decidable(task: &Task, action: Action, note: Option<&str>) -> Result<()> {
    let title = &task.title;
    let id = &task.id;

    if task.column != BoardColumn::InReview {
        return Err(Error::invalid(format!(
            "\"{title}\" is not in review, so it cannot be {verb}. Only a task waiting in the \
             review column can. (task {id})",
            verb = action.verb(),
        )));
    }
    if task.archived_at.is_some() {
        return Err(Error::invalid(format!(
            "\"{title}\" is archived, so it cannot be {verb}. Unarchive it first. (task {id})",
            verb = action.verb(),
        )));
    }
    match task.run_state {
        // No override: a process is, or is about to be, writing in that
        // directory (D20 point 1).
        RunState::Queued | RunState::Running | RunState::WaitingRetry => {
            return Err(Error::invalid(format!(
                "\"{title}\" is {state}, so it cannot be {verb}. Wait for the run to finish, or \
                 cancel it first; there is no way to force this one. (task {id})",
                state = match task.run_state {
                    RunState::Queued => "queued to run",
                    RunState::Running => "running",
                    _ => "waiting to retry",
                },
                verb = action.verb(),
            )));
        }
        RunState::Failed | RunState::Cancelled if action != Action::Approve => {
            return Err(Error::invalid(format!(
                "\"{title}\" ended its last run {outcome}, and the queue skips a task in that \
                 state, so sending it back to ready would leave it sitting there. Use Retry on \
                 it instead. (task {id})",
                outcome = if task.run_state == RunState::Failed {
                    "failed"
                } else {
                    "cancelled"
                },
            )));
        }
        _ => {}
    }
    if action != Action::Approve {
        ensure_ready_has_a_plan(BoardColumn::Ready, &task.plan, title)?;
        if note.is_none_or(|text| text.trim().is_empty()) {
            return Err(Error::invalid(format!(
                "a note is required to {what} \"{title}\": it is the only thing that tells the \
                 next run what the review found. (task {id})",
                what = if action == Action::Reject {
                    "reject"
                } else {
                    "send back"
                },
            )));
        }
    }
    Ok(())
}

/// Reject's local half, and the only part of it that touches this machine's
/// disk: refuse a worktree holding uncommitted work, then remove the directory
/// and forget this machine's record of it. The branch stays in git; the note,
/// `branch = NULL`, the move and the marker are the board's half.
///
/// One function, so that team mode can move exactly this to the runner that
/// holds the worktree and leave the rest of `reject` on the board (ADR-0033
/// point 7).
async fn set_aside_worktree(
    ctx: &ServiceContext,
    machine: &MachineContext,
    task: &Task,
) -> Result<()> {
    let Some(recorded) = crate::machine::local::worktree_path(machine, &task.id).await? else {
        return Ok(());
    };

    let changes = cleanup::uncommitted_change_count(Path::new(&recorded)).await?;
    if changes > 0 {
        return Err(Error::invalid(cleanup::uncommitted_changes_sentence(
            &task.title,
            changes,
            "commit the work, or remove the worktree under Settings → Storage, before rejecting.",
        )));
    }

    // Forgets the record once the directory is gone.
    worktree::remove(ctx, machine, &task.id, false, ForceRemoval::No).await
}

/// The one place a review writes `extra_instructions`, through the plan
/// helper: the note is a plan revision by the reviewer (ADR-0032 point 3),
/// inside the verdict's one transaction, so a refused verdict bumps nothing.
async fn write_extra_instructions(
    tx: &mut SqliteConnection,
    task: &Task,
    extra_instructions: &str,
    reviewer: &str,
    written_during_run: bool,
) -> Result<()> {
    write_plan(
        tx,
        &task.id,
        task.plan.as_deref(),
        Some(extra_instructions),
        reviewer,
        written_during_run,
    )
    .await?;
    Ok(())
}
