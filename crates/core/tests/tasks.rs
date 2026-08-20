//! The task service, against a real migrated database (ADR-0007, ADR-0018,
//! task 004's acceptance criteria).
//!
//! `set_run_state`'s transition table has its own exhaustive, colocated unit
//! test in `crates/core/src/tasks/run_state.rs` — nothing here re-proves
//! that matrix. What only a database (and a subscriber) can prove is here:
//! that every operation reads and writes the rows it claims to, that patch
//! semantics genuinely leave untouched fields alone, that a forced rebalance
//! really does make room, and that every mutation publishes exactly one
//! [`ChangeEvent`] naming the task it touched — which, per ADR-0018's own
//! consequence, is a `cargo test -p rimaia-core` assertion rather than
//! something needing a window.

use pretty_assertions::assert_eq;
use rimaia_core::db::{BoardColumn, RunState};
use rimaia_core::tasks::{self, NewTask, NewTaskLink, Patch, TaskFilter, TaskLinkPatch, TaskPatch};
use rimaia_core::testing::TestContext;
use rimaia_core::{ChangeEvent, Clock, ErrorCode};
use sqlx::SqlitePool;

// ---------------------------------------------------------------------------
// create_task
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_created_task_defaults_to_not_ready_and_idle_and_publishes_its_id() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;

    let created = tasks::create_task(
        &h.context,
        NewTask {
            repository_id: repository_id.clone(),
            title: "Wire the board".to_string(),
            plan: None,
            extra_instructions: None,
            column: None,
            links: vec![],
        },
    )
    .await
    .expect("create a task");

    assert_eq!(created.repository_id, repository_id);
    assert_eq!(created.title, "Wire the board");
    assert_eq!(created.column, BoardColumn::NotReady);
    assert_eq!(created.run_state, RunState::Idle);
    assert_eq!(created.position, 0.0, "the first card in an empty column");
    assert_eq!(created.created_at, h.clock.now());
    assert_eq!(created.updated_at, h.clock.now());

    assert_eq!(
        h.changes.try_recv().expect("a publication"),
        ChangeEvent::tasks([created.id])
    );
}

#[tokio::test]
async fn creating_a_second_task_appends_below_the_first() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;

    let first = create_ready(&h, &repository_id, "first", "a plan").await;
    let second = create_ready(&h, &repository_id, "second", "a plan").await;

    assert!(
        second.position > first.position,
        "second ({}) must sort below first ({})",
        second.position,
        first.position
    );
}

#[tokio::test]
async fn a_blank_title_is_refused() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;

    let error = tasks::create_task(
        &h.context,
        NewTask {
            repository_id,
            title: "   ".to_string(),
            plan: None,
            extra_instructions: None,
            column: None,
            links: vec![],
        },
    )
    .await
    .expect_err("a blank title must be refused");

    assert_eq!(error.code(), ErrorCode::Invalid);
}

#[tokio::test]
async fn creating_directly_into_ready_without_a_plan_is_refused() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;

    let error = tasks::create_task(
        &h.context,
        NewTask {
            repository_id,
            title: "no plan yet".to_string(),
            plan: None,
            extra_instructions: None,
            column: Some(BoardColumn::Ready),
            links: vec![],
        },
    )
    .await
    .expect_err("ready with no plan must be refused");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert!(
        h.changes.try_recv().is_err(),
        "a refused create must publish nothing"
    );
}

#[tokio::test]
async fn links_supplied_at_creation_are_stored_in_order() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;

    let created = tasks::create_task(
        &h.context,
        NewTask {
            repository_id,
            title: "with links".to_string(),
            plan: None,
            extra_instructions: None,
            column: None,
            links: vec![
                NewTaskLink {
                    label: "Design doc".to_string(),
                    url: "https://example.com/design".to_string(),
                },
                NewTaskLink {
                    label: "Issue".to_string(),
                    url: "https://example.com/issue".to_string(),
                },
            ],
        },
    )
    .await
    .expect("create a task with links");

    let detail = tasks::get_task(&h.context, &created.id)
        .await
        .expect("read it back");

    assert_eq!(
        detail
            .links
            .iter()
            .map(|l| l.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Design doc", "Issue"]
    );
    assert!(detail.links[0].position < detail.links[1].position);
}

#[tokio::test]
async fn a_whitespace_only_plan_is_stored_as_no_plan_not_an_empty_string() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;

    let created = tasks::create_task(
        &h.context,
        NewTask {
            repository_id,
            title: "blank plan".to_string(),
            plan: Some("   ".to_string()),
            extra_instructions: None,
            column: None,
            links: vec![],
        },
    )
    .await
    .expect("create a task with a whitespace-only plan");

    assert_eq!(
        created.plan, None,
        "NULL is the only spelling of \"no plan\" (the migration's own comment on tasks.plan)"
    );
}

// ---------------------------------------------------------------------------
// get_task
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_task_returns_links_dependencies_and_the_latest_run() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let blocker = create_ready(&h, &repository_id, "blocker", "a plan").await;
    let task = create_ready(&h, &repository_id, "dependent", "a plan").await;

    seed_dependency(&h.context.pool, &task.id, &blocker.id).await;
    seed_run(&h.context.pool, &task.id, 1, "session-1").await;
    let latest_run_id = seed_run(&h.context.pool, &task.id, 2, "session-1").await;

    let detail = tasks::get_task(&h.context, &task.id)
        .await
        .expect("read it back");

    assert_eq!(detail.task.id, task.id);
    assert_eq!(detail.depends_on, vec![blocker.id]);
    assert_eq!(
        detail.last_run.expect("a most recent run").id,
        latest_run_id,
        "the higher attempt number must win"
    );
}

#[tokio::test]
async fn get_task_on_a_missing_id_is_not_found() {
    let h = TestContext::new().await;

    let error = tasks::get_task(&h.context, "no-such-task")
        .await
        .expect_err("a missing task must be reported");

    assert_eq!(error.code(), ErrorCode::NotFound);
}

// ---------------------------------------------------------------------------
// list_tasks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_tasks_filters_by_repository_column_and_run_state() {
    let h = TestContext::new().await;
    let repository_a = seed_repository(&h.context.pool).await;
    let repository_b = seed_repository(&h.context.pool).await;

    let a_ready = create_ready(&h, &repository_a, "a-ready", "plan").await;
    create_ready(&h, &repository_b, "b-ready", "plan").await;
    let a_not_ready = tasks::create_task(
        &h.context,
        NewTask {
            repository_id: repository_a.clone(),
            title: "a-not-ready".to_string(),
            plan: None,
            extra_instructions: None,
            column: None,
            links: vec![],
        },
    )
    .await
    .expect("create a not-ready task");

    let filtered = tasks::list_tasks(
        &h.context,
        TaskFilter {
            repository_id: Some(repository_a.clone()),
            column: Some(BoardColumn::Ready),
            run_state: None,
        },
    )
    .await
    .expect("list filtered tasks");

    assert_eq!(
        filtered.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
        vec![a_ready.id]
    );

    let all_in_repository_a = tasks::list_tasks(
        &h.context,
        TaskFilter {
            repository_id: Some(repository_a),
            column: None,
            run_state: None,
        },
    )
    .await
    .expect("list every task in one repository");

    let mut ids: Vec<_> = all_in_repository_a.iter().map(|t| t.id.clone()).collect();
    ids.sort();
    let mut expected = vec![a_not_ready.id, filtered[0].id.clone()];
    expected.sort();
    assert_eq!(ids, expected);
}

#[tokio::test]
async fn list_tasks_orders_a_column_by_position() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let first = create_ready(&h, &repository_id, "first", "plan").await;
    let second = create_ready(&h, &repository_id, "second", "plan").await;
    let third = create_ready(&h, &repository_id, "third", "plan").await;

    let listed = tasks::list_tasks(
        &h.context,
        TaskFilter {
            repository_id: Some(repository_id),
            column: Some(BoardColumn::Ready),
            run_state: None,
        },
    )
    .await
    .expect("list the column");

    assert_eq!(
        listed.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
        vec![first.id, second.id, third.id]
    );
}

// ---------------------------------------------------------------------------
// update_task — patch semantics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_task_only_changes_fields_the_patch_sets() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let created = tasks::create_task(
        &h.context,
        NewTask {
            repository_id,
            title: "original title".to_string(),
            plan: Some("original plan".to_string()),
            extra_instructions: Some("original extra".to_string()),
            column: None,
            links: vec![],
        },
    )
    .await
    .expect("create a task");
    h.changes.try_recv().expect("drain the create event");

    h.clock.advance(chrono::Duration::minutes(5));
    let updated = tasks::update_task(
        &h.context,
        &created.id,
        TaskPatch {
            title: Some("new title".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("patch only the title");

    assert_eq!(updated.title, "new title");
    assert_eq!(
        updated.plan,
        Some("original plan".to_string()),
        "unset fields must be untouched"
    );
    assert_eq!(
        updated.extra_instructions,
        Some("original extra".to_string())
    );
    assert_eq!(updated.updated_at, h.clock.now());
    assert_eq!(
        updated.created_at, created.created_at,
        "created_at never changes"
    );

    assert_eq!(
        h.changes.try_recv().expect("a publication"),
        ChangeEvent::tasks([created.id])
    );
}

#[tokio::test]
async fn update_task_clear_sets_a_nullable_field_to_none() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let created = tasks::create_task(
        &h.context,
        NewTask {
            repository_id,
            title: "task".to_string(),
            plan: None,
            extra_instructions: Some("will be cleared".to_string()),
            column: None,
            links: vec![],
        },
    )
    .await
    .expect("create a task");

    let updated = tasks::update_task(
        &h.context,
        &created.id,
        TaskPatch {
            extra_instructions: Patch::Clear,
            ..Default::default()
        },
    )
    .await
    .expect("clear extra_instructions");

    assert_eq!(updated.extra_instructions, None);
}

#[tokio::test]
async fn patching_the_plan_to_whitespace_only_clears_it_to_no_plan() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let created = tasks::create_task(
        &h.context,
        NewTask {
            repository_id,
            title: "task".to_string(),
            plan: Some("a real plan".to_string()),
            extra_instructions: None,
            column: None,
            links: vec![],
        },
    )
    .await
    .expect("create a task");

    let updated = tasks::update_task(
        &h.context,
        &created.id,
        TaskPatch {
            plan: Patch::Set("   ".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("patch the plan to whitespace only");

    assert_eq!(
        updated.plan, None,
        "NULL is the only spelling of \"no plan\" (the migration's own comment on tasks.plan)"
    );
}

#[tokio::test]
async fn update_task_cannot_blank_the_title() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let created = tasks::create_task(
        &h.context,
        NewTask {
            repository_id,
            title: "task".to_string(),
            plan: None,
            extra_instructions: None,
            column: None,
            links: vec![],
        },
    )
    .await
    .expect("create a task");

    let error = tasks::update_task(
        &h.context,
        &created.id,
        TaskPatch {
            title: Some("   ".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect_err("a blank title must be refused");

    assert_eq!(error.code(), ErrorCode::Invalid);
}

#[tokio::test]
async fn clearing_the_plan_of_a_ready_task_is_refused() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let created = create_ready(&h, &repository_id, "ready task", "has a plan").await;

    let error = tasks::update_task(
        &h.context,
        &created.id,
        TaskPatch {
            plan: Patch::Clear,
            ..Default::default()
        },
    )
    .await
    .expect_err("clearing the plan of a ready task must be refused");

    assert_eq!(error.code(), ErrorCode::Invalid);

    let unchanged = tasks::get_task(&h.context, &created.id)
        .await
        .expect("read it back");
    assert_eq!(unchanged.task.plan, Some("has a plan".to_string()));
}

// ---------------------------------------------------------------------------
// delete_task
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deleting_a_task_removes_its_links_and_outgoing_edges() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let blocker = create_ready(&h, &repository_id, "blocker", "plan").await;
    let task = tasks::create_task(
        &h.context,
        NewTask {
            repository_id,
            title: "with a link".to_string(),
            plan: None,
            extra_instructions: None,
            column: None,
            links: vec![NewTaskLink {
                label: "doc".to_string(),
                url: "https://example.com".to_string(),
            }],
        },
    )
    .await
    .expect("create a task with a link");
    seed_dependency(&h.context.pool, &task.id, &blocker.id).await;
    h.changes.try_recv().ok();

    tasks::delete_task(&h.context, &task.id)
        .await
        .expect("delete the task");

    assert!(
        tasks::get_task(&h.context, &task.id).await.is_err(),
        "the task itself must be gone"
    );
    let remaining_links: i64 = sqlx::query_scalar!(
        "SELECT count(*) FROM task_links WHERE task_id = ?1",
        task.id
    )
    .fetch_one(&h.context.pool)
    .await
    .expect("count links");
    assert_eq!(remaining_links, 0, "its links must cascade");
    let remaining_edges: i64 = sqlx::query_scalar!(
        "SELECT count(*) FROM task_dependencies WHERE task_id = ?1",
        task.id
    )
    .fetch_one(&h.context.pool)
    .await
    .expect("count outgoing edges");
    assert_eq!(remaining_edges, 0, "its outgoing edges must cascade");
    assert!(
        tasks::get_task(&h.context, &blocker.id).await.is_ok(),
        "the task it depended on must survive"
    );

    assert_eq!(
        h.changes.try_recv().expect("a publication"),
        ChangeEvent::tasks([task.id])
    );
}

#[tokio::test]
async fn deleting_a_task_other_tasks_depend_on_is_refused_and_names_them() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let blocker = create_ready(&h, &repository_id, "the blocker", "plan").await;
    let dependent = create_ready(&h, &repository_id, "the dependent", "plan").await;
    seed_dependency(&h.context.pool, &dependent.id, &blocker.id).await;

    let error = tasks::delete_task(&h.context, &blocker.id)
        .await
        .expect_err("a task with dependents must be refused");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert!(
        error.to_string().contains("the dependent"),
        "the message must name the dependent task, got: {error}"
    );
    assert!(
        tasks::get_task(&h.context, &blocker.id).await.is_ok(),
        "a refused delete must leave the task in place"
    );
}

// ---------------------------------------------------------------------------
// move_task — reordering, columns, forced rebalance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn moving_a_task_to_the_top_of_its_own_column_reorders_it() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let first = create_ready(&h, &repository_id, "first", "plan").await;
    let second = create_ready(&h, &repository_id, "second", "plan").await;
    h.changes.try_recv().ok();

    h.clock.advance(chrono::Duration::minutes(1));
    let moved = tasks::move_task(
        &h.context,
        &second.id,
        BoardColumn::Ready,
        None,
        Some(&first.id),
    )
    .await
    .expect("move second above first");

    assert!(moved.position < first.position);
    assert_eq!(moved.updated_at, h.clock.now());

    let ordered = tasks::list_tasks(
        &h.context,
        TaskFilter {
            repository_id: Some(repository_id),
            column: Some(BoardColumn::Ready),
            run_state: None,
        },
    )
    .await
    .expect("list the column");
    assert_eq!(
        ordered.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
        vec![second.id.clone(), first.id]
    );

    assert_eq!(
        h.changes.try_recv().expect("a publication"),
        ChangeEvent::tasks([second.id])
    );
}

#[tokio::test]
async fn moving_a_task_to_a_different_column_changes_its_column_and_position() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let task = create_ready(&h, &repository_id, "task", "plan").await;

    let moved = tasks::move_task(&h.context, &task.id, BoardColumn::InReview, None, None)
        .await
        .expect("move into an empty column");

    assert_eq!(moved.column, BoardColumn::InReview);
    assert_eq!(moved.position, 0.0);
}

#[tokio::test]
async fn moving_to_ready_without_a_plan_is_refused() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let task = tasks::create_task(
        &h.context,
        NewTask {
            repository_id,
            title: "no plan".to_string(),
            plan: None,
            extra_instructions: None,
            column: None,
            links: vec![],
        },
    )
    .await
    .expect("create a task with no plan");
    h.changes.try_recv().ok();

    let error = tasks::move_task(&h.context, &task.id, BoardColumn::Ready, None, None)
        .await
        .expect_err("moving to ready without a plan must be refused");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert!(
        h.changes.try_recv().is_err(),
        "a refused move must publish nothing"
    );
    let unchanged = tasks::get_task(&h.context, &task.id)
        .await
        .expect("read it back");
    assert_eq!(
        unchanged.task.column,
        BoardColumn::NotReady,
        "the column must not have moved"
    );
}

#[tokio::test]
async fn moving_to_done_is_always_allowed_even_without_a_plan() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let task = tasks::create_task(
        &h.context,
        NewTask {
            repository_id,
            title: "no plan".to_string(),
            plan: None,
            extra_instructions: None,
            column: None,
            links: vec![],
        },
    )
    .await
    .expect("create a task with no plan");

    let moved = tasks::move_task(&h.context, &task.id, BoardColumn::Done, None, None)
        .await
        .expect("moving to done must be allowed regardless of plan");

    assert_eq!(moved.column, BoardColumn::Done);
}

#[tokio::test]
async fn a_neighbour_from_a_different_column_is_refused() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let in_not_ready = tasks::create_task(
        &h.context,
        NewTask {
            repository_id: repository_id.clone(),
            title: "not ready".to_string(),
            plan: None,
            extra_instructions: None,
            column: None,
            links: vec![],
        },
    )
    .await
    .expect("create a not-ready task");
    let task = create_ready(&h, &repository_id, "task", "plan").await;

    let error = tasks::move_task(
        &h.context,
        &task.id,
        BoardColumn::Ready,
        Some(&in_not_ready.id),
        None,
    )
    .await
    .expect_err("a neighbour outside the destination column must be refused");

    assert_eq!(error.code(), ErrorCode::Invalid);
}

#[tokio::test]
async fn naming_no_neighbours_in_a_nonempty_column_is_refused() {
    let h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    // Already occupies the *destination* column — the task under test starts
    // in `ready` and is moved into `in_review`, which is where the ambiguity
    // has to be found.
    seed_task_at(
        &h.context.pool,
        &repository_id,
        "already there",
        BoardColumn::InReview,
        0.0,
    )
    .await;
    let task = create_ready(&h, &repository_id, "moving", "plan").await;

    let error = tasks::move_task(&h.context, &task.id, BoardColumn::InReview, None, None)
        .await
        .expect_err("ambiguous placement must be refused rather than guessed");

    assert_eq!(error.code(), ErrorCode::Invalid);
}

#[tokio::test]
async fn a_forced_rebalance_still_lands_the_task_between_its_neighbours() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let lower = create_ready(&h, &repository_id, "lower", "plan").await;
    h.changes.try_recv().ok();
    // Just under `MIN_POSITION_GAP` above `lower`, forcing `position_between`
    // to report `NeedsRebalance` on the very first attempt.
    seed_task_at(
        &h.context.pool,
        &repository_id,
        "upper",
        BoardColumn::Ready,
        lower.position + 5e-7,
    )
    .await;
    let upper_id = sqlx::query_scalar!("SELECT id FROM tasks WHERE title = 'upper'")
        .fetch_one(&h.context.pool)
        .await
        .expect("find the seeded upper task");
    let moving = create_ready(&h, &repository_id, "moving", "plan").await;
    h.changes.try_recv().ok();

    let moved = tasks::move_task(
        &h.context,
        &moving.id,
        BoardColumn::Ready,
        Some(&lower.id),
        Some(&upper_id),
    )
    .await
    .expect("a forced rebalance must still find room");

    let ordered = tasks::list_tasks(
        &h.context,
        TaskFilter {
            repository_id: Some(repository_id),
            column: Some(BoardColumn::Ready),
            run_state: None,
        },
    )
    .await
    .expect("list the column");
    let positions: Vec<f64> = ordered.iter().map(|t| t.position).collect();
    let mut sorted = positions.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(
        positions, sorted,
        "the column must come out strictly ordered"
    );
    assert_eq!(
        ordered.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
        vec![lower.id.clone(), moved.id.clone(), upper_id.clone()],
        "moving must land the task between the two it was asked to"
    );

    // A forced rebalance renumbers every row in the column, not just the one
    // that moved (ADR-0018's own "an id means re-read this" contract): the
    // publication must name all of them, or a subscriber that reads the id
    // list literally keeps the pre-rebalance positions of the cards it was
    // never told about.
    assert_eq!(
        h.changes.try_recv().expect("a publication"),
        ChangeEvent::tasks([moved.id, lower.id, upper_id])
    );
}

// ---------------------------------------------------------------------------
// set_run_state, through the service
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_legal_run_state_transition_is_written_and_published() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let task = create_ready(&h, &repository_id, "task", "plan").await;
    h.changes.try_recv().ok();
    assert_eq!(task.run_state, RunState::Idle);

    h.clock.advance(chrono::Duration::seconds(30));
    let updated = tasks::set_run_state(&h.context, &task.id, RunState::Queued)
        .await
        .expect("idle -> queued is legal");

    assert_eq!(updated.run_state, RunState::Queued);
    assert_eq!(updated.updated_at, h.clock.now());
    assert_eq!(
        h.changes.try_recv().expect("a publication"),
        ChangeEvent::tasks([task.id])
    );
}

#[tokio::test]
async fn an_illegal_run_state_transition_changes_nothing_and_publishes_nothing() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let task = create_ready(&h, &repository_id, "task", "plan").await;
    h.changes.try_recv().ok();

    let error = tasks::set_run_state(&h.context, &task.id, RunState::Running)
        .await
        .expect_err("idle -> running skips queued and must be refused");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert!(
        h.changes.try_recv().is_err(),
        "a refused transition must publish nothing"
    );
    let unchanged = tasks::get_task(&h.context, &task.id)
        .await
        .expect("read it back");
    assert_eq!(unchanged.task.run_state, RunState::Idle);
    assert_eq!(
        unchanged.task.updated_at, task.updated_at,
        "updated_at must not move"
    );
}

// ---------------------------------------------------------------------------
// links
// ---------------------------------------------------------------------------

#[tokio::test]
async fn adding_a_link_appends_it_and_publishes_the_owning_task() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let task = create_ready(&h, &repository_id, "task", "plan").await;
    h.changes.try_recv().ok();

    let link = tasks::add_task_link(
        &h.context,
        &task.id,
        NewTaskLink {
            label: "doc".to_string(),
            url: "https://example.com".to_string(),
        },
    )
    .await
    .expect("add a link");

    assert_eq!(link.task_id, task.id);
    assert_eq!(
        h.changes.try_recv().expect("a publication"),
        ChangeEvent::tasks([task.id])
    );
}

#[tokio::test]
async fn updating_a_link_only_changes_the_patched_field() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let task = create_ready(&h, &repository_id, "task", "plan").await;
    h.changes.try_recv().ok();
    let link = tasks::add_task_link(
        &h.context,
        &task.id,
        NewTaskLink {
            label: "original".to_string(),
            url: "https://example.com/original".to_string(),
        },
    )
    .await
    .expect("add a link");
    h.changes.try_recv().ok();

    let updated = tasks::update_task_link(
        &h.context,
        &link.id,
        TaskLinkPatch {
            label: Some("renamed".to_string()),
            url: None,
        },
    )
    .await
    .expect("patch only the label");

    assert_eq!(updated.label, "renamed");
    assert_eq!(updated.url, "https://example.com/original");
    assert_eq!(
        h.changes.try_recv().expect("a publication"),
        ChangeEvent::tasks([task.id])
    );
}

#[tokio::test]
async fn removing_a_link_deletes_it_and_publishes_the_owning_task() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let task = create_ready(&h, &repository_id, "task", "plan").await;
    let link = tasks::add_task_link(
        &h.context,
        &task.id,
        NewTaskLink {
            label: "doc".to_string(),
            url: "https://example.com".to_string(),
        },
    )
    .await
    .expect("add a link");
    h.changes.try_recv().ok();

    tasks::remove_task_link(&h.context, &link.id)
        .await
        .expect("remove the link");

    let remaining: i64 =
        sqlx::query_scalar!("SELECT count(*) FROM task_links WHERE id = ?1", link.id)
            .fetch_one(&h.context.pool)
            .await
            .expect("count links");
    assert_eq!(remaining, 0);
    assert_eq!(
        h.changes.try_recv().expect("a publication"),
        ChangeEvent::tasks([task.id])
    );
}

#[tokio::test]
async fn reordering_links_places_one_between_two_others() {
    let mut h = TestContext::new().await;
    let repository_id = seed_repository(&h.context.pool).await;
    let task = create_ready(&h, &repository_id, "task", "plan").await;
    let first = tasks::add_task_link(
        &h.context,
        &task.id,
        NewTaskLink {
            label: "first".to_string(),
            url: "https://example.com/1".to_string(),
        },
    )
    .await
    .expect("add first");
    let second = tasks::add_task_link(
        &h.context,
        &task.id,
        NewTaskLink {
            label: "second".to_string(),
            url: "https://example.com/2".to_string(),
        },
    )
    .await
    .expect("add second");
    let third = tasks::add_task_link(
        &h.context,
        &task.id,
        NewTaskLink {
            label: "third".to_string(),
            url: "https://example.com/3".to_string(),
        },
    )
    .await
    .expect("add third");
    // Drain the create and the three add-link publications; the assertion
    // below is about the reorder's own publication.
    for _ in 0..4 {
        h.changes.try_recv().ok();
    }

    tasks::reorder_task_link(&h.context, &third.id, Some(&first.id), Some(&second.id))
        .await
        .expect("move third between first and second");

    let detail = tasks::get_task(&h.context, &task.id)
        .await
        .expect("read it back");
    assert_eq!(
        detail
            .links
            .iter()
            .map(|l| l.id.clone())
            .collect::<Vec<_>>(),
        vec![first.id, third.id, second.id]
    );
    assert_eq!(
        h.changes.try_recv().expect("a publication"),
        ChangeEvent::tasks([task.id])
    );
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Creates a task already in `ready`, with a non-blank plan, so tests that
/// are not themselves about `create_task` do not have to restate its rules.
async fn create_ready(
    h: &TestContext,
    repository_id: &str,
    title: &str,
    plan: &str,
) -> rimaia_core::db::Task {
    tasks::create_task(
        &h.context,
        NewTask {
            repository_id: repository_id.to_string(),
            title: title.to_string(),
            plan: Some(plan.to_string()),
            extra_instructions: None,
            column: Some(BoardColumn::Ready),
            links: vec![],
        },
    )
    .await
    .expect("create a ready task fixture")
}

async fn seed_repository(pool: &SqlitePool) -> String {
    let id = rimaia_core::db::new_id();
    sqlx::query!(
        r#"INSERT INTO repositories (id, name, path, default_branch, worktree_root, allow_unattended_runs, created_at)
           VALUES (?1, 'rimaia', '/tmp/rimaia', 'main', '/tmp/rimaia-worktrees', 0, ?2)"#,
        id,
        NOW,
    )
    .execute(pool)
    .await
    .expect("seed a repository");
    id
}

/// A `tasks` row inserted directly at a chosen position, bypassing the
/// service's own positioning — the only way to construct the too-close-to-
/// represent gap [`a_forced_rebalance_still_lands_the_task_between_its_neighbours`]
/// needs.
async fn seed_task_at(
    pool: &SqlitePool,
    repository_id: &str,
    title: &str,
    column: BoardColumn,
    position: f64,
) {
    let id = rimaia_core::db::new_id();
    sqlx::query!(
        r#"INSERT INTO tasks (id, repository_id, title, board_column, position, run_state, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, 'idle', ?6, ?6)"#,
        id,
        repository_id,
        title,
        column,
        position,
        NOW,
    )
    .execute(pool)
    .await
    .expect("seed a task at an explicit position");
}

async fn seed_dependency(pool: &SqlitePool, task_id: &str, depends_on_task_id: &str) {
    sqlx::query!(
        "INSERT INTO task_dependencies (task_id, depends_on_task_id) VALUES (?1, ?2)",
        task_id,
        depends_on_task_id,
    )
    .execute(pool)
    .await
    .expect("seed a dependency edge");
}

/// A `runs` row task 004 does not itself create — that is task 008's job —
/// but [`get_task`](tasks::get_task) reads the table, so its own tests need
/// a way to put a row there.
async fn seed_run(pool: &SqlitePool, task_id: &str, attempt: i64, session_id: &str) -> String {
    let id = rimaia_core::db::new_id();
    sqlx::query!(
        r#"INSERT INTO runs (id, task_id, attempt, status, session_id, prompt, started_at, log_path)
           VALUES (?1, ?2, ?3, 'running', ?4, 'do the thing', ?5, ?6)"#,
        id,
        task_id,
        attempt,
        session_id,
        NOW,
        id,
    )
    .execute(pool)
    .await
    .expect("seed a run");
    id
}

/// Written the way sqlx writes a bound `DateTime<Utc>` — a numeric `+00:00`
/// offset, never `Z` (the migration's own header, and every other fixture
/// file in this suite).
const NOW: &str = "2026-08-20T00:00:00+00:00";
