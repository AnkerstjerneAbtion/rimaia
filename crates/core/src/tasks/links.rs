//! Task links: zero-or-more `{label, url}` external references, ordered
//! independently of the board (ADR-0007). Add, update, remove, reorder.
//!
//! Ordering here is the same fractional scheme `service.rs`'s `move_task`
//! uses for cards — [`position_between`] and [`rebalanced_positions`],
//! scoped to one task's links instead of one repository's column. There is
//! no `rebalance_link_positions` in `position.rs`: that module's
//! `rebalance_column` is hardcoded to the `tasks` table on purpose (its own
//! doc comment explains why the transaction obligation matters), so a link's
//! renumber is its own small query here rather than a forced generalisation
//! of that one.

use sqlx::SqliteConnection;

use crate::context::ServiceContext;
use crate::db::TaskLink;
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::tasks::position::{position_between, rebalanced_positions, Placement};
use crate::tasks::types::{NewTaskLink, TaskLinkPatch};

/// Appends a link to the bottom of a task's link list.
#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), task_id = %task_id))]
pub async fn add_task_link(
    ctx: &ServiceContext,
    task_id: &str,
    input: NewTaskLink,
) -> Result<TaskLink> {
    validate_link(&input.label, &input.url)?;

    let mut tx = ctx.pool.begin().await?;
    ensure_task_exists(&mut tx, task_id).await?;

    let position = append_link_position(&mut tx, task_id).await?;
    let id = crate::db::new_id();

    sqlx::query!(
        "INSERT INTO task_links (id, task_id, label, url, position) VALUES (?1, ?2, ?3, ?4, ?5)",
        id,
        task_id,
        input.label,
        input.url,
        position,
    )
    .execute(&mut *tx)
    .await?;

    stamp_task_updated_at(ctx, &mut tx, task_id).await?;

    tx.commit().await?;

    // Publish before the read-back: the row is already committed, so a
    // failure in `fetch_link_row` below must not cost the notification for a
    // mutation that already happened (ADR-0018).
    ctx.publish(ChangeEvent::tasks([task_id.to_string()]));
    let link = fetch_link_row(&ctx.pool, &id).await?;
    Ok(link)
}

/// Patch semantics on a link: fields left `None` in `patch` keep their
/// current value. Unlike [`crate::tasks::TaskPatch`], plain `Option` is
/// enough — `label` and `url` are both `NOT NULL`, so there is no "clear"
/// this type needs to represent.
#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), link_id = %link_id))]
pub async fn update_task_link(
    ctx: &ServiceContext,
    link_id: &str,
    patch: TaskLinkPatch,
) -> Result<TaskLink> {
    let mut tx = ctx.pool.begin().await?;
    let current = fetch_link_row(&mut *tx, link_id).await?;

    let label = patch.label.unwrap_or(current.label);
    let url = patch.url.unwrap_or(current.url);
    validate_link(&label, &url)?;

    sqlx::query!(
        "UPDATE task_links SET label = ?1, url = ?2 WHERE id = ?3",
        label,
        url,
        link_id,
    )
    .execute(&mut *tx)
    .await?;

    stamp_task_updated_at(ctx, &mut tx, &current.task_id).await?;

    tx.commit().await?;

    // Publish before the read-back — see `add_task_link`'s identical comment.
    ctx.publish(ChangeEvent::tasks([current.task_id]));
    let updated = fetch_link_row(&ctx.pool, link_id).await?;
    Ok(updated)
}

/// One link by its own id.
///
/// A read, so it takes the context only for the pool and publishes nothing.
/// Task 010 needs it: every MCP tool answers with the whole task it touched,
/// and `remove_task_link` is handed a link id — the owning task has to be
/// known *before* the row is deleted.
pub async fn get_task_link(ctx: &ServiceContext, link_id: &str) -> Result<TaskLink> {
    fetch_link_row(&ctx.pool, link_id).await
}

#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), link_id = %link_id))]
pub async fn remove_task_link(ctx: &ServiceContext, link_id: &str) -> Result<()> {
    let mut tx = ctx.pool.begin().await?;
    let current = fetch_link_row(&mut *tx, link_id).await?;

    sqlx::query!("DELETE FROM task_links WHERE id = ?1", link_id)
        .execute(&mut *tx)
        .await?;

    stamp_task_updated_at(ctx, &mut tx, &current.task_id).await?;

    tx.commit().await?;

    ctx.publish(ChangeEvent::tasks([current.task_id]));
    Ok(())
}

/// Reorders a link among its task's other links, between `before_id` and
/// `after_id` — the same neighbour contract `move_task` uses for cards, and
/// the same "no neighbour named is only legal when nothing else is there"
/// rule for the same reason (see `service.rs::resolve_task_position`).
#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), link_id = %link_id))]
pub async fn reorder_task_link(
    ctx: &ServiceContext,
    link_id: &str,
    before_id: Option<&str>,
    after_id: Option<&str>,
) -> Result<TaskLink> {
    if before_id == Some(link_id) || after_id == Some(link_id) {
        return Err(Error::invalid("a link cannot be reordered next to itself"));
    }

    let mut tx = ctx.pool.begin().await?;
    let current = fetch_link_row(&mut *tx, link_id).await?;

    let position =
        resolve_link_position(&mut tx, &current.task_id, link_id, before_id, after_id).await?;

    sqlx::query!(
        "UPDATE task_links SET position = ?1 WHERE id = ?2",
        position,
        link_id,
    )
    .execute(&mut *tx)
    .await?;

    stamp_task_updated_at(ctx, &mut tx, &current.task_id).await?;

    tx.commit().await?;

    // Publish before the read-back — see `add_task_link`'s identical comment.
    ctx.publish(ChangeEvent::tasks([current.task_id]));
    let updated = fetch_link_row(&ctx.pool, link_id).await?;
    Ok(updated)
}

// ---------------------------------------------------------------------------
// Validation and row access
// ---------------------------------------------------------------------------

fn validate_link(label: &str, url: &str) -> Result<()> {
    if label.trim().is_empty() {
        return Err(Error::invalid("a task link needs a non-blank label"));
    }
    if url.trim().is_empty() {
        return Err(Error::invalid("a task link needs a non-blank url"));
    }
    Ok(())
}

async fn ensure_task_exists(tx: &mut SqliteConnection, task_id: &str) -> Result<()> {
    let exists: i64 = sqlx::query_scalar!("SELECT count(*) FROM tasks WHERE id = ?1", task_id)
        .fetch_one(&mut *tx)
        .await?;
    if exists == 0 {
        return Err(Error::not_found(format!("no task with id {task_id}")));
    }
    Ok(())
}

/// Stamps the owning task's `updated_at`, inside the caller's transaction.
///
/// A link is part of a task's own editable state (ADR-0007), so adding,
/// editing, removing or reordering one is a change to the task exactly as
/// much as editing its title is — task 004's "every mutation stamps
/// `updated_at` and emits a change event" rule applies here too, not only to
/// the writes in `service.rs`.
async fn stamp_task_updated_at(
    ctx: &ServiceContext,
    tx: &mut SqliteConnection,
    task_id: &str,
) -> Result<()> {
    let now = ctx.clock.now();
    sqlx::query!(
        "UPDATE tasks SET updated_at = ?1 WHERE id = ?2",
        now,
        task_id,
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn fetch_link_row<'e, E>(executor: E, id: &str) -> Result<TaskLink>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query_as!(
        TaskLink,
        "SELECT id, task_id, label, url, position FROM task_links WHERE id = ?1",
        id,
    )
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| Error::not_found(format!("no task link with id {id}")))
}

// ---------------------------------------------------------------------------
// Fractional placement, scoped to one task's links
// ---------------------------------------------------------------------------

async fn append_link_position(tx: &mut SqliteConnection, task_id: &str) -> Result<f64> {
    let last = last_link_position(&mut *tx, task_id, None).await?;
    match position_between(last, None) {
        Placement::At(position) => Ok(position),
        Placement::NeedsRebalance => {
            rebalance_link_positions(tx, task_id).await?;
            let last = last_link_position(&mut *tx, task_id, None).await?;
            match position_between(last, None) {
                Placement::At(position) => Ok(position),
                Placement::NeedsRebalance => Err(Error::internal(
                    "a freshly rebalanced set of links still has no room to append",
                )),
            }
        }
    }
}

async fn resolve_link_position(
    tx: &mut SqliteConnection,
    task_id: &str,
    moving_id: &str,
    before_id: Option<&str>,
    after_id: Option<&str>,
) -> Result<f64> {
    let (before_position, after_position) =
        link_neighbour_positions(&mut *tx, task_id, before_id, after_id).await?;

    if before_position.is_none() && after_position.is_none() {
        if link_task_has_other_rows(&mut *tx, task_id, moving_id).await? {
            return Err(Error::invalid(
                "reordering a link with neither a before nor an after neighbour is only valid when it is the only link on the task",
            ));
        }
        return Ok(0.0);
    }

    match position_between(before_position, after_position) {
        Placement::At(position) => Ok(position),
        Placement::NeedsRebalance => {
            rebalance_link_positions(tx, task_id).await?;
            let (before_position, after_position) =
                link_neighbour_positions(&mut *tx, task_id, before_id, after_id).await?;
            match position_between(before_position, after_position) {
                Placement::At(position) => Ok(position),
                Placement::NeedsRebalance => Err(Error::internal(
                    "a freshly rebalanced set of links still has no room for this drop",
                )),
            }
        }
    }
}

async fn link_neighbour_positions(
    tx: &mut SqliteConnection,
    task_id: &str,
    before_id: Option<&str>,
    after_id: Option<&str>,
) -> Result<(Option<f64>, Option<f64>)> {
    let before = match before_id {
        Some(before_id) => Some(link_position_on_task(&mut *tx, task_id, before_id).await?),
        None => None,
    };
    let after = match after_id {
        Some(after_id) => Some(link_position_on_task(&mut *tx, task_id, after_id).await?),
        None => None,
    };
    Ok((before, after))
}

async fn link_position_on_task(tx: &mut SqliteConnection, task_id: &str, id: &str) -> Result<f64> {
    sqlx::query_scalar!(
        "SELECT position FROM task_links WHERE id = ?1 AND task_id = ?2",
        id,
        task_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        Error::invalid(format!(
            "link {id} is not on the same task, so it cannot be used as a neighbour"
        ))
    })
}

async fn last_link_position(
    tx: &mut SqliteConnection,
    task_id: &str,
    excluding: Option<&str>,
) -> Result<Option<f64>> {
    sqlx::query_scalar!(
        r#"SELECT position FROM task_links
           WHERE task_id = ?1 AND id IS NOT ?2
           ORDER BY position DESC, id DESC LIMIT 1"#,
        task_id,
        excluding,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(Error::from)
}

async fn link_task_has_other_rows(
    tx: &mut SqliteConnection,
    task_id: &str,
    excluding: &str,
) -> Result<bool> {
    let count: i64 = sqlx::query_scalar!(
        "SELECT count(*) FROM task_links WHERE task_id = ?1 AND id != ?2",
        task_id,
        excluding,
    )
    .fetch_one(&mut *tx)
    .await?;
    Ok(count > 0)
}

/// Renumbers every link of one task to evenly spaced positions, in the order
/// they currently sort. The same shape as `position.rs::rebalance_column`,
/// scoped to `task_links` instead of `tasks`; see the module doc for why
/// that module does not own this query.
async fn rebalance_link_positions(tx: &mut SqliteConnection, task_id: &str) -> Result<()> {
    let ids = sqlx::query_scalar!(
        "SELECT id FROM task_links WHERE task_id = ?1 ORDER BY position ASC, id ASC",
        task_id,
    )
    .fetch_all(&mut *tx)
    .await?;

    for (id, position) in ids.iter().zip(rebalanced_positions(ids.len())) {
        sqlx::query!(
            "UPDATE task_links SET position = ?1 WHERE id = ?2",
            position,
            id,
        )
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}
