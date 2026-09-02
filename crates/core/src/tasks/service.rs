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

use std::collections::HashMap;

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
use crate::strategy::{effective_strategy, EffectiveStrategy, StrategyOrigin};
use crate::tasks::position::{position_between, rebalance_column, rebalanced_positions, Placement};
use crate::tasks::strategy::{defaults_for_repository, ResolvedDefaults};
use crate::tasks::types::{NewTask, Patch, TaskFilter, TaskPatch};

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
    /// What a run would actually spawn with, and which link of the precedence
    /// chain said so — filled by [`apply_effective_strategy`], whose doc
    /// comment argues why these are computed here and not in the frontend.
    pub effective_model: Option<String>,
    pub effective_effort: Option<String>,
    pub effective_origin: StrategyOrigin,
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
    /// At least one of this task's dependencies is in a column that does not
    /// satisfy it (ADR-0008, seam-contract D12) — see
    /// [`BoardColumn::satisfies_a_dependency`] for why that is a column test
    /// and deliberately not a `runs.status` one.
    ///
    /// **Derived on every read, never stored.** ADR-0008's amendment of
    /// 2026-09-02 argues that at length: the predicate for B changes when *A*
    /// moves column, so a cached copy would live on the wrong row.
    pub blocked_by_incomplete: bool,
    /// The first unsatisfied dependency's title, in
    /// [`BoardColumn::board_rank`] then ascending `position` order — the same
    /// order ADR-0008's base-ref rule picks a base branch in, so the card names
    /// the head of the stalled chain rather than an arbitrary member of it.
    ///
    /// `None` exactly when [`blocked_by_incomplete`](TaskSummary::blocked_by_incomplete)
    /// is false. The card must *name* the blocker — task 011's acceptance
    /// criterion is "each showing A as the reason" — and a title is the only
    /// field that names it, which is why this is a fifth summary field rather
    /// than a lookup the frontend does per card.
    pub blocking_title: Option<String>,
    pub last_run: Option<LastRunSummary>,
    /// The same three fields [`TaskDetail`] carries, for the same reason — see
    /// [`apply_effective_strategy`]. The card renders a badge off them and
    /// never off [`Task::model`].
    pub effective_model: Option<String>,
    pub effective_effort: Option<String>,
    pub effective_origin: StrategyOrigin,
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
///
/// The three `effective_*` fields come out of this **unresolved** — no model,
/// no effort, [`StrategyOrigin::ClaudeCode`] — because they are not columns and
/// a row cannot answer for them. [`list_tasks`] fills them in immediately
/// afterwards, and is the only caller; anything else that reaches for
/// `query_as::<TaskSummary>` has to do the same or it is rendering a card that
/// claims nothing is configured.
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
            blocking_title: row.try_get("blocking_title")?,
            last_run,
            effective_model: None,
            effective_effort: None,
            effective_origin: StrategyOrigin::ClaudeCode,
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
///
/// # Blocking is computed here, on the column, and only here
///
/// ADR-0008's condition is `board_column IN ('in_review', 'done')` and nothing
/// else — [`BoardColumn::satisfies_a_dependency`] carries the two reasons it is
/// not a `runs.status = 'succeeded'` predicate, and both are ordinary rather
/// than exotic. The two dependency expressions below are a third and fourth
/// correlated subquery over `task_dependencies`, which is one user's board and
/// a handful of rows per task: D12's argument is against *fifty reads per board
/// read*, not against a subquery, and this keeps the count at one.
///
/// `blocking_title` orders by column rank and then ascending `position`,
/// deliberately not by `board_column ASC`. Within the unsatisfied pair the two
/// happen to agree — `'not_ready' < 'ready'` alphabetically as well as by rank
/// — but writing the coincidence down as the rule is how the *satisfied* pair
/// would later be ordered `done` before `in_review`, which is backwards. See
/// [`BoardColumn::board_rank`], which
/// [`the_summary_query_ranks_columns_the_way_board_rank_does`](tests::the_summary_query_ranks_columns_the_way_board_rank_does)
/// pins this literal against.
const TASK_SUMMARY_SELECT: &str = r#"
SELECT t.*,
       (SELECT count(*) FROM task_links WHERE task_id = t.id) AS link_count,
       (SELECT count(*) FROM task_dependencies WHERE task_id = t.id) AS dependency_count,
       EXISTS (SELECT 1
                 FROM task_dependencies d
                 JOIN tasks dep ON dep.id = d.depends_on_task_id
                WHERE d.task_id = t.id
                  AND dep.board_column NOT IN ('in_review', 'done')) AS blocked_by_incomplete,
       (SELECT dep.title
          FROM task_dependencies d
          JOIN tasks dep ON dep.id = d.depends_on_task_id
         WHERE d.task_id = t.id
           AND dep.board_column NOT IN ('in_review', 'done')
         ORDER BY CASE dep.board_column
                    WHEN 'not_ready' THEN 0
                    WHEN 'ready' THEN 1
                    WHEN 'in_review' THEN 2
                    WHEN 'done' THEN 3
                  END ASC,
                  dep.position ASC, dep.created_at ASC, dep.id ASC
         LIMIT 1) AS blocking_title,
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
/// what it depends on, its most recent run (by attempt number), and the
/// strategy a run would actually spawn with (see [`apply_effective_strategy`]).
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

    let defaults = defaults_for_repository(ctx, &task.repository_id).await?;
    let effective = effective_strategy(&task, &defaults.repository, &defaults.global);
    let effective_origin = strongest_origin(&effective);

    Ok(TaskDetail {
        task,
        links,
        depends_on,
        last_run,
        effective_model: effective.model,
        effective_effort: effective.effort,
        effective_origin,
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

    let mut summaries = query.fetch_all(&ctx.pool).await?;
    apply_effective_strategy(ctx, &mut summaries).await?;
    Ok(summaries)
}

/// Fills every card's effective model and effort after the board read.
///
/// After the query rather than inside it because the precedence chain is a
/// business rule (ADR-0016) and lives in [`effective_strategy`], not in SQL —
/// and because the defaults it reads are `settings` rows, which the board query
/// does not join to and should not learn how to.
///
/// One global read plus one per *distinct repository*, not one per card: a
/// fifty-card board across three repositories costs four settings reads, which
/// keeps seam-contract D12's "one query per board read, not fifty" true in
/// spirit. `HashMap` rather than sorting because the board's own ordering
/// already groups by repository and this must not depend on that continuing to
/// be the case.
async fn apply_effective_strategy(
    ctx: &ServiceContext,
    summaries: &mut [TaskSummary],
) -> Result<()> {
    let mut by_repository: HashMap<String, ResolvedDefaults> = HashMap::new();

    for summary in summaries.iter_mut() {
        let repository_id = summary.task.repository_id.clone();
        if !by_repository.contains_key(&repository_id) {
            by_repository.insert(
                repository_id.clone(),
                defaults_for_repository(ctx, &repository_id).await?,
            );
        }
        let defaults = &by_repository[&repository_id];

        let effective = effective_strategy(&summary.task, &defaults.repository, &defaults.global);
        summary.effective_origin = strongest_origin(&effective);
        summary.effective_model = effective.model;
        summary.effective_effort = effective.effort;
    }

    Ok(())
}

/// Seam-contract D17.6: what a task's mode becomes after an edit.
///
/// The rule lives here, in the service, rather than in the panel or in the MCP
/// handler, because both doors reach `update_task` and a rule enforced in one of
/// them is the defect ADR-0006 names. Three cases, in order of who is being
/// most explicit:
///
/// 1. **An explicit `strategy_mode` on the patch wins.** Somebody used the mode
///    selector; that is not a thing to second-guess.
/// 2. **Naming a model or an effort means `manual`.** Otherwise the pair would
///    be stored and then ignored: a task in `default` mode resolves through the
///    repository and global defaults and never reads its own columns, so a
///    dropdown that appeared to take effect would not have.
/// 3. **Clearing both flips `manual` back to `default`.** A manual task with
///    nothing selected is a mode with no content, and leaving it there would
///    pin the card to whatever `default` resolves to today while claiming the
///    user chose it.
///
/// A `planned` task is deliberately *not* returned to `default` by case 3: its
/// mode says a planner decides, and the planner has not run yet.
fn resolve_strategy_mode(
    requested: Option<StrategyMode>,
    current: StrategyMode,
    touched_a_field: bool,
    both_now_empty: bool,
) -> StrategyMode {
    if let Some(mode) = requested {
        return mode;
    }

    // `touched_a_field` gates both remaining cases, and it is the whole
    // correctness of this function. Deriving the answer from the *resulting*
    // row alone cannot tell "the user just cleared both" from "both were
    // already empty and this patch never mentioned them" — and the second is
    // every ordinary edit. A task set to `manual` with no model yet, whose
    // title is then edited, would be handed back to `default`, which under a
    // repository default of `planned` silently re-arms the planner the user
    // opted out of.
    if !touched_a_field {
        return current;
    }

    if both_now_empty {
        // Only a `manual` task. A `planned` one means "a planner decides",
        // which is still true of a card with no model on it yet.
        if current == StrategyMode::Manual {
            return StrategyMode::Default;
        }
        return current;
    }

    StrategyMode::Manual
}

/// The more specific of a strategy's two origins.
///
/// The card draws one badge, not two, so it needs one answer for "where did
/// this come from". Model and effort resolve independently — a task may name a
/// model and inherit its effort — and when they disagree the more specific one
/// is the honest label: a badge reading "from the repository default" on a card
/// that names its own model would be wrong in the direction that matters,
/// because it invites the reader to go change a default that this card is not
/// listening to.
///
/// [`StrategyOrigin::ClaudeCode`] is the least specific because it means the
/// flag was never passed at all.
fn strongest_origin(effective: &EffectiveStrategy) -> StrategyOrigin {
    fn specificity(origin: StrategyOrigin) -> u8 {
        match origin {
            StrategyOrigin::Task => 3,
            StrategyOrigin::Repository => 2,
            StrategyOrigin::Global => 1,
            StrategyOrigin::ClaudeCode => 0,
        }
    }

    if specificity(effective.model_origin) >= specificity(effective.effort_origin) {
        effective.model_origin
    } else {
        effective.effort_origin
    }
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
    // Whether the patch *mentioned* either field — set or cleared — not whether
    // the row ends up carrying one. An edit that never names them must leave
    // the mode exactly as it was; see `resolve_strategy_mode`.
    let touched_a_field =
        !matches!(patch.model, Patch::Unset) || !matches!(patch.effort, Patch::Unset);
    let model = patch.model.apply(current.model);
    let effort = patch.effort.apply(current.effort);
    let strategy_mode = resolve_strategy_mode(
        patch.strategy_mode,
        current.strategy_mode,
        touched_a_field,
        model.is_none() && effort.is_none(),
    );

    ensure_ready_has_a_plan(current.column, &plan, &title)?;

    let now = ctx.clock.now();
    sqlx::query!(
        r#"UPDATE tasks SET repository_id = ?1, title = ?2, plan = ?3, extra_instructions = ?4,
            model = ?5, effort = ?6, strategy_mode = ?7, position = ?8, updated_at = ?9
            WHERE id = ?10"#,
        placement.repository_id,
        title,
        plan,
        extra_instructions,
        model,
        effort,
        strategy_mode,
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
        // Task 011's Scope: "already in 004; extend the message with the
        // dependency context". The count and the names were already here; what
        // was missing is the *next step*, which ADR-0008 states as "the edges
        // must be removed first" and which is not guessable from a refusal that
        // only names who objects. Two clauses rather than a pluralized noun, for
        // the reason `repo::remove` gives at its own count: English inflects the
        // verb as well.
        let subject = if dependents.len() == 1 {
            "1 other task depends on it".to_string()
        } else {
            format!("{} other tasks depend on it", dependents.len())
        };
        return Err(Error::invalid(format!(
            "cannot delete this task: {subject}: {names}. \
             Each of those branches from this one, so clear the dependency on \
             it in their task panels before deleting it.",
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
            pr_url, resume_after AS "resume_after: DateTime<Utc>", base_ref,
            model, effort, run_environment, input_tokens, output_tokens,
            cache_read_tokens, cache_creation_tokens
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
            blocked_by_incomplete: true,
            blocking_title: Some("Add the API endpoint".to_string()),
            // seam-contract D9's case: the task is `failed`, and the only place
            // the word "interrupted" reaches the board is this exit class.
            last_run: Some(LastRunSummary {
                status: RunStatus::Interrupted,
                exit_class: Some(ExitClass::Interrupted),
                ended_at: Some("2026-08-20T12:29:00Z".parse().expect("a literal timestamp")),
            }),
            effective_model: Some("sonnet".to_string()),
            effective_effort: Some("high".to_string()),
            effective_origin: StrategyOrigin::Repository,
        };

        let wire = serde_json::to_value(&summary).expect("a DTO must always serialize");

        assert_eq!(wire["id"], json!("3f2b1c00-0000-4000-8000-000000000001"));
        assert_eq!(wire["column"], json!("ready"));
        assert_eq!(wire["runState"], json!("failed"));
        assert_eq!(wire["linkCount"], json!(2));
        assert_eq!(wire["dependencyCount"], json!(1));
        assert_eq!(wire["blockedByIncomplete"], json!(true));
        // The card has to *name* the blocker, not just show a badge — task
        // 011's criterion is "each showing A as the reason".
        assert_eq!(wire["blockingTitle"], json!("Add the API endpoint"));
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
            blocking_title: None,
            last_run: None,
            effective_model: None,
            effective_effort: None,
            effective_origin: StrategyOrigin::ClaudeCode,
        };

        let wire = serde_json::to_value(&summary).expect("a DTO must always serialize");

        // `null`, never an absent key: `lastRun: LastRunSummary | null` in
        // `src/types.ts` is a field the card reads, not one it probes for.
        assert_eq!(wire["lastRun"], json!(null));
        assert_eq!(wire["blockingTitle"], json!(null));
        // Same rule for the strategy badge: the card reads
        // `effectiveModel: string | null` and draws nothing for `null`, so an
        // absent key would make it render `undefined`.
        assert_eq!(wire["effectiveModel"], json!(null));
        assert_eq!(wire["effectiveOrigin"], json!("claude_code"));
    }

    // -----------------------------------------------------------------------
    // Seam-contract D17.6 — what an edit does to a task's mode
    //
    // The rule lives in the service because both doors reach `update_task`, so
    // it is tested where it lives rather than once per door. Every case below
    // is a sentence of D17.6.
    // -----------------------------------------------------------------------

    #[test]
    fn choosing_a_model_takes_the_task_over_and_makes_it_manual() {
        // Otherwise the pair is stored and then ignored: a task in `default`
        // mode resolves through the repository and global defaults and never
        // reads its own columns, so a dropdown that appeared to take effect
        // would not have.
        assert_eq!(
            resolve_strategy_mode(None, StrategyMode::Default, true, false),
            StrategyMode::Manual,
        );
    }

    #[test]
    fn clearing_both_of_them_hands_a_manual_task_back_to_the_default() {
        // A manual task with nothing selected is a mode with no content, and
        // leaving it there would pin the card to whatever `default` resolves to
        // today while claiming the user chose it.
        assert_eq!(
            resolve_strategy_mode(None, StrategyMode::Manual, true, true),
            StrategyMode::Default,
        );
    }

    #[test]
    fn an_explicit_mode_wins_over_what_the_fields_imply() {
        // Somebody used the mode selector; that is not a thing to second-guess.
        // Both directions, because the interesting one is the mode that
        // disagrees with the payload beside it.
        assert_eq!(
            resolve_strategy_mode(
                Some(StrategyMode::Planned),
                StrategyMode::Default,
                true,
                false
            ),
            StrategyMode::Planned,
        );
        assert_eq!(
            resolve_strategy_mode(Some(StrategyMode::Manual), StrategyMode::Manual, true, true),
            StrategyMode::Manual,
            "asking for manual while clearing both is still asking for manual",
        );
    }

    #[test]
    fn clearing_both_of_them_does_not_drag_a_planned_task_back_to_the_default() {
        // The exception D17.6 spells out: `planned` says a planner decides, and
        // the planner has not run yet. Returning it to `default` here would
        // silently cancel the planning nobody asked to cancel.
        assert_eq!(
            resolve_strategy_mode(None, StrategyMode::Planned, true, true),
            StrategyMode::Planned,
        );
        assert_eq!(
            resolve_strategy_mode(None, StrategyMode::Planned, false, true),
            StrategyMode::Planned,
            "an edit that touched neither field is not an edit to the mode",
        );
    }

    #[test]
    fn an_edit_that_names_neither_field_leaves_the_mode_exactly_as_it_was() {
        for mode in [
            StrategyMode::Default,
            StrategyMode::Manual,
            StrategyMode::Planned,
        ] {
            assert_eq!(resolve_strategy_mode(None, mode, false, false), mode);
        }
    }

    // -----------------------------------------------------------------------
    // One badge for two origins
    // -----------------------------------------------------------------------

    #[test]
    fn a_card_that_names_its_own_model_does_not_claim_to_be_showing_a_default() {
        // The failure that matters is the direction: a badge reading "from the
        // repository default" on a card that names its own model invites the
        // reader to go and change a default this card is not listening to.
        assert_eq!(
            strongest_origin(&resolved(StrategyOrigin::Task, StrategyOrigin::Repository)),
            StrategyOrigin::Task,
        );
        assert_eq!(
            strongest_origin(&resolved(StrategyOrigin::Repository, StrategyOrigin::Task)),
            StrategyOrigin::Task,
            "the more specific one wins whichever half it is",
        );
    }

    #[test]
    fn the_origins_rank_task_then_repository_then_global_then_the_cli_itself() {
        // `ClaudeCode` is least specific because it means the flag was never
        // passed at all, so anything else beside it is the honest label.
        let ranked = [
            StrategyOrigin::Task,
            StrategyOrigin::Repository,
            StrategyOrigin::Global,
            StrategyOrigin::ClaudeCode,
        ];
        for (at, stronger) in ranked.iter().enumerate() {
            for weaker in &ranked[at + 1..] {
                assert_eq!(
                    strongest_origin(&resolved(*stronger, *weaker)),
                    *stronger,
                    "{stronger:?} beside {weaker:?}",
                );
                assert_eq!(strongest_origin(&resolved(*weaker, *stronger)), *stronger);
            }
        }
    }

    #[test]
    fn two_halves_from_the_same_level_report_that_level() {
        assert_eq!(
            strongest_origin(&resolved(StrategyOrigin::Global, StrategyOrigin::Global)),
            StrategyOrigin::Global,
        );
    }

    // -----------------------------------------------------------------------
    // ADR-0008's blocking predicate, as SQL
    // -----------------------------------------------------------------------

    #[test]
    fn the_summary_query_ranks_columns_the_way_board_rank_does() {
        // The `CASE` in `TASK_SUMMARY_SELECT` is a hand-typed copy of
        // `BoardColumn::board_rank`, and the two disagreeing is invisible: the
        // query still runs, still returns a title, and returns the *wrong*
        // dependency's title only on a board where two dependencies sit in
        // different columns. This is the check that makes the copy safe.
        for column in BoardColumn::ALL {
            assert!(
                TASK_SUMMARY_SELECT.contains(&format!(
                    "WHEN '{}' THEN {}",
                    column.as_sql(),
                    column.board_rank(),
                )),
                "{column:?} is ranked differently in the query than in board_rank",
            );
        }
    }

    #[test]
    fn the_summary_query_calls_exactly_in_review_and_done_satisfying() {
        // Spelled out in SQL rather than through `satisfies_a_dependency`, so
        // this asserts the two agree. A third column quietly added to the `NOT
        // IN` list would unblock work whose dependency never ran.
        let satisfying: Vec<&str> = BoardColumn::ALL
            .into_iter()
            .filter(|column| column.satisfies_a_dependency())
            .map(BoardColumn::as_sql)
            .collect();

        assert_eq!(satisfying, vec!["in_review", "done"]);
        assert_eq!(
            TASK_SUMMARY_SELECT
                .matches("board_column NOT IN ('in_review', 'done')")
                .count(),
            2,
            "both dependency expressions must use the same satisfaction test",
        );
    }

    // -----------------------------------------------------------------------
    // The board read's defaults
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn every_card_resolves_against_its_own_repositorys_defaults_however_they_interleave() {
        // The memoization is keyed by repository, and a cache that answered for
        // the whole board with the first repository it saw would still pass a
        // single-repository test. So the cards alternate: A, B, A, B. The board's
        // own ordering happens to group by repository today, and this must not
        // depend on that continuing to be true.
        let h = crate::testing::TestContext::new().await;
        let first = seed_repository(&h.context, "first").await;
        let second = seed_repository(&h.context, "second").await;

        crate::strategy::settings::set_repository_default(
            &h.context,
            &first,
            &crate::strategy::StrategyDefaults {
                mode: StrategyMode::Default,
                model: Some("opus".to_string()),
                effort: Some("high".to_string()),
            },
        )
        .await
        .expect("the first repository's defaults");
        crate::strategy::settings::set_global_default(
            &h.context,
            &crate::strategy::StrategyDefaults {
                mode: StrategyMode::Default,
                model: Some("haiku".to_string()),
                effort: None,
            },
        )
        .await
        .expect("the global defaults beneath them");

        let mut board = vec![
            card(&first, StrategyMode::Default, None),
            card(&second, StrategyMode::Default, None),
            // A manual card in the *first* repository, which is the one whose
            // defaults are memoized: its own model has to win anyway.
            card(&first, StrategyMode::Manual, Some("sonnet")),
            card(&second, StrategyMode::Default, None),
        ];

        apply_effective_strategy(&h.context, &mut board)
            .await
            .expect("the board resolves");

        assert_eq!(
            board
                .iter()
                .map(|summary| (
                    summary.effective_model.as_deref(),
                    summary.effective_effort.as_deref(),
                    summary.effective_origin,
                ))
                .collect::<Vec<_>>(),
            vec![
                (Some("opus"), Some("high"), StrategyOrigin::Repository),
                // The second repository configures nothing, so it falls through
                // to the global default — never to the first repository's.
                (Some("haiku"), None, StrategyOrigin::Global),
                (Some("sonnet"), Some("high"), StrategyOrigin::Task),
                (Some("haiku"), None, StrategyOrigin::Global),
            ],
        );
    }

    #[tokio::test]
    async fn an_empty_board_reads_no_defaults_at_all() {
        // The loop reads per distinct repository, so no cards means no reads —
        // and the function still has to succeed rather than assume a first row.
        let h = crate::testing::TestContext::new().await;
        let mut board: Vec<TaskSummary> = Vec::new();

        apply_effective_strategy(&h.context, &mut board)
            .await
            .expect("an empty board is not an error");

        assert!(board.is_empty());
    }

    /// An [`EffectiveStrategy`] whose only interesting fields are its two
    /// origins — the pair [`strongest_origin`] chooses between.
    fn resolved(model_origin: StrategyOrigin, effort_origin: StrategyOrigin) -> EffectiveStrategy {
        EffectiveStrategy {
            mode: StrategyMode::Default,
            model: None,
            effort: None,
            model_origin,
            effort_origin,
        }
    }

    /// A registered repository, seeded directly: this module's subject is the
    /// strategy chain, and `repo::register` would drag a real checkout into a
    /// test that never looks at one.
    async fn seed_repository(ctx: &ServiceContext, name: &str) -> String {
        let id = crate::db::new_id();
        sqlx::query(
            "INSERT INTO repositories (id, name, path, default_branch, worktree_root,
                allow_unattended_runs, created_at)
             VALUES (?1, ?2, ?3, 'main', '/tmp/rimaia-worktrees', 0, ?4)",
        )
        .bind(&id)
        .bind(name)
        .bind(format!("/tmp/{name}"))
        .bind(ctx.clock.now())
        .execute(&ctx.pool)
        .await
        .expect("seed a repository");
        id
    }

    /// One card on the board, with everything [`apply_effective_strategy`] does
    /// not read left at its most boring value.
    fn card(repository_id: &str, mode: StrategyMode, model: Option<&str>) -> TaskSummary {
        TaskSummary {
            task: Task {
                id: crate::db::new_id(),
                repository_id: repository_id.to_string(),
                title: "A card".to_string(),
                plan: None,
                extra_instructions: None,
                column: BoardColumn::Ready,
                position: 1.0,
                run_state: RunState::Idle,
                branch: None,
                worktree_path: None,
                strategy_mode: mode,
                model: model.map(str::to_string),
                effort: None,
                strategy_plan: None,
                strategy_source: None,
                strategy_updated_at: None,
                created_at: crate::testing::test_epoch(),
                updated_at: crate::testing::test_epoch(),
                source: MutationSource::Ui,
            },
            link_count: 0,
            dependency_count: 0,
            blocked_by_incomplete: false,
            blocking_title: None,
            last_run: None,
            // Unresolved, exactly as `FromRow` leaves them — which is the state
            // `apply_effective_strategy` is given.
            effective_model: None,
            effective_effort: None,
            effective_origin: StrategyOrigin::ClaudeCode,
        }
    }
}
