//! [`rebalance_column`] against a real, migrated database.
//!
//! The pure half of fractional ordering — [`position_between`] and
//! [`rebalanced_positions`] — has its own colocated unit tests in
//! `crates/core/src/tasks/position.rs`; nothing here re-proves that
//! arithmetic. What only a database can prove is the SQL: that a renumber
//! reads and writes the rows it claims to, in the order it claims to, and
//! that the round trip through SQLite leaves `position_between` able to find
//! room again. That last property is task 002's own acceptance criterion —
//! "`position_between` including the rebalance path" — end to end.

use rimaia_core::db::{BoardColumn, RunState};
use rimaia_core::tasks::{position_between, rebalance_column, Placement};
use rimaia_core::testing::test_pool;
use sqlx::SqlitePool;

#[tokio::test]
async fn rebalancing_preserves_the_columns_existing_order() {
    let pool = test_pool().await;
    let repository_id = seed_repository(&pool).await;
    // Inserted out of position order, so a bug that renumbered by insertion
    // order rather than by `position` would still pass by accident if this
    // matched them.
    seed_task(&pool, "task-c", &repository_id, BoardColumn::Ready, 5.0, T1).await;
    seed_task(&pool, "task-a", &repository_id, BoardColumn::Ready, 1.0, T1).await;
    seed_task(&pool, "task-b", &repository_id, BoardColumn::Ready, 3.0, T1).await;

    let updated_at_before = updated_at_by_id(&pool, &repository_id).await;

    let touched = rebalance(&pool, &repository_id, BoardColumn::Ready).await;

    assert_eq!(touched, 3);
    assert_eq!(
        ordered_positions(&pool, &repository_id, BoardColumn::Ready).await,
        vec![
            ("task-a".to_string(), 0.0),
            ("task-b".to_string(), 1.0),
            ("task-c".to_string(), 2.0),
        ]
    );
    // The promise `rebalance_column`'s doc makes, asserted rather than trusted.
    // Task 004 owns `updated_at` and edits this function's caller; adding
    // `updated_at = ?` to the renumber's UPDATE would look like a consistency
    // fix and would break nothing else. Renumbering is not a change to any
    // card, and stamping a whole column modified makes every client refresh for
    // nothing.
    assert_eq!(
        updated_at_by_id(&pool, &repository_id).await,
        updated_at_before,
        "a renumber must not stamp updated_at"
    );
}

#[tokio::test]
async fn a_whole_second_sorts_before_the_same_second_with_a_fraction() {
    // The `created_at` tie-break is a *string* comparison — the column is TEXT
    // and `ORDER BY position, created_at, id` sorts it lexicographically. In
    // sqlx's spelling that agrees with chronological order, because '+' (0x2B)
    // precedes '.' (0x2E). It would not in the 'Z' spelling: 'Z' is 0x5A, so
    // '...T00:00:00Z' sorts *after* '...T00:00:00.500+00:00' and this pair
    // would come out reversed. Board order is execution order (ADR-0007), so
    // the property is pinned here rather than assumed from the format.
    let pool = test_pool().await;
    let repository_id = seed_repository(&pool).await;

    // Both at the same position, and seeded later-first so a renumber that
    // ignored `created_at` and fell through to insertion order — or to `id`,
    // which sorts 'h' before 'o' — would fail this rather than pass it.
    seed_task(
        &pool,
        "half-a-second-later",
        &repository_id,
        BoardColumn::Ready,
        1.0,
        T1_AND_A_HALF,
    )
    .await;
    seed_task(
        &pool,
        "on-the-second",
        &repository_id,
        BoardColumn::Ready,
        1.0,
        T1,
    )
    .await;

    rebalance(&pool, &repository_id, BoardColumn::Ready).await;

    assert_eq!(
        ordered_positions(&pool, &repository_id, BoardColumn::Ready).await,
        vec![
            ("on-the-second".to_string(), 0.0),
            ("half-a-second-later".to_string(), 1.0),
        ]
    );
}

#[tokio::test]
async fn rebalancing_touches_only_the_repository_and_column_asked_about() {
    let pool = test_pool().await;
    let target_repository = seed_repository(&pool).await;
    let other_repository = seed_repository(&pool).await;

    seed_task(
        &pool,
        "in-target",
        &target_repository,
        BoardColumn::Ready,
        5.0,
        T1,
    )
    .await;
    seed_task(
        &pool,
        "in-target-2",
        &target_repository,
        BoardColumn::Ready,
        100.0,
        T1,
    )
    .await;
    // Same repository, a different column: must be left exactly as it was.
    seed_task(
        &pool,
        "other-column",
        &target_repository,
        BoardColumn::NotReady,
        42.0,
        T1,
    )
    .await;
    // Same column name, a different repository: must also be left alone.
    seed_task(
        &pool,
        "other-repo",
        &other_repository,
        BoardColumn::Ready,
        7.0,
        T1,
    )
    .await;

    rebalance(&pool, &target_repository, BoardColumn::Ready).await;

    assert_eq!(
        ordered_positions(&pool, &target_repository, BoardColumn::Ready).await,
        vec![
            ("in-target".to_string(), 0.0),
            ("in-target-2".to_string(), 1.0),
        ]
    );
    assert_eq!(
        ordered_positions(&pool, &target_repository, BoardColumn::NotReady).await,
        vec![("other-column".to_string(), 42.0)],
        "a sibling column in the same repository must not be touched"
    );
    assert_eq!(
        ordered_positions(&pool, &other_repository, BoardColumn::Ready).await,
        vec![("other-repo".to_string(), 7.0)],
        "a same-named column in a different repository must not be touched"
    );
}

#[tokio::test]
async fn a_column_whose_cards_share_a_position_comes_out_strictly_ordered() {
    let pool = test_pool().await;
    let repository_id = seed_repository(&pool).await;

    // All four share `position`, the degenerate case a rebalance repairs.
    // "c2" and "c1" are ordered by `created_at` alone; "a-tie" and "b-tie"
    // additionally share `created_at`, so only `id` can separate them. Ids are
    // chosen so that alphabetical order and creation order agree for the
    // first pair and would disagree if `id` were consulted before
    // `created_at` — proving the tie-break order is the one documented, not
    // its reverse.
    seed_task(&pool, "c1", &repository_id, BoardColumn::Ready, 1.0, T2).await;
    seed_task(&pool, "c2", &repository_id, BoardColumn::Ready, 1.0, T1).await;
    seed_task(&pool, "b-tie", &repository_id, BoardColumn::Ready, 1.0, T3).await;
    seed_task(&pool, "a-tie", &repository_id, BoardColumn::Ready, 1.0, T3).await;

    rebalance(&pool, &repository_id, BoardColumn::Ready).await;

    assert_eq!(
        ordered_positions(&pool, &repository_id, BoardColumn::Ready).await,
        vec![
            ("c2".to_string(), 0.0),
            ("c1".to_string(), 1.0),
            ("a-tie".to_string(), 2.0),
            ("b-tie".to_string(), 3.0),
        ]
    );
}

#[tokio::test]
async fn forcing_a_rebalance_leaves_position_between_able_to_find_room_again() {
    let pool = test_pool().await;
    let repository_id = seed_repository(&pool).await;
    seed_task(&pool, "lower", &repository_id, BoardColumn::Ready, 1.0, T1).await;
    // Just under `MIN_POSITION_GAP` above its neighbour.
    seed_task(
        &pool,
        "upper",
        &repository_id,
        BoardColumn::Ready,
        1.0 + 5e-7,
        T1,
    )
    .await;

    let before = ordered_positions(&pool, &repository_id, BoardColumn::Ready).await;
    assert_eq!(
        position_between(Some(before[0].1), Some(before[1].1)),
        Placement::NeedsRebalance,
        "the fixture must actually need a rebalance, or this test proves nothing"
    );

    rebalance(&pool, &repository_id, BoardColumn::Ready).await;

    let after = ordered_positions(&pool, &repository_id, BoardColumn::Ready).await;
    assert_eq!(
        position_between(Some(after[0].1), Some(after[1].1)),
        Placement::At(0.5),
        "a rebalanced pair of neighbours must have room for a drop between them"
    );
}

// Every fixture timestamp below is written the way sqlx writes a bound
// `DateTime<Utc>`: a numeric `+00:00` offset, never `Z` (sqlx-sqlite 0.8.6,
// `src/types/chrono.rs:69`, and the migration's header). That is not cosmetic
// here — `created_at` is TEXT and the tie-break sorts it as a string, so a
// fixture in the other spelling would exercise an ordering production never
// produces. `T1_AND_A_HALF` is deliberately sub-second: the two widths have to
// interleave correctly, and that is what
// `a_whole_second_sorts_before_the_same_second_with_a_fraction` pins.
const T1: &str = "2026-08-20T00:00:00+00:00";
const T1_AND_A_HALF: &str = "2026-08-20T00:00:00.500+00:00";
const T2: &str = "2026-08-20T00:01:00+00:00";
const T3: &str = "2026-08-20T00:02:00+00:00";

/// Runs the renumber inside a transaction and commits it, because that is the
/// obligation [`rebalance_column`]'s own doc comment places on every caller —
/// and this is the only worked example of that call in the repository, so it
/// is what the next caller copies. In autocommit a mid-loop failure leaves the
/// column strictly worse ordered than it started; the doc comment spells that
/// out.
///
/// The transaction is committed and dropped before returning, so the caller's
/// subsequent pool queries never contend with it — [`test_pool`] caps the pool
/// at one connection, and holding this one open would deadlock the next query
/// rather than fail it.
async fn rebalance(pool: &SqlitePool, repository_id: &str, column: BoardColumn) -> usize {
    let mut tx = pool
        .begin()
        .await
        .expect("the sole test connection must still be available");
    let touched = rebalance_column(&mut tx, repository_id, column)
        .await
        .expect("rebalance_column must succeed against a migrated schema");
    tx.commit().await.expect("commit the renumber");
    touched
}

/// Every task in one `(repository, column)`, in board order, as `(id,
/// position)` — the shape every assertion above compares against.
async fn ordered_positions(
    pool: &SqlitePool,
    repository_id: &str,
    column: BoardColumn,
) -> Vec<(String, f64)> {
    sqlx::query!(
        r#"
        SELECT id, position
        FROM tasks
        WHERE repository_id = ?1 AND board_column = ?2
        ORDER BY position ASC, created_at ASC, id ASC
        "#,
        repository_id,
        column,
    )
    .fetch_all(pool)
    .await
    .expect("read back the column")
    .into_iter()
    .map(|row| (row.id, row.position))
    .collect()
}

/// Every task of one repository as `(id, updated_at)`, keyed by id rather than
/// by board order so the comparison survives the very renumber it is checking.
/// The raw stored text, not a decoded `DateTime`, so a rewrite that happened to
/// land on the same instant in a different spelling would still show up.
async fn updated_at_by_id(pool: &SqlitePool, repository_id: &str) -> Vec<(String, String)> {
    sqlx::query!(
        "SELECT id, updated_at FROM tasks WHERE repository_id = ?1 ORDER BY id ASC",
        repository_id,
    )
    .fetch_all(pool)
    .await
    .expect("read back the column's updated_at")
    .into_iter()
    .map(|row| (row.id, row.updated_at))
    .collect()
}

/// A minimal `repositories` row; nothing in this suite reads its fields, only
/// its id as the foreign key `tasks.repository_id` requires.
async fn seed_repository(pool: &SqlitePool) -> String {
    let id = rimaia_core::db::new_id();
    sqlx::query!(
        r#"
        INSERT INTO repositories (id, name, path, default_branch, worktree_root, allow_unattended_runs, created_at)
        VALUES (?1, 'rimaia', '/tmp/rimaia', 'main', '/tmp/rimaia-worktrees', 0, ?2)
        "#,
        id,
        T1,
    )
    .execute(pool)
    .await
    .expect("seed a repository");
    id
}

/// A minimal `tasks` row with an explicit id, so tie-break assertions can
/// name an expected order instead of matching against a random UUID.
#[allow(clippy::too_many_arguments)]
async fn seed_task(
    pool: &SqlitePool,
    id: &str,
    repository_id: &str,
    column: BoardColumn,
    position: f64,
    created_at: &str,
) {
    sqlx::query!(
        r#"
        INSERT INTO tasks (
            id, repository_id, title, board_column, position, run_state, created_at, updated_at
        )
        VALUES (?1, ?2, 'a seeded task', ?3, ?4, ?5, ?6, ?7)
        "#,
        id,
        repository_id,
        column,
        position,
        RunState::Idle,
        created_at,
        created_at,
    )
    .execute(pool)
    .await
    .expect("seed a task");
}
