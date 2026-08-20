//! Task CRUD, ordering and the detail read (ADR-0007, seam-contract D1, D2,
//! D9).
//!
//! `set_run_state` lives in [`crate::tasks::run_state`] and link operations
//! in [`crate::tasks::links`] — both large enough, and specific enough in
//! their own invariants, to earn their own file. Everything else a caller
//! does to a task's row is here.
//!
//! Every function takes [`ServiceContext`] rather than a bare `&SqlitePool`
//! (ADR-0018): the pool, the clock and the change sender travel together, so
//! `updated_at` is never `Utc::now()` and a committed mutation is never
//! reported to nobody by accident. Nothing here is a shell type — the MCP
//! server (task 010) builds the same context and gets the same rules
//! (ADR-0006).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{SqliteConnection, SqlitePool};

use crate::context::ServiceContext;
use crate::db::{
    new_id, BoardColumn, ExitClass, Run, RunState, RunStatus, StrategyMode, StrategySource, Task,
    TaskLink,
};
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::tasks::position::{position_between, rebalance_column, rebalanced_positions, Placement};
use crate::tasks::types::{NewTask, TaskFilter, TaskPatch};

/// A task with everything a detail view needs in one read: its links in
/// board order, the ids of what it depends on, and a summary of its most
/// recent attempt.
///
/// `#[serde(flatten)]` on `task` so the wire shape is the task's own fields
/// plus these three, not a nested object the frontend has to reach into.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDetail {
    #[serde(flatten)]
    pub task: Task,
    pub links: Vec<TaskLink>,
    /// Outgoing edges only — what this task depends on, not what depends on
    /// it. Cycle detection and the blocked-task view are task 011's
    /// business; this is storage plus the shape [`get_task`] promises.
    pub depends_on: Vec<String>,
    pub last_run: Option<Run>,
}

/// Creates a task at the bottom of its target column, inside one repository.
///
/// `title` must be non-blank (ADR-0006: a business rule, not a `CHECK` —
/// the schema constrains only `NOT NULL`). A task created directly into
/// [`BoardColumn::Ready`] is held to the same empty-plan guard
/// [`move_task`] enforces, so the invariant holds regardless of which door a
/// task enters `ready` through.
pub async fn create_task(ctx: &ServiceContext, input: NewTask) -> Result<Task> {
    validate_title(&input.title)?;
    let column = input.column.unwrap_or(BoardColumn::NotReady);
    let plan = normalize_plan(input.plan);
    ensure_ready_has_a_plan(column, &plan, &input.title)?;

    let mut tx = ctx.pool.begin().await?;

    let (position, rebalanced_ids) =
        append_task_position(&mut tx, &input.repository_id, column).await?;
    let id = new_id();
    let now = ctx.clock.now();

    sqlx::query!(
        r#"INSERT INTO tasks
            (id, repository_id, title, plan, extra_instructions, board_column, position,
             run_state, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)"#,
        id,
        input.repository_id,
        input.title,
        plan,
        input.extra_instructions,
        column,
        position,
        RunState::Idle,
        now,
    )
    .execute(&mut *tx)
    .await?;

    for (link, link_position) in input
        .links
        .iter()
        .zip(rebalanced_positions(input.links.len()))
    {
        let link_id = new_id();
        sqlx::query!(
            "INSERT INTO task_links (id, task_id, label, url, position) VALUES (?1, ?2, ?3, ?4, ?5)",
            link_id,
            id,
            link.label,
            link.url,
            link_position,
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    // Publish before the read-back: the row is already committed, so a
    // failure in `fetch_task_row` below must not cost the notification for a
    // mutation that already happened (ADR-0018). A rebalance touches every
    // row it renumbered, not just this one, so those ids ride along too — see
    // `rebalance_column`'s own doc comment for why a subscriber needs them.
    ctx.publish(ChangeEvent::tasks(
        std::iter::once(id.clone()).chain(rebalanced_ids.into_iter().filter(|rid| rid != &id)),
    ));
    let created = fetch_task_row(&ctx.pool, &id).await?;
    Ok(created)
}

/// The full detail read: the task itself, its links in order, the ids of
/// what it depends on, and its most recent run (by attempt number).
pub async fn get_task(ctx: &ServiceContext, id: &str) -> Result<TaskDetail> {
    let task = fetch_task_row(&ctx.pool, id).await?;

    let links = sqlx::query_as!(
        TaskLink,
        "SELECT id, task_id, label, url, position FROM task_links
         WHERE task_id = ?1 ORDER BY position ASC, id ASC",
        id,
    )
    .fetch_all(&ctx.pool)
    .await?;

    let depends_on = sqlx::query_scalar!(
        "SELECT depends_on_task_id FROM task_dependencies
         WHERE task_id = ?1 ORDER BY depends_on_task_id ASC",
        id,
    )
    .fetch_all(&ctx.pool)
    .await?;

    let last_run = fetch_last_run(&ctx.pool, id).await?;

    Ok(TaskDetail {
        task,
        links,
        depends_on,
        last_run,
    })
}

/// Every task matching `filter`, ordered the way the board reads a column:
/// by repository, then column, then position — the same leading columns as
/// the migration's own `idx_tasks_board`, so a broad call (few or no
/// filters) is still a board-shaped read rather than an arbitrary one.
///
/// Optional filters are why this is hand-built SQL through the `FromRow`
/// path rather than `query_as!`: the macro needs a query whose shape is
/// fixed at compile time, and which `WHERE` clauses apply here depends on
/// which fields of `filter` are `Some` (seam-contract D5).
pub async fn list_tasks(ctx: &ServiceContext, filter: TaskFilter) -> Result<Vec<Task>> {
    let mut sql = String::from("SELECT * FROM tasks WHERE 1 = 1");
    if filter.repository_id.is_some() {
        sql.push_str(" AND repository_id = ?");
    }
    if filter.column.is_some() {
        sql.push_str(" AND board_column = ?");
    }
    if filter.run_state.is_some() {
        sql.push_str(" AND run_state = ?");
    }
    sql.push_str(
        " ORDER BY repository_id ASC, board_column ASC, position ASC, created_at ASC, id ASC",
    );

    let mut query = sqlx::query_as::<_, Task>(&sql);
    if let Some(repository_id) = filter.repository_id {
        query = query.bind(repository_id);
    }
    if let Some(column) = filter.column {
        query = query.bind(column);
    }
    if let Some(run_state) = filter.run_state {
        query = query.bind(run_state);
    }

    Ok(query.fetch_all(&ctx.pool).await?)
}

/// Applies `patch`'s changed fields to a task and stamps `updated_at`. Fields
/// [`patch`](TaskPatch) leaves `Unset` keep their current value exactly.
///
/// If the task is currently in [`BoardColumn::Ready`], clearing its plan is
/// refused — the empty-plan guard is about the *state* "ready with no plan",
/// not only about the `move_task` call that would create it, so editing the
/// plan out from under a ready card is held to the same rule.
pub async fn update_task(ctx: &ServiceContext, id: &str, patch: TaskPatch) -> Result<Task> {
    let mut tx = ctx.pool.begin().await?;
    let current = fetch_task_row(&mut *tx, id).await?;

    let title = match patch.title {
        Some(title) => {
            validate_title(&title)?;
            title
        }
        None => current.title,
    };
    let plan = normalize_plan(patch.plan.apply(current.plan));
    let extra_instructions = patch.extra_instructions.apply(current.extra_instructions);
    let model = patch.model.apply(current.model);
    let effort = patch.effort.apply(current.effort);

    ensure_ready_has_a_plan(current.column, &plan, &title)?;

    let now = ctx.clock.now();
    sqlx::query!(
        r#"UPDATE tasks SET title = ?1, plan = ?2, extra_instructions = ?3, model = ?4,
            effort = ?5, updated_at = ?6 WHERE id = ?7"#,
        title,
        plan,
        extra_instructions,
        model,
        effort,
        now,
        id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // See `create_task`'s identical comment: publish before the read-back so
    // a failed re-read never costs the notification for a committed write.
    ctx.publish(ChangeEvent::tasks([id.to_string()]));
    let updated = fetch_task_row(&ctx.pool, id).await?;
    Ok(updated)
}

/// Deletes a task, refusing when another task depends on it (ADR-0008:
/// "deleting a task with dependents is refused; the edges must be removed
/// first"). The schema's `RESTRICT` on `depends_on_task_id` is the backstop;
/// this is the message the user actually reads, naming what still depends on
/// it.
///
/// A task nothing depends on is removed along with its links, its own
/// outgoing dependency edges and its runs — all by the schema's `CASCADE`,
/// exercised here through the service rather than assumed from the schema
/// test suite in `tests/store.rs`.
pub async fn delete_task(ctx: &ServiceContext, id: &str) -> Result<()> {
    let mut tx = ctx.pool.begin().await?;
    fetch_task_row(&mut *tx, id).await?;

    let dependents = sqlx::query_scalar!(
        r#"SELECT t.title AS "title!"
           FROM task_dependencies d JOIN tasks t ON t.id = d.task_id
           WHERE d.depends_on_task_id = ?1
           ORDER BY t.title ASC"#,
        id,
    )
    .fetch_all(&mut *tx)
    .await?;

    if !dependents.is_empty() {
        return Err(Error::invalid(format!(
            "cannot delete this task: {count} other task(s) depend on it: {names}",
            count = dependents.len(),
            names = dependents.join(", "),
        )));
    }

    sqlx::query!("DELETE FROM tasks WHERE id = ?1", id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    ctx.publish(ChangeEvent::tasks([id.to_string()]));
    Ok(())
}

/// Moves a task to `column`, landing it between `before_id` (the card that
/// ends up above it) and `after_id` (the card that ends up below), and
/// rebalances the destination column first if there is no representable
/// position between them — all in one transaction, so a concurrent MCP
/// write and a board drag can never interleave into a corrupted order
/// (seam-contract D1: the arithmetic is [`position_between`]'s, this owns
/// the transaction and the neighbour lookup).
///
/// `before_id` and `after_id` name tasks already in the **destination**
/// column of the **same repository** — a task cannot be dropped next to a
/// card in a different column or a different board. Naming neither is only
/// legal when the destination is otherwise empty; anywhere else it is
/// ambiguous (this crate's own choice — see the return value of the agent
/// that wrote this for why no ADR settles it) rather than a silent
/// "append", because guessing risks landing on top of a card that is
/// already there.
///
/// Refuses to land in [`BoardColumn::Ready`] with no plan. Landing in
/// [`BoardColumn::Done`] is always allowed from anywhere — the user is in
/// charge of their own board.
pub async fn move_task(
    ctx: &ServiceContext,
    id: &str,
    column: BoardColumn,
    before_id: Option<&str>,
    after_id: Option<&str>,
) -> Result<Task> {
    if before_id == Some(id) || after_id == Some(id) {
        return Err(Error::invalid("a task cannot be moved next to itself"));
    }

    let mut tx = ctx.pool.begin().await?;
    let task = fetch_task_row(&mut *tx, id).await?;

    ensure_ready_has_a_plan(column, &task.plan, &task.title)?;

    let (position, rebalanced_ids) = resolve_task_position(
        &mut tx,
        &task.repository_id,
        column,
        id,
        before_id,
        after_id,
    )
    .await?;

    let now = ctx.clock.now();
    sqlx::query!(
        "UPDATE tasks SET board_column = ?1, position = ?2, updated_at = ?3 WHERE id = ?4",
        column,
        position,
        now,
        id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // See `create_task`'s identical comment: publish before the read-back,
    // and name every id a forced rebalance renumbered alongside this one.
    ctx.publish(ChangeEvent::tasks(
        std::iter::once(id.to_string()).chain(rebalanced_ids.into_iter().filter(|rid| rid != id)),
    ));
    let updated = fetch_task_row(&ctx.pool, id).await?;
    Ok(updated)
}

// ---------------------------------------------------------------------------
// Shared validation
// ---------------------------------------------------------------------------

fn validate_title(title: &str) -> Result<()> {
    if title.trim().is_empty() {
        return Err(Error::invalid("a task's title must not be blank"));
    }
    Ok(())
}

/// A plan counts as present when it holds something other than whitespace —
/// `None` and `Some("   ")` are the same "nothing to hand an agent" as far as
/// this guard is concerned, even though only the former is how
/// [`create_task`] and a freshly captured task spell it.
fn plan_is_present(plan: &Option<String>) -> bool {
    plan.as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}

/// Collapses a blank plan to `None` before it reaches the database.
///
/// The migration's own comment on `tasks.plan` is explicit that `NULL` is
/// the only spelling of "no plan": "an empty string would be a second way to
/// spell it." [`create_task`] and [`update_task`] both run their caller's
/// plan through this before binding it, so `Some("")` or `Some("   ")` never
/// reaches the column — a consumer that tests `plan IS NOT NULL` (a prompt
/// composer, a card badge) would otherwise read a whitespace-only plan as a
/// real one.
fn normalize_plan(plan: Option<String>) -> Option<String> {
    match plan {
        Some(value) if value.trim().is_empty() => None,
        other => other,
    }
}

/// The one rule named in task 004's scope: a task cannot be in
/// [`BoardColumn::Ready`] without a plan. Shared by [`create_task`],
/// [`update_task`] and [`move_task`] so the invariant holds no matter which
/// operation would otherwise produce that state.
fn ensure_ready_has_a_plan(column: BoardColumn, plan: &Option<String>, title: &str) -> Result<()> {
    if column == BoardColumn::Ready && !plan_is_present(plan) {
        return Err(Error::invalid(format!(
            "cannot put \"{title}\" in ready without a plan"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Row access shared with `run_state` and `links`
// ---------------------------------------------------------------------------

/// The one place a task row is read back — used both inside a transaction
/// (`&mut *tx`, before a write that depends on the current row) and against
/// the bare pool (after a commit, to hand the caller the row it just
/// produced). Generic over [`sqlx::Executor`] rather than fixed to
/// `&mut SqliteConnection` the way [`rebalance_column`] is: nothing here
/// requires the caller's transaction the way a renumber does, so the
/// flexibility is free.
pub(super) async fn fetch_task_row<'e, E>(executor: E, id: &str) -> Result<Task>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query_as!(
        Task,
        r#"SELECT id, repository_id, title, plan, extra_instructions,
            board_column AS "column: BoardColumn", position, run_state AS "run_state: RunState",
            branch, worktree_path, strategy_mode AS "strategy_mode: StrategyMode", model, effort,
            strategy_plan, strategy_source AS "strategy_source: StrategySource",
            strategy_updated_at AS "strategy_updated_at: DateTime<Utc>",
            created_at AS "created_at: DateTime<Utc>", updated_at AS "updated_at: DateTime<Utc>"
           FROM tasks WHERE id = ?1"#,
        id,
    )
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| Error::not_found(format!("no task with id {id}")))
}

async fn fetch_last_run(pool: &SqlitePool, task_id: &str) -> Result<Option<Run>> {
    let run = sqlx::query_as!(
        Run,
        r#"SELECT id, task_id, attempt, status AS "status: RunStatus", session_id, prompt,
            started_at AS "started_at: DateTime<Utc>", ended_at AS "ended_at: DateTime<Utc>",
            exit_class AS "exit_class: ExitClass", error_message, num_turns, cost_usd, log_path,
            pr_url, resume_after AS "resume_after: DateTime<Utc>"
           FROM runs WHERE task_id = ?1 ORDER BY attempt DESC LIMIT 1"#,
        task_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(run)
}

// ---------------------------------------------------------------------------
// Fractional placement — task 004's obligation as `position.rs`'s own doc
// comment describes it: call `position_between`, and on
// `Placement::NeedsRebalance`, renumber and ask again, in the same
// transaction as the write that follows.
// ---------------------------------------------------------------------------

/// Where [`create_task`] always lands: the bottom of the target column,
/// regardless of what is already there.
///
/// The second element is every id [`rebalance_column`] renumbered along the
/// way — empty on the common path where no rebalance was needed. The caller
/// publishes these alongside the new task's own id.
async fn append_task_position(
    tx: &mut SqliteConnection,
    repository_id: &str,
    column: BoardColumn,
) -> Result<(f64, Vec<String>)> {
    let last = last_task_position(&mut *tx, repository_id, column, None).await?;
    match position_between(last, None) {
        Placement::At(position) => Ok((position, Vec::new())),
        Placement::NeedsRebalance => {
            let rebalanced_ids = rebalance_column(tx, repository_id, column).await?;
            let last = last_task_position(&mut *tx, repository_id, column, None).await?;
            match position_between(last, None) {
                Placement::At(position) => Ok((position, rebalanced_ids)),
                Placement::NeedsRebalance => Err(Error::internal(
                    "a freshly rebalanced column still has no room to append a task",
                )),
            }
        }
    }
}

/// Where [`move_task`] lands a task between named neighbours, rebalancing
/// the destination column and retrying once if there is no room.
///
/// The second element is every id [`rebalance_column`] renumbered along the
/// way — see [`append_task_position`]'s identical note.
async fn resolve_task_position(
    tx: &mut SqliteConnection,
    repository_id: &str,
    column: BoardColumn,
    moving_id: &str,
    before_id: Option<&str>,
    after_id: Option<&str>,
) -> Result<(f64, Vec<String>)> {
    let (before_position, after_position) =
        task_neighbour_positions(&mut *tx, repository_id, column, before_id, after_id).await?;

    if before_position.is_none() && after_position.is_none() {
        // `position_between(None, None)` is only "an empty column" by its own
        // doc comment. Naming no neighbour at all is legal here on exactly
        // that condition; anywhere else it is ambiguous rather than
        // "append", so it is refused instead of guessed.
        if task_column_has_other_rows(&mut *tx, repository_id, column, moving_id).await? {
            return Err(Error::invalid(
                "moving a task with neither a before nor an after neighbour is only valid when the destination column is empty",
            ));
        }
        return Ok((0.0, Vec::new()));
    }

    match position_between(before_position, after_position) {
        Placement::At(position) => Ok((position, Vec::new())),
        Placement::NeedsRebalance => {
            let rebalanced_ids = rebalance_column(tx, repository_id, column).await?;
            let (before_position, after_position) =
                task_neighbour_positions(&mut *tx, repository_id, column, before_id, after_id)
                    .await?;
            match position_between(before_position, after_position) {
                Placement::At(position) => Ok((position, rebalanced_ids)),
                Placement::NeedsRebalance => Err(Error::internal(
                    "a freshly rebalanced column still has no room for this drop",
                )),
            }
        }
    }
}

async fn task_neighbour_positions(
    tx: &mut SqliteConnection,
    repository_id: &str,
    column: BoardColumn,
    before_id: Option<&str>,
    after_id: Option<&str>,
) -> Result<(Option<f64>, Option<f64>)> {
    let before = match before_id {
        Some(before_id) => {
            Some(task_position_in_column(&mut *tx, repository_id, column, before_id).await?)
        }
        None => None,
    };
    let after = match after_id {
        Some(after_id) => {
            Some(task_position_in_column(&mut *tx, repository_id, column, after_id).await?)
        }
        None => None,
    };
    Ok((before, after))
}

async fn task_position_in_column(
    tx: &mut SqliteConnection,
    repository_id: &str,
    column: BoardColumn,
    id: &str,
) -> Result<f64> {
    sqlx::query_scalar!(
        "SELECT position FROM tasks WHERE id = ?1 AND repository_id = ?2 AND board_column = ?3",
        id,
        repository_id,
        column,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        Error::invalid(format!(
            "task {id} is not in the destination column, so it cannot be used as a neighbour"
        ))
    })
}

/// The current bottom of a column, or `excluding`'s position skipped if it
/// names a row already there — so [`append_task_position`] and a "no
/// neighbour named" [`move_task`] call both see the *other* cards, not the
/// one being placed.
async fn last_task_position(
    tx: &mut SqliteConnection,
    repository_id: &str,
    column: BoardColumn,
    excluding: Option<&str>,
) -> Result<Option<f64>> {
    sqlx::query_scalar!(
        r#"SELECT position FROM tasks
           WHERE repository_id = ?1 AND board_column = ?2 AND id IS NOT ?3
           ORDER BY position DESC, created_at DESC, id DESC LIMIT 1"#,
        repository_id,
        column,
        excluding,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(Error::from)
}

async fn task_column_has_other_rows(
    tx: &mut SqliteConnection,
    repository_id: &str,
    column: BoardColumn,
    excluding: &str,
) -> Result<bool> {
    let count: i64 = sqlx::query_scalar!(
        "SELECT count(*) FROM tasks WHERE repository_id = ?1 AND board_column = ?2 AND id != ?3",
        repository_id,
        column,
        excluding,
    )
    .fetch_one(&mut *tx)
    .await?;
    Ok(count > 0)
}
