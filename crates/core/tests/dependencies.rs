//! `set_task_dependencies` against a real migrated database (ADR-0008,
//! seam-contract D16).
//!
//! The graph walk itself has exhaustive, colocated unit tests in
//! `crates/core/src/tasks/dependencies.rs` — pure, database-free, and the only
//! cheap place to force a cycle that is *already* in the table. What only a
//! database can prove is here: that the write replaces rather than merges,
//! that every refusal happens before a single edge is written, that a rejected
//! request leaves the previous set exactly as it was, and that each mutation
//! publishes the tasks whose incoming edges changed (ADR-0018).

use std::slice;

use pretty_assertions::assert_eq;
use rimaia_core::db::BoardColumn;
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::TestContext;
use rimaia_core::{ChangeEvent, Clock, ErrorCode};
use sqlx::SqlitePool;

#[tokio::test]
async fn setting_a_dependency_stores_the_edge_and_publishes_both_tasks() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let api = create_task(&h, &repository_id, "Add the API").await;
    let caller = create_task(&h, &repository_id, "Call the API").await;
    drain(&mut h);

    let stored = tasks::set_task_dependencies(&h.context, &caller.id, slice::from_ref(&api.id))
        .await
        .expect("store one edge");

    assert_eq!(stored, vec![api.id.clone()]);
    assert_eq!(
        tasks::get_task(&h.context, &caller.id)
            .await
            .expect("read the task back")
            .depends_on,
        vec![api.id.clone()]
    );

    let published = task_ids(h.changes.try_recv().expect("a publication"));
    assert!(published.contains(&caller.id), "the task that was written");
    assert!(
        published.contains(&api.id),
        "and the task that gained an incoming edge: {published:?}"
    );
}

#[tokio::test]
async fn setting_dependencies_replaces_the_whole_set_rather_than_adding_to_it() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let first = create_task(&h, &repository_id, "First").await;
    let second = create_task(&h, &repository_id, "Second").await;
    let dependent = create_task(&h, &repository_id, "Dependent").await;

    tasks::set_task_dependencies(&h.context, &dependent.id, slice::from_ref(&first.id))
        .await
        .expect("store the first set");
    let stored =
        tasks::set_task_dependencies(&h.context, &dependent.id, slice::from_ref(&second.id))
            .await
            .expect("replace it");

    assert_eq!(stored, vec![second.id.clone()], "replaced, not merged");
}

#[tokio::test]
async fn an_empty_list_clears_every_dependency() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let api = create_task(&h, &repository_id, "Add the API").await;
    let caller = create_task(&h, &repository_id, "Call the API").await;
    tasks::set_task_dependencies(&h.context, &caller.id, slice::from_ref(&api.id))
        .await
        .expect("store one edge");

    let stored = tasks::set_task_dependencies(&h.context, &caller.id, &[])
        .await
        .expect("clear the set");

    assert_eq!(stored, Vec::<String>::new());
    assert_eq!(
        tasks::get_task(&h.context, &caller.id)
            .await
            .expect("read the task back")
            .depends_on,
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn a_repeated_id_in_the_request_is_stored_once() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let api = create_task(&h, &repository_id, "Add the API").await;
    let caller = create_task(&h, &repository_id, "Call the API").await;

    let stored =
        tasks::set_task_dependencies(&h.context, &caller.id, &[api.id.clone(), api.id.clone()])
            .await
            .expect("a repeated id is not an error");

    assert_eq!(stored, vec![api.id.clone()]);
}

#[tokio::test]
async fn setting_the_same_set_again_still_stamps_updated_at_and_publishes() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let api = create_task(&h, &repository_id, "Add the API").await;
    let caller = create_task(&h, &repository_id, "Call the API").await;
    tasks::set_task_dependencies(&h.context, &caller.id, slice::from_ref(&api.id))
        .await
        .expect("store one edge");
    h.clock.advance(chrono::Duration::minutes(5));
    drain(&mut h);

    tasks::set_task_dependencies(&h.context, &caller.id, slice::from_ref(&api.id))
        .await
        .expect("store the same edge again");

    let reread = tasks::get_task(&h.context, &caller.id)
        .await
        .expect("read the task back");
    assert_eq!(reread.task.updated_at, h.clock.now());
    assert!(
        task_ids(h.changes.try_recv().expect("a publication")).contains(&caller.id),
        "an unchanged set is still a write the board should hear about"
    );
}

#[tokio::test]
async fn a_task_cannot_depend_on_itself() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let task = create_task(&h, &repository_id, "Alone").await;

    let error = tasks::set_task_dependencies(&h.context, &task.id, slice::from_ref(&task.id))
        .await
        .expect_err("a self-edge is refused");

    // The service's own sentence, not the schema's constraint prose: the
    // `CHECK` is the backstop, this is what a caller reads.
    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(error.to_string(), "a task cannot depend on itself");
}

#[tokio::test]
async fn a_dependency_on_an_unknown_task_is_refused_naming_the_id() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let task = create_task(&h, &repository_id, "Dependent").await;

    let error = tasks::set_task_dependencies(&h.context, &task.id, &["nope".to_string()])
        .await
        .expect_err("an unknown dependency is refused");

    assert_eq!(error.code(), ErrorCode::NotFound);
    assert_eq!(error.to_string(), "no task with id nope");
}

#[tokio::test]
async fn a_dependency_in_another_repository_is_refused() {
    let h = TestContext::new().await;
    let here = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let elsewhere = seed_repository(&h.context.pool, "other", "/tmp/other").await;
    let dependent = create_task(&h, &here, "Call the API").await;
    let foreign = create_task(&h, &elsewhere, "Add the API").await;

    let error =
        tasks::set_task_dependencies(&h.context, &dependent.id, slice::from_ref(&foreign.id))
            .await
            .expect_err("a cross-repository edge is refused");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        "cannot make \"Call the API\" depend on \"Add the API\": they are in different \
         repositories, and a dependent task branches from its dependency"
    );
}

#[tokio::test]
async fn a_two_task_cycle_is_refused_naming_both_titles() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let a = create_task(&h, &repository_id, "A").await;
    let b = create_task(&h, &repository_id, "B").await;
    tasks::set_task_dependencies(&h.context, &b.id, slice::from_ref(&a.id))
        .await
        .expect("b depends on a");

    let error = tasks::set_task_dependencies(&h.context, &a.id, slice::from_ref(&b.id))
        .await
        .expect_err("closing the loop is refused");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        "cannot save these dependencies: they would create a cycle — \
         \"A\" depends on \"B\" depends on \"A\""
    );
}

#[tokio::test]
async fn a_four_task_cycle_is_refused_naming_the_whole_path() {
    // The case the schema's one-row `CHECK` cannot catch, and the reason the
    // walk exists at all.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let a = create_task(&h, &repository_id, "A").await;
    let b = create_task(&h, &repository_id, "B").await;
    let c = create_task(&h, &repository_id, "C").await;
    let d = create_task(&h, &repository_id, "D").await;
    tasks::set_task_dependencies(&h.context, &b.id, slice::from_ref(&a.id))
        .await
        .expect("b depends on a");
    tasks::set_task_dependencies(&h.context, &c.id, slice::from_ref(&b.id))
        .await
        .expect("c depends on b");
    tasks::set_task_dependencies(&h.context, &d.id, slice::from_ref(&c.id))
        .await
        .expect("d depends on c");

    let error = tasks::set_task_dependencies(&h.context, &a.id, slice::from_ref(&d.id))
        .await
        .expect_err("closing a four-task loop is refused");

    assert_eq!(
        error.to_string(),
        "cannot save these dependencies: they would create a cycle — \
         \"A\" depends on \"D\" depends on \"C\" depends on \"B\" depends on \"A\""
    );
}

#[tokio::test]
async fn a_diamond_is_not_a_cycle() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let base = create_task(&h, &repository_id, "Base").await;
    let left = create_task(&h, &repository_id, "Left").await;
    let right = create_task(&h, &repository_id, "Right").await;
    let top = create_task(&h, &repository_id, "Top").await;
    tasks::set_task_dependencies(&h.context, &left.id, slice::from_ref(&base.id))
        .await
        .expect("left depends on base");
    tasks::set_task_dependencies(&h.context, &right.id, slice::from_ref(&base.id))
        .await
        .expect("right depends on base");

    let mut expected = vec![left.id.clone(), right.id.clone()];
    expected.sort();
    let stored =
        tasks::set_task_dependencies(&h.context, &top.id, &[left.id.clone(), right.id.clone()])
            .await
            .expect("a diamond is a legal graph");

    assert_eq!(stored, expected);
}

#[tokio::test]
async fn a_rejected_cycle_leaves_the_existing_edges_intact() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let a = create_task(&h, &repository_id, "A").await;
    let b = create_task(&h, &repository_id, "B").await;
    let c = create_task(&h, &repository_id, "C").await;
    tasks::set_task_dependencies(&h.context, &b.id, slice::from_ref(&a.id))
        .await
        .expect("b depends on a");
    tasks::set_task_dependencies(&h.context, &a.id, slice::from_ref(&c.id))
        .await
        .expect("a depends on c");

    tasks::set_task_dependencies(&h.context, &a.id, slice::from_ref(&b.id))
        .await
        .expect_err("closing the loop is refused");

    assert_eq!(
        tasks::get_task(&h.context, &a.id)
            .await
            .expect("read a back")
            .depends_on,
        vec![c.id.clone()],
        "the refused write must not have deleted the previous set"
    );
}

#[tokio::test]
async fn an_unknown_dependency_is_reported_before_any_edge_is_written() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let api = create_task(&h, &repository_id, "Add the API").await;
    let other = create_task(&h, &repository_id, "Something else").await;
    let dependent = create_task(&h, &repository_id, "Dependent").await;
    tasks::set_task_dependencies(&h.context, &dependent.id, slice::from_ref(&other.id))
        .await
        .expect("store the original set");

    tasks::set_task_dependencies(
        &h.context,
        &dependent.id,
        &[api.id.clone(), "nope".to_string()],
    )
    .await
    .expect_err("an unknown id in the middle of a list is refused");

    assert_eq!(
        tasks::get_task(&h.context, &dependent.id)
            .await
            .expect("read the task back")
            .depends_on,
        vec![other.id.clone()],
        "a partially-applied set would be worse than no write at all"
    );
}

#[tokio::test]
async fn deleting_a_task_something_depends_on_is_still_refused() {
    // Task 004's guard, re-proved against an edge this module wrote:
    // `ON DELETE RESTRICT` constrains deleting a *task*, not an edge row, so
    // the replace-the-whole-set `DELETE` above does not weaken it.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let api = create_task(&h, &repository_id, "Add the API").await;
    let caller = create_task(&h, &repository_id, "Call the API").await;
    tasks::set_task_dependencies(&h.context, &caller.id, slice::from_ref(&api.id))
        .await
        .expect("store one edge");

    let error = tasks::delete_task(&h.context, &api.id)
        .await
        .expect_err("a task with dependents cannot be deleted");

    assert_eq!(error.code(), ErrorCode::Invalid);
    // Task 011 extended the message with the dependency context ADR-0008 gives
    // — "the edges must be removed first" — because a refusal that only names
    // who objects leaves the next step to be guessed. Asserted in full rather
    // than by substring: the sentence *is* the affordance (seam-contract D8).
    assert_eq!(
        error.to_string(),
        "cannot delete this task: 1 other task depends on it: Call the API. \
         Each of those branches from this one, so clear the dependency on it in \
         their task panels before deleting it.",
    );
}

#[tokio::test]
async fn the_delete_refusal_inflects_for_more_than_one_dependent() {
    // English inflects the verb as well as the noun, which is why the count is
    // two clauses rather than one pluralized format string.
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool, "rimaia", "/tmp/rimaia").await;
    let api = create_task(&h, &repository_id, "Add the API").await;
    for title in ["Call the API", "Document the API"] {
        let dependent = create_task(&h, &repository_id, title).await;
        tasks::set_task_dependencies(&h.context, &dependent.id, slice::from_ref(&api.id))
            .await
            .expect("store one edge");
    }

    let error = tasks::delete_task(&h.context, &api.id)
        .await
        .expect_err("a task with dependents cannot be deleted");

    assert_eq!(
        error.to_string(),
        "cannot delete this task: 2 other tasks depend on it: Call the API, Document the API. \
         Each of those branches from this one, so clear the dependency on it in \
         their task panels before deleting it.",
    );
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const NOW: &str = "2026-08-20T02:00:00+00:00";

async fn seed_repository(pool: &SqlitePool, name: &str, path: &str) -> String {
    let id = rimaia_core::db::new_id();
    sqlx::query!(
        r#"INSERT INTO repositories (id, name, path, default_branch, worktree_root, allow_unattended_runs, created_at)
           VALUES (?1, ?2, ?3, 'main', '/tmp/rimaia-worktrees', 0, ?4)"#,
        id,
        name,
        path,
        NOW,
    )
    .execute(pool)
    .await
    .expect("seed a repository");
    id
}

async fn create_task(h: &TestContext, repository_id: &str, title: &str) -> rimaia_core::db::Task {
    tasks::create_task(
        &h.context,
        NewTask {
            repository_id: repository_id.to_string(),
            title: title.to_string(),
            plan: Some("a plan".to_string()),
            extra_instructions: None,
            column: Some(BoardColumn::Ready),
            links: vec![],
        },
    )
    .await
    .expect("create a task fixture")
}

fn task_ids(event: ChangeEvent) -> Vec<String> {
    match event {
        ChangeEvent::Tasks(ids) => ids.to_vec(),
        other => panic!("expected a task change, got {other:?}"),
    }
}

/// Empties the subscriber so a test's assertion is about the publication its
/// own call made, not one a fixture made first.
fn drain(h: &mut TestContext) {
    while h.changes.try_recv().is_ok() {}
}
