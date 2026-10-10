//! The one table rebuild, against real files (seam-contract D28 parts 1, 2, 3
//! and 7, task 038).
//!
//! `20261003120000_team_mode_board.sql` is the one migration in this schema
//! that can silently delete data: D28 part 1 measured a careless rebuild
//! emptying `runs` and reporting success. So every test here runs the real
//! files against a real database, a file in a `TempDir` or the in-memory test
//! pool, and the pre-rebuild board is built by sqlx's public `Migrator` over
//! copies of the files that came before it, never by hand-written DDL that
//! could drift from what an install actually holds.

use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use rimaia_core::db;
use rimaia_core::db::settings::{
    placement, Placement, BASE_INSTRUCTIONS, DEFAULT_BASE_INSTRUCTIONS, RUNNER_KEYS, USER_KEYS,
};
use rimaia_core::identity::{self, SoloIdentity};
use rimaia_core::review_loop::config::{REVIEW_CONFIG, REVIEW_INSTRUCTIONS};
use rimaia_core::runner::process::{DISALLOWED_TOOLS, MAX_TURNS};
use rimaia_core::strategy::catalogue::STRATEGY_CATALOGUE;
use rimaia_core::strategy::settings::{
    repository_default_key, STRATEGY_APPROVAL, STRATEGY_DEFAULT,
};
use rimaia_core::testing::{test_epoch, test_pool, TestClock};
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tempfile::TempDir;
use uuid::Uuid;

/// The rebuild's version: every board file older than this is "before".
const REBUILD_VERSION: &str = "20261003120000";

const REPOSITORY: &str = "3f2b1c00-0000-4000-8000-000000000001";

// ---------------------------------------------------------------------------
// The rebuild keeps every row
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_team_mode_rebuild_keeps_every_row() {
    let dir = scratch_dir();
    let file = dir.path().join("rimaia.db");
    let before = pre_rebuild_board(&file).await;
    fill_every_table(&before).await;
    let tables = board_tables(&before).await;
    let mut snapshots = Vec::new();
    for table in &tables {
        let columns = columns_of(&before, table).await;
        let rows = rows_of(&before, table, &columns).await;
        snapshots.push((table.clone(), columns, rows));
    }
    let stored_settings: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM settings ORDER BY key")
            .fetch_all(&before)
            .await
            .expect("read the settings");
    before.close().await;

    let after = db::connect(&file).await.expect("reopen the board");
    db::migrate(&after).await.expect("the rebuild applies");

    // Every pre-existing value, column by column, and the row counts with
    // them: a row the copy lost or doubled changes the sorted list.
    for (table, columns, rows) in &snapshots {
        assert_eq!(
            &rows_of(&after, table, columns).await,
            rows,
            "{table} must hold exactly what it held before the rebuild"
        );
    }

    let solo = solo_identity(&after).await;
    assert_eq!(
        distinct(&after, "SELECT DISTINCT team_id FROM repositories").await,
        vec![solo.team_id.clone()],
        "every repository belongs to the adopted team"
    );
    assert_eq!(
        distinct(&after, "SELECT DISTINCT team_id FROM tasks").await,
        vec![solo.team_id.clone()],
        "every task belongs to the adopted team"
    );
    assert_eq!(
        distinct(&after, "SELECT DISTINCT runner_id FROM runs").await,
        vec![solo.runner_id.clone()],
        "every run names the adopted runner"
    );

    // Each key where D28 part 4 places it, driven from the same lists the
    // migration spells in SQL.
    let user_settings: Vec<(String, String, String)> =
        sqlx::query_as("SELECT user_id, key, value FROM user_settings ORDER BY key")
            .fetch_all(&after)
            .await
            .expect("read user_settings");
    let team_settings: Vec<(String, String, String)> =
        sqlx::query_as("SELECT team_id, key, value FROM team_settings ORDER BY key")
            .fetch_all(&after)
            .await
            .expect("read team_settings");
    let expected_user: Vec<(String, String, String)> = stored_settings
        .iter()
        .filter(|(key, _)| placement(key) == Placement::User)
        .map(|(key, value)| (solo.user_id.clone(), key.clone(), value.clone()))
        .collect();
    let expected_team: Vec<(String, String, String)> = stored_settings
        .iter()
        .filter(|(key, _)| placement(key) == Placement::Team)
        .map(|(key, value)| (solo.team_id.clone(), key.clone(), value.clone()))
        .collect();
    assert_eq!(user_settings, expected_user);
    assert_eq!(team_settings, expected_team);
    assert!(
        stored_settings
            .iter()
            .any(|(key, _)| placement(key) == Placement::Runner),
        "the fixture holds runner keys, so leaving them out was a choice"
    );

    let dangling: i64 = sqlx::query_scalar("SELECT count(*) FROM pragma_foreign_key_check")
        .fetch_one(&after)
        .await
        .expect("check the foreign keys");
    assert_eq!(dangling, 0);

    let indexes = distinct(
        &after,
        "SELECT name FROM sqlite_master
          WHERE type = 'index' AND tbl_name IN ('repositories', 'tasks', 'runs')
            AND name NOT LIKE 'sqlite_%'
          ORDER BY name",
    )
    .await;
    for index in [
        "idx_tasks_board",
        "idx_tasks_run_state",
        "idx_runs_task_attempt",
        "idx_runs_task_kind",
    ] {
        assert!(
            indexes.contains(&index.to_string()),
            "{index} is recreated: {indexes:?}"
        );
    }

    assert_every_connection_enforces_foreign_keys(&after).await;
    after.close().await;
}

#[tokio::test]
async fn a_fresh_board_adopts_nothing() {
    // The server's shape: `db::migrate` alone writes no team, so a hosted
    // instance does not start with a team nobody belongs to.
    let pool = test_pool().await;

    for table in ["solo_identity", "teams", "users", "runners"] {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .expect("count the rows");
        assert_eq!(rows, 0, "{table} on a fresh board");
    }
}

#[tokio::test]
async fn the_rebuild_refuses_to_run_over_a_cascade() {
    // Applied the way `cargo sqlx migrate run` would apply it: a plain
    // `Migrator::run` on a connection with enforcement on, where `DROP TABLE
    // tasks` would cascade into `runs`.
    let dir = scratch_dir();
    let file = dir.path().join("rimaia.db");
    let pool = pre_rebuild_board(&file).await;
    insert_repository_task_and_run(&pool).await;
    let every_file = migrations_dir(|_| true);
    let migrator = Migrator::new(every_file.path())
        .await
        .expect("read every board migration");

    let error = migrator
        .run(&pool)
        .await
        .expect_err("the guard refuses a rebuild with enforcement on");

    assert!(
        format!("{error:?}").contains(
            "team_mode_board needs foreign_keys OFF: apply it through rimaia_core::db::migrate"
        ),
        "{error:?}"
    );
    assert_eq!(count(&pool, "tasks").await, 1, "the task is still there");
    assert_eq!(count(&pool, "runs").await, 1, "the run is still there");
    pool.close().await;
}

#[tokio::test]
async fn a_dangling_reference_stops_the_rebuild_and_keeps_the_board() {
    // ADR-0003 counts the sqlite3 CLI as a writer, and it runs with
    // enforcement off: a task whose repository is gone is a board an install
    // can hold. The rebuild must not guess which team it belongs to.
    let dir = scratch_dir();
    let file = dir.path().join("rimaia.db");
    pre_rebuild_board(&file).await.close().await;
    let unchecked = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&file)
                .foreign_keys(false),
        )
        .await
        .expect("open the board the way the sqlite3 CLI does");
    insert_repository_task_and_run(&unchecked).await;
    sqlx::query("DELETE FROM repositories")
        .execute(&unchecked)
        .await
        .expect("strand the task");
    unchecked.close().await;

    let pool = db::connect(&file).await.expect("reopen the board");
    let error = db::migrate(&pool)
        .await
        .expect_err("a dangling reference stops the rebuild");

    assert!(
        error.to_string().contains(
            "team_mode_board found a dangling reference before it began: run PRAGMA foreign_key_check"
        ),
        "{error}"
    );
    assert_eq!(count(&pool, "tasks").await, 1, "the task is still there");
    assert_eq!(count(&pool, "runs").await, 1, "its run is still there");
    let rebuilt: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'teams'",
    )
    .fetch_one(&pool)
    .await
    .expect("read the schema");
    assert_eq!(rebuilt, 0, "the whole file rolled back");
    let recorded: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE version = ?1")
            .bind(REBUILD_VERSION.parse::<i64>().expect("a version"))
            .fetch_one(&pool)
            .await
            .expect("read the bookkeeping");
    assert_eq!(recorded, 0, "the next launch retries it");
    assert_every_connection_enforces_foreign_keys(&pool).await;
    pool.close().await;
}

// ---------------------------------------------------------------------------
// Adoption and creation agree
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ensure_solo_and_adoption_build_the_same_identity() {
    // One board adopted by the migration (it has a repository, so there is
    // something a team must own), one created at first launch.
    let dir = scratch_dir();
    let file = dir.path().join("rimaia.db");
    let before = pre_rebuild_board(&file).await;
    sqlx::query(
        "INSERT INTO repositories (id, name, path, default_branch, worktree_root, created_at)
         VALUES (?1, 'rimaia', '/tmp/rimaia', 'main', '/tmp/rimaia-worktrees',
                 '2026-08-20T02:00:00+00:00')",
    )
    .bind(REPOSITORY)
    .execute(&before)
    .await
    .expect("a repository to adopt");
    before.close().await;
    let adopted = db::connect(&file).await.expect("reopen the board");
    db::migrate(&adopted).await.expect("the rebuild adopts");

    let created = test_pool().await;
    identity::ensure_solo(&created, &TestClock::new(test_epoch()))
        .await
        .expect("a first launch creates the identity");

    let (adopted_shape, adopted_ids) = identity_shape(&adopted).await;
    let (created_shape, created_ids) = identity_shape(&created).await;

    assert_eq!(adopted_shape, created_shape);
    for id in adopted_ids.iter().chain(&created_ids) {
        let parsed = Uuid::parse_str(id).unwrap_or_else(|error| panic!("{id}: {error}"));
        assert_eq!(parsed.get_version_num(), 4, "{id} is a version-4 UUID");
        assert_eq!(parsed.get_variant(), uuid::Variant::RFC4122, "{id}");
        assert_eq!(&parsed.to_string(), id, "{id} is spelled as D10 spells ids");
    }
    adopted.close().await;
}

/// Everything about a board's identity except its ids and timestamps, with
/// each id replaced by what it is, plus the ids themselves.
async fn identity_shape(pool: &SqlitePool) -> (Vec<String>, Vec<String>) {
    let solo = solo_identity(pool).await;
    let name = |id: &str| -> String {
        if id == solo.team_id {
            "the team".to_string()
        } else if id == solo.user_id {
            "the user".to_string()
        } else if id == solo.runner_id {
            "the runner".to_string()
        } else {
            format!("a stranger {id}")
        }
    };

    let mut shape = Vec::new();
    // Every column but the id and the timestamp, as SQLite quotes it, so a
    // NULL and an empty string differ.
    let users: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, quote(identity_provider) || ' ' || quote(provider_subject) || ' '
                    || login || ' ' || quote(avatar_url)
           FROM users",
    )
    .fetch_all(pool)
    .await
    .expect("read users");
    for (id, rest) in users {
        shape.push(format!("user {}: {rest}", name(&id)));
    }
    let teams: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT id, name, personal_user_id FROM teams")
            .fetch_all(pool)
            .await
            .expect("read teams");
    for (id, team_name, personal) in teams {
        shape.push(format!(
            "team {}: {team_name}, personal to {}",
            name(&id),
            personal.as_deref().map_or("nobody".to_string(), name)
        ));
    }
    let memberships: Vec<(String, String, String)> =
        sqlx::query_as("SELECT team_id, user_id, role FROM team_memberships")
            .fetch_all(pool)
            .await
            .expect("read memberships");
    for (team, user, role) in memberships {
        shape.push(format!("{} is {role} of {}", name(&user), name(&team)));
    }
    let runners: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id, user_id, label || ' ' || provider || ' ' || quote(app_version) || ' '
                             || quote(last_seen_at) || ' ' || quote(unpaired_at)
           FROM runners",
    )
    .fetch_all(pool)
    .await
    .expect("read runners");
    for (id, user, rest) in runners {
        shape.push(format!("runner {} of {}: {rest}", name(&id), name(&user)));
    }
    let team_settings: Vec<(String, String, String)> =
        sqlx::query_as("SELECT team_id, key, value FROM team_settings ORDER BY key")
            .fetch_all(pool)
            .await
            .expect("read team_settings");
    for (team, key, value) in team_settings {
        shape.push(format!("{} sets {key} = {value:?}", name(&team)));
    }
    let user_settings: i64 = sqlx::query_scalar("SELECT count(*) FROM user_settings")
        .fetch_one(pool)
        .await
        .expect("count user_settings");
    shape.push(format!("{user_settings} user settings"));

    assert!(
        shape.contains(&format!(
            "the team sets {BASE_INSTRUCTIONS} = {DEFAULT_BASE_INSTRUCTIONS:?}"
        )),
        "{shape:#?}"
    );
    (shape, vec![solo.team_id, solo.user_id, solo.runner_id])
}

// ---------------------------------------------------------------------------
// How migrations are applied
// ---------------------------------------------------------------------------

#[test]
fn no_migration_opts_out_of_its_transaction() {
    // sqlx 0.8.6 ignores the marker on SQLite, so a file that carried it would
    // still run in a transaction today and silently stop doing so the day a
    // driver honours it (D28 part 1). Read off what the binary embedded.
    let embedded: Vec<_> = db::migrations().collect();

    assert!(!embedded.is_empty());
    for migration in embedded {
        assert!(
            !migration.no_tx,
            "{} {} opts out of its transaction",
            migration.version, migration.description
        );
    }
}

#[tokio::test]
async fn a_failed_migration_leaves_no_connection_with_enforcement_off() {
    let dir = scratch_dir();
    std::fs::write(
        dir.path().join("20990101000000_a_table.sql"),
        "-- A table.\nCREATE TABLE a_table (id INTEGER PRIMARY KEY);\n",
    )
    .expect("write a migration");
    std::fs::write(
        dir.path().join("20990101000100_broken.sql"),
        "-- Broken.\nINSERT INTO no_such_table (id) VALUES (1);\n",
    )
    .expect("write a migration that fails");
    let migrator = Migrator::new(dir.path())
        .await
        .expect("read the migrations");
    // Built as `test_pool` builds its own, but left unmigrated.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(":memory:")
                .foreign_keys(true),
        )
        .await
        .expect("an in-memory database");

    db::apply_migrations(&migrator, &pool)
        .await
        .expect_err("the second file fails");

    let enforced: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&pool)
        .await
        .expect("read the pragma");
    assert_eq!(enforced, 1);
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn scratch_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("rimaia-team-mode-")
        .tempdir()
        .expect("a scratch directory")
}

/// The board's migrations, as `crates/core/build.rs` watches them.
fn board_migrations() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/migrations")
}

/// A directory holding copies of the board migrations `keep` accepts.
fn migrations_dir(keep: impl Fn(&str) -> bool) -> TempDir {
    let dir = scratch_dir();
    for entry in std::fs::read_dir(board_migrations()).expect("read the migrations") {
        let path = entry.expect("a directory entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a UTF-8 file name")
            .to_string();
        if name.ends_with(".sql") && keep(&name) {
            std::fs::copy(&path, dir.path().join(&name)).expect("copy a migration");
        }
    }
    dir
}

/// A file board at the schema every install had before the rebuild: every
/// board migration older than it, applied through sqlx's public API.
async fn pre_rebuild_board(file: &Path) -> SqlitePool {
    let older = migrations_dir(|name| name < REBUILD_VERSION);
    let migrator = Migrator::new(older.path())
        .await
        .expect("read the pre-rebuild migrations");
    let pool = db::connect(file).await.expect("open the board");
    migrator
        .run(&pool)
        .await
        .expect("the pre-rebuild migrations apply");
    pool
}

async fn insert_repository_task_and_run(pool: &SqlitePool) {
    sqlx::raw_sql(
        "INSERT INTO repositories (id, name, path, default_branch, worktree_root, created_at)
         VALUES ('3f2b1c00-0000-4000-8000-000000000001', 'rimaia', '/tmp/rimaia', 'main',
                 '/tmp/rimaia-worktrees', '2026-08-20T02:00:00+00:00');
         INSERT INTO tasks (id, repository_id, title, board_column, position, run_state,
                            created_at, updated_at)
         VALUES ('3f2b1c00-0000-4000-8000-000000000101', '3f2b1c00-0000-4000-8000-000000000001',
                 'A task', 'ready', 1.0, 'idle', '2026-08-20T02:00:00+00:00',
                 '2026-08-20T02:00:00+00:00');
         INSERT INTO runs (id, task_id, attempt, status, session_id, prompt, started_at, log_path)
         VALUES ('3f2b1c00-0000-4000-8000-000000000201', '3f2b1c00-0000-4000-8000-000000000101',
                 1, 'succeeded', 'session', 'prompt', '2026-08-20T02:00:00+00:00',
                 '/tmp/run.jsonl');",
    )
    .execute(pool)
    .await
    .expect("a repository, a task and a run");
}

/// D28 part 7's fixture: every table, every nullable column set somewhere,
/// tasks in all four columns and one archived, and every settings key part 4
/// places, a per-repository strategy default among them.
async fn fill_every_table(pool: &SqlitePool) {
    sqlx::raw_sql(FILL)
        .execute(pool)
        .await
        .expect("fill every table");

    let per_repository = repository_default_key(REPOSITORY);
    let team_keys = [
        STRATEGY_CATALOGUE,
        STRATEGY_DEFAULT,
        per_repository.as_str(),
        STRATEGY_APPROVAL,
        MAX_TURNS,
        DISALLOWED_TOOLS,
        REVIEW_INSTRUCTIONS,
        REVIEW_CONFIG,
    ];
    for key in RUNNER_KEYS.iter().chain(&USER_KEYS).chain(&team_keys) {
        sqlx::query("INSERT INTO settings (key, value) VALUES (?1, ?2)")
            .bind(key)
            .bind(format!("what {key} held"))
            .execute(pool)
            .await
            .expect("store a setting");
    }
    sqlx::query("UPDATE settings SET value = ?2 WHERE key = ?1")
        .bind(BASE_INSTRUCTIONS)
        .bind("Open a draft PR, never a ready one.")
        .execute(pool)
        .await
        .expect("edit the seeded instructions");
}

const FILL: &str = "
INSERT INTO repositories (id, name, path, default_branch, worktree_root, allow_unattended_runs,
                          created_at, max_concurrency, credential_login, credential_label,
                          credential_added_at, on_archive, on_archive_script, review_config)
VALUES ('3f2b1c00-0000-4000-8000-000000000001', 'rimaia', '/Users/someone/Code/My Projects/rimaia',
        'main', '/Users/someone/worktrees', 1, '2026-08-20T02:00:00.123456+00:00', 3, 'octocat',
        'Work token', '2026-08-21T09:00:00+00:00', 'script', '/Users/someone/bin/teardown.sh',
        '{\"max_review_loops\":2}');

INSERT INTO tasks (id, repository_id, title, plan, extra_instructions, board_column, position,
                   run_state, branch, worktree_path, strategy_mode, model, effort, strategy_plan,
                   strategy_source, strategy_updated_at, created_at, updated_at, source,
                   archived_at, review_instructions, review_config)
VALUES
  ('3f2b1c00-0000-4000-8000-000000000101', '3f2b1c00-0000-4000-8000-000000000001',
   'Captured', NULL, NULL, 'not_ready', 1.0, 'idle', NULL, NULL, 'default', NULL, NULL, NULL,
   NULL, NULL, '2026-08-20T02:00:00+00:00', '2026-08-20T02:00:00+00:00', 'ui', NULL, NULL, NULL),
  ('3f2b1c00-0000-4000-8000-000000000102', '3f2b1c00-0000-4000-8000-000000000001',
   'Queued tonight', '1. Do the work', 'Keep the diff small', 'ready', 1.5, 'waiting_retry',
   'rimaia/queued', '/Users/someone/worktrees/queued', 'planned', 'opus', 'high',
   '{\"phases\":[]}', 'planner', '2026-08-20T03:00:00+00:00', '2026-08-20T02:00:00+00:00',
   '2026-08-20T04:00:00+00:00', 'mcp', NULL, 'Check the migrations', '{\"severity\":\"high\"}'),
  ('3f2b1c00-0000-4000-8000-000000000103', '3f2b1c00-0000-4000-8000-000000000001',
   'Awaiting review', '1. Review me', NULL, 'in_review', 2.0, 'idle', 'rimaia/review', NULL,
   'manual', 'sonnet', NULL, NULL, 'user', '2026-08-20T05:00:00+00:00',
   '2026-08-20T02:00:00+00:00', '2026-08-20T05:00:00+00:00', 'system', NULL, NULL, NULL),
  ('3f2b1c00-0000-4000-8000-000000000104', '3f2b1c00-0000-4000-8000-000000000001',
   'Shipped', '1. Ship it', NULL, 'done', 3.0, 'idle', NULL, NULL, 'default', NULL, NULL, NULL,
   NULL, NULL, '2026-08-20T02:00:00+00:00', '2026-08-20T06:00:00+00:00', 'ui', NULL, NULL, NULL),
  ('3f2b1c00-0000-4000-8000-000000000105', '3f2b1c00-0000-4000-8000-000000000001',
   'Archived', '1. Gone', NULL, 'done', 4.0, 'cancelled', NULL, NULL, 'default', NULL, NULL,
   NULL, NULL, NULL, '2026-08-20T02:00:00+00:00', '2026-08-20T07:00:00+00:00', 'ui',
   '2026-08-20T07:00:00+00:00', NULL, NULL);

INSERT INTO task_dependencies (task_id, depends_on_task_id)
VALUES ('3f2b1c00-0000-4000-8000-000000000102', '3f2b1c00-0000-4000-8000-000000000103');

INSERT INTO task_links (id, task_id, label, url, position)
VALUES ('3f2b1c00-0000-4000-8000-000000000301', '3f2b1c00-0000-4000-8000-000000000102',
        'Spec', 'https://example.com/spec', 1.0),
       ('3f2b1c00-0000-4000-8000-000000000302', '3f2b1c00-0000-4000-8000-000000000102',
        'Thread', 'https://example.com/thread', 2.0);

INSERT INTO runs (id, task_id, attempt, status, session_id, prompt, started_at, ended_at,
                  exit_class, error_message, num_turns, cost_usd, log_path, pr_url, resume_after,
                  base_ref, model, effort, run_environment, input_tokens, output_tokens,
                  cache_read_tokens, cache_creation_tokens, head_sha, base_sha, kind,
                  findings_recorded_at)
VALUES
  ('3f2b1c00-0000-4000-8000-000000000201', '3f2b1c00-0000-4000-8000-000000000103', 1,
   'succeeded', 'session-1', 'Do the work', '2026-08-20T02:00:00+00:00',
   '2026-08-20T02:30:00+00:00', 'success', NULL, 12, 1.25,
   '/Users/someone/runs/201.jsonl', 'https://github.com/o/r/pull/7', NULL, 'main', 'opus',
   'high', 'strict_local', 1000, 2000, 3000, 4000, 'abc123', 'def456', 'implementation', NULL),
  ('3f2b1c00-0000-4000-8000-000000000202', '3f2b1c00-0000-4000-8000-000000000103', 2,
   'succeeded', 'session-2', 'Review it', '2026-08-20T02:31:00+00:00',
   '2026-08-20T02:40:00+00:00', 'success', NULL, 3, 0.25, '/Users/someone/runs/202.jsonl',
   NULL, NULL, 'main', 'sonnet', NULL, 'inherit', 10, 20, 30, 40, 'abc123', 'def456', 'review',
   '2026-08-20T02:39:00+00:00'),
  ('3f2b1c00-0000-4000-8000-000000000203', '3f2b1c00-0000-4000-8000-000000000102', 1,
   'failed', 'session-3', 'Fix it', '2026-08-20T03:00:00+00:00', '2026-08-20T03:05:00+00:00',
   'usage_limit', 'Out of usage', 1, 0.01, '/Users/someone/runs/203.jsonl', NULL,
   '2026-08-20T08:00:00+00:00', 'main', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
   'fix', NULL);

INSERT INTO review_bundles (run_id, files_changed, insertions, deletions, files, commits, patch,
                            patch_bytes, patch_truncated, patch_pruned_at, created_at)
VALUES ('3f2b1c00-0000-4000-8000-000000000201', 2, 10, 3, '[]', '[]', 'diff --git a b', 14, 1,
        '2026-08-25T00:00:00+00:00', '2026-08-20T02:30:00+00:00');

INSERT INTO review_findings (id, task_id, review_run_id, ordinal, severity, title, body, file,
                             line, fingerprint, status, resolution, resolved_by_run_id,
                             created_at, resolved_at)
VALUES ('3f2b1c00-0000-4000-8000-000000000401', '3f2b1c00-0000-4000-8000-000000000103',
        '3f2b1c00-0000-4000-8000-000000000202', 0, 'high', 'Unchecked length', 'It panics.',
        'src/lib.rs', 42, 'fp-1', 'fixed', 'Checked it.', '3f2b1c00-0000-4000-8000-000000000201',
        '2026-08-20T02:39:00+00:00', '2026-08-20T02:45:00+00:00'),
       ('3f2b1c00-0000-4000-8000-000000000402', '3f2b1c00-0000-4000-8000-000000000103',
        '3f2b1c00-0000-4000-8000-000000000202', 1, 'low', 'Naming', 'Rename it.', NULL, NULL,
        NULL, 'open', NULL, NULL, '2026-08-20T02:39:00+00:00', NULL);

INSERT INTO schedules (id, name, mode, cron, start_at, max_concurrency, enabled, timezone,
                       stop_at, last_fired_at, armed_at)
VALUES ('3f2b1c00-0000-4000-8000-000000000501', 'Nightly', 'parallel', '0 22 * * *',
        '2026-08-20T22:00:00+00:00', 3, 0, 'Europe/Copenhagen', '06:00',
        '2026-08-19T22:00:00+00:00', '2026-08-18T10:00:00+00:00');
";

/// Every table the board had, by name.
async fn board_tables(pool: &SqlitePool) -> Vec<String> {
    distinct(
        pool,
        "SELECT name FROM sqlite_master
          WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations'
          ORDER BY name",
    )
    .await
}

async fn columns_of(pool: &SqlitePool, table: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
        .bind(table)
        .fetch_all(pool)
        .await
        .expect("read the columns")
}

/// Each row as the literal SQLite would write for each of `columns`, so a
/// value that changed type (an integer becoming text, say) is a change too.
async fn rows_of(pool: &SqlitePool, table: &str, columns: &[String]) -> Vec<String> {
    let select = columns
        .iter()
        .map(|column| format!("'{column}=' || quote(\"{column}\")"))
        .collect::<Vec<_>>()
        .join(" || ' ' || ");
    let mut rows: Vec<String> = sqlx::query_scalar(&format!("SELECT {select} FROM \"{table}\""))
        .fetch_all(pool)
        .await
        .unwrap_or_else(|error| panic!("read {table}: {error}"));
    rows.sort();
    rows
}

async fn distinct(pool: &SqlitePool, query: &str) -> Vec<String> {
    sqlx::query_scalar(query)
        .fetch_all(pool)
        .await
        .unwrap_or_else(|error| panic!("{query}: {error}"))
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .expect("count the rows")
}

async fn solo_identity(pool: &SqlitePool) -> SoloIdentity {
    let (team_id, user_id, runner_id, created_at): (String, String, String, String) =
        sqlx::query_as("SELECT team_id, user_id, runner_id, created_at FROM solo_identity")
            .fetch_one(pool)
            .await
            .expect("the board has a solo identity");
    SoloIdentity {
        team_id,
        user_id,
        runner_id,
        created_at: created_at.parse().expect("an RFC 3339 timestamp"),
    }
}

/// Several connections held at once, so the one the migrator used is among
/// them along with fresh ones.
async fn assert_every_connection_enforces_foreign_keys(pool: &SqlitePool) {
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(pool.acquire().await.expect("a connection"));
    }
    for conn in &mut held {
        let enforced: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mut **conn)
            .await
            .expect("read the pragma");
        assert_eq!(enforced, 1);
    }
}
