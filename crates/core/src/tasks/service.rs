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
use sqlx::sqlite::SqliteRow;
use sqlx::{FromRow, Row, SqliteConnection, SqlitePool};

use crate::context::ServiceContext;
use crate::db::{
    new_id, BoardColumn, ExitClass, MutationSource, Run, RunState, RunStatus, StrategyMode,
    StrategySource, Task, TaskLink,
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

/// One card's worth of a task: every column of the row, plus the two counts
/// and the last-run fields the board draws on it (seam-contract D12).
///
/// Deliberately not [`TaskDetail`]. The panel needs the link rows themselves,
/// the dependency ids and the whole [`Run`]; a card needs two numbers and
/// three fields, and the difference is that a fifty-card board read is one
/// query rather than fifty — on every `tasks:changed`, which arrives once per
/// mutation.
///
/// `#[serde(flatten)]` on `task` for the reason [`TaskDetail`] gives: the wire
/// shape is the task's own fields plus these four, not a nested object.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSummary {
    #[serde(flatten)]
    pub task: Task,
    pub link_count: i64,
    pub dependency_count: i64,
    /// Reserved for task 011, and a constant `false` until it lands
    /// (seam-contract D12). It ships now, with the card that renders it and
    /// the TypeScript mirror, so that turning it into a real predicate is a
    /// change to [`list_tasks`]'s query and nothing else — the `0` literal in
    /// [`TASK_SUMMARY_SELECT`] is the single place task 011 edits. Deliberate,
    /// not an unfinished thought.
    pub blocked_by_incomplete: bool,
    pub last_run: Option<LastRunSummary>,
}

/// What a card shows about a task's most recent attempt.
///
/// Three fields of a [`Run`] rather than the row: the word "interrupted" is
/// read off `exit_class` (seam-contract D9), `ended_at` is the card's relative
/// time, and the prompt, the session id and the transcript path are the
/// panel's business.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastRunSummary {
    pub status: RunStatus,
    pub exit_class: Option<ExitClass>,
    /// `None` while the attempt is still in flight — which is also why the
    /// "last" run is chosen by attempt number and never by this column.
    pub ended_at: Option<DateTime<Utc>>,
}

/// Hand-written rather than derived: `last_run` is an `Option<struct>` over
/// three nullable columns, and `#[sqlx(flatten)]` has no way to say "all three
/// NULL means `None`". A task with no runs is the common case on a board, not
/// an edge one.
impl<'r> FromRow<'r, SqliteRow> for TaskSummary {
    fn from_row(row: &'r SqliteRow) -> sqlx::Result<Self> {
        let last_run = match row.try_get::<Option<RunStatus>, _>("last_run_status")? {
            // `status` is `NOT NULL` on the row, so it is the one column that
            // distinguishes "no run" from a run whose other fields are still
            // unset.
            Some(status) => Some(LastRunSummary {
                status,
                exit_class: row.try_get("last_run_exit_class")?,
                ended_at: row.try_get("last_run_ended_at")?,
            }),
            None => None,
        };

        Ok(TaskSummary {
            task: Task::from_row(row)?,
            link_count: row.try_get("link_count")?,
            dependency_count: row.try_get("dependency_count")?,
            blocked_by_incomplete: row.try_get("blocked_by_incomplete")?,
            last_run,
        })
    }
}

/// The board's bulk read (seam-contract D12) up to its `WHERE`, which
/// [`list_tasks`] appends its optional filters to.
///
/// The counts are correlated subqueries rather than two `LEFT JOIN`s under one
/// `GROUP BY`: joining both child tables at once multiplies their rows
/// together, so a task with three links and two dependencies would report six
/// of each unless every count were a `count(DISTINCT …)`. They also keep the
/// no-rows case honest for free — a task with no links, no edges and no runs
/// comes back with zeros and NULLs rather than dropping out of the result, and
/// an inner join here would silently empty a board of brand-new tasks.
///
/// The last run is joined on the highest `attempt`, never on `ended_at`, which
/// is NULL while a run is in flight. `idx_runs_task_attempt` is UNIQUE on
/// `(task_id, attempt)`, so that join matches at most one row and needs no
/// `GROUP BY` of its own.
const TASK_SUMMARY_SELECT: &str = r#"
SELECT t.*,
       (SELECT count(*) FROM task_links WHERE task_id = t.id) AS link_count,
       (SELECT count(*) FROM task_dependencies WHERE task_id = t.id) AS dependency_count,
       0 AS blocked_by_incomplete,
       r.status AS last_run_status,
       r.exit_class AS last_run_exit_class,
       r.ended_at AS last_run_ended_at
  FROM tasks t
  LEFT JOIN runs r ON r.task_id = t.id
       AND r.attempt = (SELECT max(attempt) FROM runs WHERE task_id = t.id)
 WHERE 1 = 1"#;

/// Creates a task at the bottom of its target column, inside one repository.
///
/// `title` must be non-blank (ADR-0006: a business rule, not a `CHECK` —
/// the schema constrains only `NOT NULL`). A task created directly into
/// [`BoardColumn::Ready`] is held to the same empty-plan guard
/// [`move_task`] enforces, so the invariant holds regardless of which door a
/// task enters `ready` through.
#[tracing::instrument(
    skip_all,
    fields(source = ctx.source.as_str(), repository_id = %input.repository_id)
)]
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
             run_state, created_at, updated_at, source)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10)"#,
        id,
        input.repository_id,
        input.title,
        plan,
        input.extra_instructions,
        column,
        position,
        RunState::Idle,
        now,
        ctx.source,
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

/// Every task matching `filter` as a [`TaskSummary`], ordered the way the
/// board reads a column: by repository, then column, then position — the same
/// leading columns as the migration's own `idx_tasks_board`, so a broad call
/// (few or no filters) is still a board-shaped read rather than an arbitrary
/// one.
///
/// A summary and not a bare row because the card has to show a link count, a
/// dependency indicator and the outcome of the last run (seam-contract D12);
/// [`get_task`] is unchanged and still the detail read.
///
/// Optional filters are why this is hand-built SQL through the `FromRow`
/// path rather than `query_as!`: the macro needs a query whose shape is
/// fixed at compile time, and which `WHERE` clauses apply here depends on
/// which fields of `filter` are `Some` (seam-contract D5).
pub async fn list_tasks(ctx: &ServiceContext, filter: TaskFilter) -> Result<Vec<TaskSummary>> {
    let mut sql = String::from(TASK_SUMMARY_SELECT);
    if filter.repository_id.is_some() {
        sql.push_str(" AND t.repository_id = ?");
    }
    if filter.column.is_some() {
        sql.push_str(" AND t.board_column = ?");
    }
    if filter.run_state.is_some() {
        sql.push_str(" AND t.run_state = ?");
    }
    // Qualified with `t.`: `runs` carries an `id` and a `task_id` of its own,
    // so an unqualified ordering column would be ambiguous the moment the join
    // above matched.
    sql.push_str(
        " ORDER BY t.repository_id ASC, t.board_column ASC, t.position ASC, t.created_at ASC, t.id ASC",
    );

    let mut query = sqlx::query_as::<_, TaskSummary>(&sql);
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
///
/// `patch.repository_id` re-files the task, and is refused once anything has
/// tied it to the repository it is in — see
/// [`resolve_repository_placement`] and seam-contract D13.
#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), task_id = %id))]
pub async fn update_task(ctx: &ServiceContext, id: &str, patch: TaskPatch) -> Result<Task> {
    let mut tx = ctx.pool.begin().await?;
    let current = fetch_task_row(&mut *tx, id).await?;

    // Resolved before the patch's own columns are folded in, because it is
    // the one field whose new value depends on rows other than this task's:
    // the destination repository has to exist, nothing may have tied the task
    // to its current one, and the position it lands on is read out of the
    // destination column.
    let placement = resolve_repository_placement(&mut tx, &current, patch.repository_id).await?;

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
        r#"UPDATE tasks SET repository_id = ?1, title = ?2, plan = ?3, extra_instructions = ?4,
            model = ?5, effort = ?6, position = ?7, updated_at = ?8 WHERE id = ?9"#,
        placement.repository_id,
        title,
        plan,
        extra_instructions,
        model,
        effort,
        placement.position,
        now,
        id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // See `create_task`'s identical comment: publish before the read-back so
    // a failed re-read never costs the notification for a committed write.
    // A reassignment that forced a rebalance in the destination column
    // renumbered other cards too, so their ids ride along the way
    // `create_task` and `move_task` send theirs.
    ctx.publish(ChangeEvent::tasks(std::iter::once(id.to_string()).chain(
        placement.rebalanced_ids.into_iter().filter(|rid| rid != id),
    )));
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
#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), task_id = %id))]
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
#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), task_id = %id, column = ?column))]
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
// Repository reassignment (seam-contract D13)
// ---------------------------------------------------------------------------

/// Where a task's row lands once [`update_task`] has resolved
/// `patch.repository_id`: which repository it belongs to, and its position in
/// that repository's copy of its column — plus every id a forced rebalance
/// renumbered on the way, which is empty on every path except a reassignment
/// into a column with no room left.
struct RepositoryPlacement {
    repository_id: String,
    position: f64,
    rebalanced_ids: Vec<String>,
}

/// What `patch.repository_id` means for the row, per seam-contract D13.
///
/// Three cases. A patch that omits the field changes nothing — patch
/// semantics, the same as every other field. A patch naming the repository
/// the task is already in also changes nothing, and in particular does not
/// reorder the column: re-filing a task where it already lives is a no-op,
/// not a request to send it to the bottom. A patch naming a different
/// repository is the reassignment, and is held to
/// [`ensure_repository_is_reassignable`] first.
///
/// The position is recomputed rather than carried, because `position` is
/// scoped to `(repository, column)` (ADR-0007) and a number that ordered the
/// old repository's column means nothing in the destination's — at best it
/// lands somewhere arbitrary, at worst it duplicates a card already there,
/// which is precisely the damage [`rebalance_column`] exists to repair. The
/// bottom of the same column is the destination: the card keeps its place in
/// the user's process and loses only a priority that was never expressed
/// against these neighbours. All of it inside [`update_task`]'s transaction,
/// so no reader ever sees the row filed under a repository it has no position
/// in.
async fn resolve_repository_placement(
    tx: &mut SqliteConnection,
    current: &Task,
    requested: Option<String>,
) -> Result<RepositoryPlacement> {
    let unchanged = || RepositoryPlacement {
        repository_id: current.repository_id.clone(),
        position: current.position,
        rebalanced_ids: Vec::new(),
    };

    let Some(repository_id) = requested else {
        return Ok(unchanged());
    };
    if repository_id == current.repository_id {
        return Ok(unchanged());
    }

    ensure_repository_exists(&mut *tx, &repository_id).await?;
    ensure_repository_is_reassignable(&mut *tx, current).await?;

    let (position, rebalanced_ids) =
        append_task_position(&mut *tx, &repository_id, current.column).await?;

    Ok(RepositoryPlacement {
        repository_id,
        position,
        rebalanced_ids,
    })
}

/// The schema's foreign key already refuses a `repository_id` naming nothing,
/// but as a constraint violation the user cannot read. This is the message
/// they get instead — deliberately the same sentence, and the same
/// [`Error::not_found`], that `repo::get` answers the identical question
/// with, so "that repository id does not exist" does not depend on which door
/// asked (ADR-0006).
async fn ensure_repository_exists(tx: &mut SqliteConnection, repository_id: &str) -> Result<()> {
    let count: i64 = sqlx::query_scalar!(
        "SELECT count(*) FROM repositories WHERE id = ?1",
        repository_id,
    )
    .fetch_one(&mut *tx)
    .await?;

    if count == 0 {
        return Err(Error::not_found(format!(
            "no repository with id {repository_id}"
        )));
    }
    Ok(())
}

/// Seam-contract D13's guard: a task's repository may be changed only while
/// it has no worktree and no runs.
///
/// Before either exists a task is a title and a plan, and mis-filing one is
/// an obvious mistake to want to undo. After: ADR-0005 has tied `branch` and
/// `worktree_path` to that repository — the same act creates both, so the
/// recorded worktree is the fact this reads — `runs` rows reference
/// transcripts produced inside it, and ADR-0008's branch chaining resolves a
/// base ref within it. A task moved out from under any of that is a task
/// whose recorded state describes a place it no longer lives.
///
/// Each refusal names what blocks it, because the panel renders this message
/// verbatim beside the selector it has disabled — and disabling that control
/// is a courtesy on top of this refusal, never a substitute for it: task 010
/// exposes `update_task` too, and a rule enforced in only one of the two
/// paths is a bug (ADR-0006). `Error::invalid` and no new `ErrorCode`
/// (seam-contract D8): the specificity that matters is in the sentence.
///
/// The worktree is checked first: it is already on the row where the run count
/// is a query, and when both hold it is the more useful of the two messages —
/// it names a place on disk the user can go and look at.
async fn ensure_repository_is_reassignable(tx: &mut SqliteConnection, task: &Task) -> Result<()> {
    if let Some(worktree_path) = &task.worktree_path {
        return Err(Error::invalid(format!(
            "cannot move \"{title}\" to another repository: it already has a worktree at {worktree_path}",
            title = task.title,
        )));
    }

    let run_count: i64 =
        sqlx::query_scalar!("SELECT count(*) FROM runs WHERE task_id = ?1", task.id)
            .fetch_one(&mut *tx)
            .await?;

    if run_count > 0 {
        // Two whole clauses rather than one format string with a pluralized
        // noun, for the reason `repo::remove` gives at its own count: English
        // inflects the verb as well as the noun.
        let reason = if run_count == 1 {
            "1 run has already been recorded against it".to_string()
        } else {
            format!("{run_count} runs have already been recorded against it")
        };
        return Err(Error::invalid(format!(
            "cannot move \"{title}\" to another repository: {reason}",
            title = task.title,
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
            created_at AS "created_at: DateTime<Utc>", updated_at AS "updated_at: DateTime<Utc>",
            source AS "source: MutationSource"
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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    /// The board's DTO is mirrored by hand in `src/types.ts`, and a key spelled
    /// `link_count` there instead of `linkCount` would typecheck on both sides
    /// and render `undefined` on every card. `db::models` pins [`Task`]'s own
    /// keys the same way and for the same reason; this pins what
    /// [`TaskSummary`] adds, including that the task is flattened alongside
    /// them rather than nested under a `task` key.
    #[test]
    fn a_task_summary_serializes_the_task_flat_alongside_its_board_fields() {
        let summary = TaskSummary {
            task: Task {
                id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
                repository_id: "3f2b1c00-0000-4000-8000-000000000002".to_string(),
                title: "Wire the board to the store".to_string(),
                plan: Some("## Steps\n1. ...".to_string()),
                extra_instructions: None,
                column: BoardColumn::Ready,
                position: 1.5,
                run_state: RunState::Failed,
                branch: None,
                worktree_path: None,
                strategy_mode: StrategyMode::Default,
                model: None,
                effort: None,
                strategy_plan: None,
                strategy_source: None,
                strategy_updated_at: None,
                created_at: "2026-08-20T12:00:00Z".parse().expect("a literal timestamp"),
                updated_at: "2026-08-20T12:30:00Z".parse().expect("a literal timestamp"),
                source: MutationSource::Ui,
            },
            link_count: 2,
            dependency_count: 1,
            blocked_by_incomplete: false,
            // seam-contract D9's case: the task is `failed`, and the only place
            // the word "interrupted" reaches the board is this exit class.
            last_run: Some(LastRunSummary {
                status: RunStatus::Interrupted,
                exit_class: Some(ExitClass::Interrupted),
                ended_at: Some("2026-08-20T12:29:00Z".parse().expect("a literal timestamp")),
            }),
        };

        let wire = serde_json::to_value(&summary).expect("a DTO must always serialize");

        assert_eq!(wire["id"], json!("3f2b1c00-0000-4000-8000-000000000001"));
        assert_eq!(wire["column"], json!("ready"));
        assert_eq!(wire["runState"], json!("failed"));
        assert_eq!(wire["linkCount"], json!(2));
        assert_eq!(wire["dependencyCount"], json!(1));
        assert_eq!(wire["blockedByIncomplete"], json!(false));
        // ADR-0019's provenance, flattened out of the task row like every other
        // column: `ui`, never `Ui`, because the value answers to SQLite's CHECK
        // as well as to TypeScript.
        assert_eq!(wire["source"], json!("ui"));
        assert_eq!(
            wire["lastRun"],
            json!({
                "status": "interrupted",
                "exitClass": "interrupted",
                "endedAt": "2026-08-20T12:29:00Z",
            })
        );
        assert!(
            wire.get("task").is_none(),
            "the task is flattened, not nested — `TaskSummary extends Task` in src/types.ts"
        );
    }

    #[test]
    fn a_task_summary_with_no_runs_serializes_last_run_as_null() {
        let summary = TaskSummary {
            task: Task {
                id: "3f2b1c00-0000-4000-8000-000000000003".to_string(),
                repository_id: "3f2b1c00-0000-4000-8000-000000000002".to_string(),
                title: "Brand new".to_string(),
                plan: None,
                extra_instructions: None,
                column: BoardColumn::NotReady,
                position: 0.0,
                run_state: RunState::Idle,
                branch: None,
                worktree_path: None,
                strategy_mode: StrategyMode::Default,
                model: None,
                effort: None,
                strategy_plan: None,
                strategy_source: None,
                strategy_updated_at: None,
                created_at: "2026-08-20T12:00:00Z".parse().expect("a literal timestamp"),
                updated_at: "2026-08-20T12:00:00Z".parse().expect("a literal timestamp"),
                source: MutationSource::Mcp,
            },
            link_count: 0,
            dependency_count: 0,
            blocked_by_incomplete: false,
            last_run: None,
        };

        let wire = serde_json::to_value(&summary).expect("a DTO must always serialize");

        // `null`, never an absent key: `lastRun: LastRunSummary | null` in
        // `src/types.ts` is a field the card reads, not one it probes for.
        assert_eq!(wire["lastRun"], json!(null));
    }
}
