//! Dependency edges: who a task is blocked by (ADR-0008, seam-contract D16).
//!
//! Task 011 owns *blocking* — the `blocked` run state, the scheduler
//! predicate, branch chaining, the panel. This module owns the write, because
//! `set_task_dependencies` is on ADR-0006's tool table and task 010 ships it:
//! a tool that stores an edge without checking for a cycle is not the tool
//! that table names.
//!
//! # Why checking each proposed edge on its own is complete
//!
//! Not obvious, and worth stating rather than rediscovering. The write only
//! changes `task_id`'s **outgoing** edges, so any cycle it could create must
//! pass through `task_id`. It is therefore enough to search the **existing**
//! graph from each proposed dependency and ask whether it reaches `task_id`;
//! the search stops the moment it arrives, so `task_id`'s own (about to be
//! replaced) edges are never expanded and cannot contribute a false positive.
//!
//! The walk is iterative with an explicit stack and a visited set. Recursion
//! would blow the stack on a long chain, and the visited set is what makes the
//! search terminate against a cycle **already in the table** — the schema only
//! forbids the one-row `task_id = depends_on_task_id` case, and ADR-0003
//! counts the user with a `sqlite3` CLI as a supported writer of this file.

use std::collections::{HashMap, HashSet};

use sqlx::SqliteConnection;

use crate::context::ServiceContext;
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::tasks::service::fetch_task_row;

/// The whole existing graph: task id -> the ids it depends on.
type Edges = HashMap<String, Vec<String>>;

/// Replaces the whole set of tasks `task_id` is blocked by (ADR-0008).
///
/// **Replace, never merge** — an empty slice clears every dependency. That is
/// what makes the MCP tool safe to call twice with the same list, and why its
/// description tells the caller to send the complete set every time.
///
/// Refuses, before writing anything: a task or dependency that does not exist,
/// a self-edge, a dependency in another repository, and any set that would
/// close a cycle. Every refusal is `Error::invalid` or `Error::not_found` with
/// the specifics in the message (seam-contract D8).
///
/// Returns the stored set sorted ascending — the order [`crate::tasks::get_task`]
/// reads it back in, so a caller comparing the two never sees a spurious
/// difference.
#[tracing::instrument(
    skip_all,
    fields(source = ctx.source.as_str(), task_id = %task_id, count = depends_on.len())
)]
pub async fn set_task_dependencies(
    ctx: &ServiceContext,
    task_id: &str,
    depends_on: &[String],
) -> Result<Vec<String>> {
    let mut tx = ctx.pool.begin().await?;
    let task = fetch_task_row(&mut *tx, task_id).await?;

    // Deduped preserving the caller's order, so the *first* offending id in
    // the request is the one an error names — an agent re-reading its own
    // call finds it where it wrote it.
    let requested = dedupe(depends_on);

    for dependency_id in &requested {
        if dependency_id == task_id {
            return Err(Error::invalid("a task cannot depend on itself"));
        }
        // Deliberately the same `no task with id {id}` sentence every other
        // not-found in this crate produces: which door asked does not change
        // the answer (ADR-0006).
        let dependency = fetch_task_row(&mut *tx, dependency_id).await?;
        if dependency.repository_id != task.repository_id {
            return Err(Error::invalid(format!(
                "cannot make \"{title}\" depend on \"{other}\": they are in different \
                 repositories, and a dependent task branches from its dependency",
                title = task.title,
                other = dependency.title,
            )));
        }
    }

    // One unscoped read of the whole table. It is one desktop user's board, and
    // scoping it to a repository would hide a hand-written cross-repository
    // edge from the walk — which is exactly the row the walk exists to catch.
    let edges = load_edges(&mut tx).await?;

    for dependency_id in &requested {
        if let Some(path) = find_path(&edges, dependency_id, task_id) {
            // Titles are read only here, on the path that is about to fail.
            let mut chain = Vec::with_capacity(path.len() + 1);
            chain.push(task_id.to_string());
            chain.extend(path);
            return Err(cycle_error(&mut tx, &chain).await?);
        }
    }

    let previous = current_dependencies(&mut tx, task_id).await?;

    sqlx::query!("DELETE FROM task_dependencies WHERE task_id = ?1", task_id)
        .execute(&mut *tx)
        .await?;
    for dependency_id in &requested {
        sqlx::query!(
            "INSERT INTO task_dependencies (task_id, depends_on_task_id) VALUES (?1, ?2)",
            task_id,
            dependency_id,
        )
        .execute(&mut *tx)
        .await?;
    }

    // Stamped even when the set is unchanged, for the reason `links.rs` gives
    // at its own `stamp_task_updated_at`: the caller asked for a write, and a
    // card whose `updated_at` did not move is a card the board will not
    // re-sort or re-render.
    let now = ctx.clock.now();
    sqlx::query!(
        "UPDATE tasks SET updated_at = ?1 WHERE id = ?2",
        now,
        task_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // The task itself, plus every task that gained or lost an incoming edge.
    // Strictly D12 says only `task_id` changed; an id means "re-read this",
    // which is always safe, and task 011's dependency panel will want them.
    ctx.publish(ChangeEvent::tasks(
        std::iter::once(task_id.to_string()).chain(symmetric_difference(&previous, &requested)),
    ));

    let mut stored = requested;
    stored.sort();
    Ok(stored)
}

/// The ids `task_id` depends on today, in the order they will come back.
async fn current_dependencies(tx: &mut SqliteConnection, task_id: &str) -> Result<Vec<String>> {
    let rows = sqlx::query_scalar!(
        "SELECT depends_on_task_id FROM task_dependencies WHERE task_id = ?1
         ORDER BY depends_on_task_id ASC",
        task_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows)
}

async fn load_edges(tx: &mut SqliteConnection) -> Result<Edges> {
    let rows = sqlx::query!("SELECT task_id, depends_on_task_id FROM task_dependencies")
        .fetch_all(&mut *tx)
        .await?;

    let mut edges: Edges = HashMap::new();
    for row in rows {
        edges
            .entry(row.task_id)
            .or_default()
            .push(row.depends_on_task_id);
    }
    Ok(edges)
}

/// The refusal, with every task in the loop named by title.
///
/// Words, never an arrow: the direction of `→` in a dependency graph is
/// exactly the thing a reader has to guess, and guessing wrong inverts the
/// meaning of the sentence.
async fn cycle_error(tx: &mut SqliteConnection, chain: &[String]) -> Result<Error> {
    let mut titles = Vec::with_capacity(chain.len());
    for id in chain {
        let title = sqlx::query_scalar!("SELECT title FROM tasks WHERE id = ?1", id)
            .fetch_optional(&mut *tx)
            .await?
            // A row that vanished between the walk and this read leaves the id,
            // which is still enough to find it. The message must not fail.
            .unwrap_or_else(|| id.clone());
        titles.push(format!("\"{title}\""));
    }

    Ok(Error::invalid(format!(
        "cannot save these dependencies: they would create a cycle — {chain}",
        chain = titles.join(" depends on "),
    )))
}

fn dedupe(ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.iter()
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect()
}

/// Every id that is in exactly one of the two sets — the tasks that gained or
/// lost an incoming edge.
fn symmetric_difference(previous: &[String], next: &[String]) -> Vec<String> {
    let before: HashSet<&String> = previous.iter().collect();
    let after: HashSet<&String> = next.iter().collect();
    before
        .symmetric_difference(&after)
        .map(|id| (*id).clone())
        .collect()
}

/// The chain of ids from `from` to `to`, following existing edges, or `None`
/// when `to` is not reachable.
///
/// Pure and database-free on purpose: the graph question is separable from the
/// storage question, and this is the half worth testing exhaustively without a
/// pool. `from` is included and `to` is the last element, so the caller can
/// print the whole loop.
fn find_path(edges: &Edges, from: &str, to: &str) -> Option<Vec<String>> {
    // Each stack frame is a node plus the path that reached it. Copying the
    // path per frame is the wrong trade at graph scale and exactly the right
    // one here: this graph is one user's board, and it buys an iterative walk
    // with no parent bookkeeping to get wrong.
    let mut stack = vec![vec![from.to_string()]];
    let mut visited = HashSet::new();

    while let Some(path) = stack.pop() {
        let node = path.last().expect("a path always has a node");
        if node == to {
            return Some(path);
        }
        if !visited.insert(node.clone()) {
            // Already expanded — this is what terminates the walk against a
            // cycle that is already in the table.
            continue;
        }
        for next in edges.get(node).into_iter().flatten() {
            let mut extended = path.clone();
            extended.push(next.clone());
            stack.push(extended);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn edges(pairs: &[(&str, &str)]) -> Edges {
        let mut edges: Edges = HashMap::new();
        for (task, depends_on) in pairs {
            edges
                .entry((*task).to_string())
                .or_default()
                .push((*depends_on).to_string());
        }
        edges
    }

    #[test]
    fn find_path_reports_the_chain_it_walked() {
        // b depends on c, c depends on a. Asking whether b reaches a is the
        // question `set_task_dependencies` asks before letting a depend on b.
        let graph = edges(&[("b", "c"), ("c", "a")]);

        assert_eq!(
            find_path(&graph, "b", "a"),
            Some(vec!["b".to_string(), "c".to_string(), "a".to_string()])
        );
    }

    #[test]
    fn find_path_returns_none_when_the_graph_is_a_forest() {
        // A diamond: d depends on b and c, both of which depend on a. Nothing
        // reaches d, so nothing here is a cycle.
        let graph = edges(&[("d", "b"), ("d", "c"), ("b", "a"), ("c", "a")]);

        assert_eq!(find_path(&graph, "b", "d"), None);
        assert_eq!(find_path(&graph, "a", "d"), None);
    }

    #[test]
    fn find_path_terminates_against_a_cycle_that_is_already_in_the_graph() {
        // Hand-written with the sqlite3 CLI, which ADR-0003 supports: b and c
        // depend on each other, and neither reaches d. Without the visited set
        // this walk never returns.
        let graph = edges(&[("b", "c"), ("c", "b")]);

        assert_eq!(find_path(&graph, "b", "d"), None);
    }

    #[test]
    fn find_path_finds_nothing_from_an_id_with_no_edges() {
        let graph = edges(&[("b", "c")]);

        assert_eq!(find_path(&graph, "lonely", "c"), None);
    }

    #[test]
    fn find_path_finds_the_zero_length_path_to_itself() {
        // The self-edge case, which the service refuses one layer up with its
        // own sentence rather than as a cycle — pinned here so a reader knows
        // the walk is not what produces that message.
        let graph = edges(&[]);

        assert_eq!(find_path(&graph, "a", "a"), Some(vec!["a".to_string()]));
    }

    #[test]
    fn dedupe_keeps_the_first_occurrence() {
        assert_eq!(
            dedupe(&["b".to_string(), "a".to_string(), "b".to_string()]),
            vec!["b".to_string(), "a".to_string()]
        );
    }

    #[test]
    fn the_symmetric_difference_names_every_task_that_gained_or_lost_an_edge() {
        let mut changed = symmetric_difference(
            &["a".to_string(), "b".to_string()],
            &["b".to_string(), "c".to_string()],
        );
        changed.sort();

        assert_eq!(changed, vec!["a".to_string(), "c".to_string()]);
    }
}
