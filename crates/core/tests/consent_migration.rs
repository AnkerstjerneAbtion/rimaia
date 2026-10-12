//! Task 045's migration against a real file: what an existing board becomes
//! (seam-contract D28 part 6).
//!
//! The board before it is built by the old-schema builder 040 lifted into
//! `testing::db`, at task 045's cutoff, over rows written in the schema an
//! install had before team mode, so 038's adoption runs over them exactly as
//! it does on a real upgrade.

use pretty_assertions::assert_eq;
use rimaia_core::db;
use rimaia_core::testing::db::{board_before, pre_team_mode_board, CONSENT_VERSION};
use sqlx::SqlitePool;
use tempfile::TempDir;

#[tokio::test]
async fn a_board_migrated_through_consent_attributes_everything_to_the_solo_user() {
    let dir = TempDir::new().expect("a scratch directory");
    let file = dir.path().join("rimaia.db");

    let before = pre_team_mode_board(&file).await;
    sqlx::raw_sql(OLD_BOARD)
        .execute(&before)
        .await
        .expect("an install's board");
    before.close().await;
    // 038 adopts the solo identity over those rows; 045 is not applied yet.
    board_before(&file, CONSENT_VERSION).await.close().await;

    let after = db::connect(&file).await.expect("reopen the board");
    db::migrate(&after).await.expect("the consent file applies");

    let solo: String = sqlx::query_scalar("SELECT user_id FROM solo_identity")
        .fetch_one(&after)
        .await
        .expect("the adopted user");
    let tasks: Vec<TaskAttribution> = sqlx::query_as(
        "SELECT id, created_by, plan_updated_by, assignee_id, review_instructions_updated_by,
                    plan_revision
               FROM tasks ORDER BY id",
    )
    .fetch_all(&after)
    .await
    .expect("read the tasks");
    let solo = Some(solo);
    assert_eq!(
        tasks,
        vec![
            (
                TASK_WITH_OVERRIDE.to_string(),
                solo.clone(),
                solo.clone(),
                None,
                solo.clone(),
                1
            ),
            (
                TASK_WITHOUT.to_string(),
                solo.clone(),
                solo.clone(),
                None,
                None,
                1
            ),
        ]
    );

    let settings: Vec<(String, Option<String>, i64)> =
        sqlx::query_as("SELECT key, updated_by, revision FROM team_settings ORDER BY key")
            .fetch_all(&after)
            .await
            .expect("read the team settings");
    assert!(
        !settings.is_empty(),
        "the adoption copied the team's settings"
    );
    for (key, updated_by, revision) in &settings {
        assert_eq!((updated_by, *revision), (&solo, 1), "{key}");
    }

    assert_eq!(
        dangling(&after).await,
        0,
        "pragma_foreign_key_check is empty"
    );
}

/// `(id, created_by, plan_updated_by, assignee_id,
/// review_instructions_updated_by, plan_revision)`.
type TaskAttribution = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
);

const TASK_WITH_OVERRIDE: &str = "3f2b1c00-0000-4000-8000-000000000101";
const TASK_WITHOUT: &str = "3f2b1c00-0000-4000-8000-000000000102";

const OLD_BOARD: &str = "
INSERT INTO repositories (id, name, path, default_branch, worktree_root, created_at)
VALUES ('3f2b1c00-0000-4000-8000-000000000001', 'rimaia', '/tmp/rimaia', 'main',
        '/tmp/rimaia-worktrees', '2026-08-20T02:00:00+00:00');
INSERT INTO tasks (id, repository_id, title, plan, board_column, position, run_state,
                   created_at, updated_at, review_instructions)
VALUES ('3f2b1c00-0000-4000-8000-000000000101', '3f2b1c00-0000-4000-8000-000000000001',
        'Reviewed its own way', '1. Do it', 'ready', 1.0, 'idle', '2026-08-20T02:00:00+00:00',
        '2026-08-20T02:00:00+00:00', 'Check the migrations'),
       ('3f2b1c00-0000-4000-8000-000000000102', '3f2b1c00-0000-4000-8000-000000000001',
        'Reviewed as the team does', '1. Do it too', 'ready', 2.0, 'idle',
        '2026-08-20T02:00:00+00:00', '2026-08-20T02:00:00+00:00', NULL);
INSERT INTO settings (key, value) VALUES ('strategy_default', '{\"model\":\"opus\"}');
UPDATE settings SET value = 'Open a draft PR.' WHERE key = 'base_instructions';
";

async fn dangling(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM pragma_foreign_key_check")
        .fetch_one(pool)
        .await
        .expect("check the foreign keys")
}
