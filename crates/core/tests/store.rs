//! The store, against a real migrated schema (ADR-0003, task 002's acceptance criteria).
//!
//! Four things prove those criteria: that a fresh launch gets the whole schema and a
//! second launch changes nothing, that every table round-trips real values rather than
//! merely accepting an `INSERT`, that every foreign key and `CHECK` this migration
//! declares actually rejects what it claims to, and that the two asymmetric delete rules
//! ADR-0008 requires — cascade on the dependent, restrict on what is depended upon — hold
//! against a real database. `position_between` and its rebalance path have their own
//! suite in `tests/position.rs`; nothing here repeats that arithmetic. Startup
//! reconciliation (`survey`) is colocated with the code it tests in `src/startup.rs`, for
//! the same reason: it is not this file's to duplicate.
//!
//! Every insert here is a plain `sqlx::query!` against the real table, never through a
//! service — task 002 ships storage and models, not a CRUD layer, so there is nothing
//! else yet to call.

use chrono::{DateTime, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::db::{
    self, new_id, BoardColumn, ExitClass, MutationSource, Repository, Run, RunState, RunStatus,
    Schedule, ScheduleMode, Setting, StrategyMode, StrategySource, Task, TaskDependency, TaskLink,
};
use rimaia_core::testing::test_pool;
use serde::Serialize;
use sqlx::SqlitePool;

/// A fixed instant used everywhere a test needs *a* timestamp but the value itself is
/// not under test. Round-trip precision has its own tests, below, with timestamps chosen
/// to make that the point.
///
/// Spelled with a numeric offset rather than `Z` because that is what sqlx writes for a
/// bound `DateTime<Utc>`, and the two spellings do not sort together — see the migration's
/// header and
/// [`a_bound_timestamp_is_stored_with_a_numeric_offset_rather_than_z`]. A fixture in the
/// spelling production never produces is a fixture that proves less than it looks like.
const NOW: &str = "2026-08-20T12:00:00+00:00";

// ---------------------------------------------------------------------------------
// Launch behaviour
// ---------------------------------------------------------------------------------

#[tokio::test]
async fn a_fresh_database_gets_every_table_the_schema_declares() {
    let pool = test_pool().await;

    let tables: Vec<String> = sqlx::query_scalar!(
        r#"SELECT name AS "name!" FROM sqlite_master
           WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != '_sqlx_migrations'
           ORDER BY name"#
    )
    .fetch_all(&pool)
    .await
    .expect("read sqlite_master");

    // Read off ADR-0003's table list, not off the migration file, so this fails the way
    // an acceptance criterion should: by naming what is missing or extra, not by echoing
    // the SQL back at itself.
    assert_eq!(
        tables,
        vec![
            "repositories",
            "runs",
            "schedules",
            "settings",
            "task_dependencies",
            "task_links",
            "tasks",
        ]
    );
}

#[tokio::test]
async fn opening_an_already_migrated_database_applies_no_further_migrations() {
    // A real file on disk, reopened, because "second launch" is literally a second
    // process against the same database — `db::mod`'s own unit test makes the same
    // point about `_sqlx_migrations`; this is that criterion again, from the
    // integration side, alongside every other proof in this file.
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("rimaia.db");

    let first = db::connect(&file).await.expect("first launch connects");
    db::migrate(&first)
        .await
        .expect("a fresh database migrates");
    let applied_first = applied_migration_versions(&first).await;
    first.close().await;

    let second = db::connect(&file).await.expect("second launch connects");
    db::migrate(&second)
        .await
        .expect("re-migrating an already-migrated database must not error");
    let applied_second = applied_migration_versions(&second).await;
    second.close().await;

    assert!(
        !applied_first.is_empty(),
        "the first launch must have applied at least one migration"
    );
    assert_eq!(
        applied_first, applied_second,
        "a second launch must apply nothing new"
    );
}

#[tokio::test]
async fn test_pool_and_a_real_migrated_file_produce_the_same_schema() {
    // The harness's whole promise (`testing/db.rs`'s own doc comment): a test can never
    // pass against a schema production does not have. This is what makes that true
    // rather than assumed — `test_pool()` runs `db::migrate` in memory, this runs it
    // against a real file, and the two `sqlite_master`s must be identical, index for
    // index, down to the `sql` SQLite would replay to recreate each object.
    let in_memory = test_pool().await;

    let dir = tempfile::tempdir().expect("temp dir");
    let file_pool = db::connect(&dir.path().join("rimaia.db"))
        .await
        .expect("connect to a real file");
    db::migrate(&file_pool)
        .await
        .expect("migrate the real file");

    let in_memory_schema = schema_snapshot(&in_memory).await;
    let file_schema = schema_snapshot(&file_pool).await;
    file_pool.close().await;

    assert_eq!(
        in_memory_schema, file_schema,
        "an in-memory test database and a real migrated file declared different schemas"
    );
}

// ---------------------------------------------------------------------------------
// Round-trip per table
// ---------------------------------------------------------------------------------

#[tokio::test]
async fn a_repository_round_trips_every_field_exactly() {
    let pool = test_pool().await;
    let id = new_id();
    // Sub-second precision, so this test also stands for "a timestamp written as UTC
    // reads back to the same instant" rather than merely to the same second.
    let created_at: DateTime<Utc> = "2026-08-20T13:45:07.123456Z".parse().expect("rfc3339");

    sqlx::query!(
        "INSERT INTO repositories
            (id, name, path, default_branch, worktree_root, allow_unattended_runs, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        id,
        "rimaia",
        "/Users/someone/Code/My Projects/rimaia",
        "main",
        "/Users/someone/Library/Application Support/com.rimaia.app/worktrees",
        true,
        created_at,
    )
    .execute(&pool)
    .await
    .expect("insert a repository");

    let repository = fetch_repository(&pool, &id).await;

    assert_eq!(
        repository,
        Repository {
            id: id.clone(),
            name: "rimaia".to_string(),
            path: "/Users/someone/Code/My Projects/rimaia".to_string(),
            default_branch: "main".to_string(),
            worktree_root: "/Users/someone/Library/Application Support/com.rimaia.app/worktrees"
                .to_string(),
            allow_unattended_runs: true,
            max_concurrency: 1,
            created_at,
            credential_login: None,
            credential_label: None,
            credential_added_at: None,
        }
    );
}

#[tokio::test]
async fn a_repository_inserted_without_allow_unattended_runs_defaults_to_false() {
    let pool = test_pool().await;
    let id = new_id();

    sqlx::query!(
        "INSERT INTO repositories (id, name, path, default_branch, worktree_root, created_at)
         VALUES (?1, 'rimaia', '/tmp/rimaia', 'main', '/tmp/rimaia-worktrees', ?2)",
        id,
        NOW,
    )
    .execute(&pool)
    .await
    .expect("insert a repository without an explicit opt-in");

    let repository = fetch_repository(&pool, &id).await;

    assert!(
        !repository.allow_unattended_runs,
        "a repository must not allow unattended runs until asked (ADR-0012)"
    );
}

#[tokio::test]
async fn a_setting_round_trips_its_key_and_value() {
    let pool = test_pool().await;

    // `base_instructions` is now seeded by 20260820120100_seed_settings.sql, so a
    // fresh key exercises the same insert-and-round-trip path without colliding
    // with that seed row.
    sqlx::query!(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)",
        "some_unseeded_setting",
        "Work from the plan. Keep commits small.",
    )
    .execute(&pool)
    .await
    .expect("insert a setting");

    let setting = fetch_setting(&pool, "some_unseeded_setting").await;

    assert_eq!(
        setting,
        Setting {
            key: "some_unseeded_setting".to_string(),
            value: "Work from the plan. Keep commits small.".to_string(),
        }
    );
}

#[tokio::test]
async fn a_task_round_trips_every_field_exactly() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let id = new_id();
    let created_at: DateTime<Utc> = "2026-08-20T09:00:00Z".parse().expect("rfc3339");
    let updated_at: DateTime<Utc> = "2026-08-20T09:30:00Z".parse().expect("rfc3339");
    let strategy_updated_at: DateTime<Utc> = "2026-08-20T09:15:00Z".parse().expect("rfc3339");

    sqlx::query!(
        "INSERT INTO tasks (
            id, repository_id, title, plan, extra_instructions, board_column, position,
            run_state, branch, worktree_path, strategy_mode, model, effort, strategy_plan,
            strategy_source, strategy_updated_at, created_at, updated_at, source
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        id,
        repository_id,
        "Wire the board to the store",
        "## Steps\n1. Do the thing",
        "Keep the diff small",
        BoardColumn::InReview,
        1.5,
        RunState::WaitingRetry,
        "rimaia/wire-the-board",
        "/tmp/rimaia-worktrees/wire-the-board",
        StrategyMode::Planned,
        "opus",
        "high",
        r#"{"phases":[]}"#,
        StrategySource::Planner,
        strategy_updated_at,
        created_at,
        updated_at,
        MutationSource::Mcp,
    )
    .execute(&pool)
    .await
    .expect("insert a fully populated task");

    let task = fetch_task(&pool, &id).await;

    assert_eq!(
        task,
        Task {
            id: id.clone(),
            repository_id: repository_id.clone(),
            title: "Wire the board to the store".to_string(),
            plan: Some("## Steps\n1. Do the thing".to_string()),
            extra_instructions: Some("Keep the diff small".to_string()),
            column: BoardColumn::InReview,
            position: 1.5,
            run_state: RunState::WaitingRetry,
            branch: Some("rimaia/wire-the-board".to_string()),
            worktree_path: Some("/tmp/rimaia-worktrees/wire-the-board".to_string()),
            strategy_mode: StrategyMode::Planned,
            model: Some("opus".to_string()),
            effort: Some("high".to_string()),
            strategy_plan: Some(r#"{"phases":[]}"#.to_string()),
            strategy_source: Some(StrategySource::Planner),
            strategy_updated_at: Some(strategy_updated_at),
            created_at,
            updated_at,
            source: MutationSource::Mcp,
        }
    );
}

#[tokio::test]
async fn a_task_without_a_board_column_or_run_state_is_rejected_by_the_schema() {
    // Not a default: unlike `allow_unattended_runs` and `strategy_mode`, the migration
    // gives `board_column` and `run_state` no `DEFAULT` — only `NOT NULL` plus a `CHECK`.
    // A freshly captured task landing on `not_ready`/`idle` is a rule for whichever code
    // creates a task through the service layer; task 002 is storage and models only, and
    // this is the backstop that keeps that true: a bare `INSERT` that omits either column
    // must fail, not silently choose a state on the schema's behalf.
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let id = new_id();

    let result = sqlx::query!(
        "INSERT INTO tasks (id, repository_id, title, position, created_at, updated_at)
         VALUES (?1, ?2, 'a task', 1.0, ?3, ?3)",
        id,
        repository_id,
        NOW,
    )
    .execute(&pool)
    .await;

    let error = result.expect_err("board_column has no DEFAULT, so omitting it must fail");
    assert!(
        error
            .to_string()
            .contains("NOT NULL constraint failed: tasks.board_column"),
        "expected a NOT NULL violation naming board_column, got: {error}"
    );
}

#[tokio::test]
async fn a_task_link_round_trips_every_field_exactly() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let task_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;
    let id = new_id();

    sqlx::query!(
        "INSERT INTO task_links (id, task_id, label, url, position) VALUES (?1, ?2, ?3, ?4, ?5)",
        id,
        task_id,
        "Design doc",
        "https://example.com/design",
        2.5,
    )
    .execute(&pool)
    .await
    .expect("insert a task link");

    let link = fetch_task_link(&pool, &id).await;

    assert_eq!(
        link,
        TaskLink {
            id: id.clone(),
            task_id: task_id.clone(),
            label: "Design doc".to_string(),
            url: "https://example.com/design".to_string(),
            position: 2.5,
        }
    );
}

#[tokio::test]
async fn a_task_dependency_round_trips_both_ends() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let dependent_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;
    let blocker_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;

    sqlx::query!(
        "INSERT INTO task_dependencies (task_id, depends_on_task_id) VALUES (?1, ?2)",
        dependent_id,
        blocker_id,
    )
    .execute(&pool)
    .await
    .expect("insert a dependency edge");

    let dependency = fetch_task_dependency(&pool, &dependent_id, &blocker_id).await;

    assert_eq!(
        dependency,
        TaskDependency {
            task_id: dependent_id.clone(),
            depends_on_task_id: blocker_id.clone(),
        }
    );
}

#[tokio::test]
async fn a_run_round_trips_every_field_exactly() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let task_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;
    let id = new_id();
    let started_at: DateTime<Utc> = "2026-08-20T02:00:00Z".parse().expect("rfc3339");
    let ended_at: DateTime<Utc> = "2026-08-20T02:12:34.5Z".parse().expect("rfc3339");
    let resume_after: DateTime<Utc> = "2026-08-20T06:00:00Z".parse().expect("rfc3339");

    sqlx::query!(
        "INSERT INTO runs (
            id, task_id, attempt, status, session_id, prompt, started_at, ended_at,
            exit_class, error_message, num_turns, cost_usd, log_path, pr_url, resume_after,
            base_ref, model, effort, run_environment,
            input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens
         ) VALUES (?1, ?2, 2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                   ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
        id,
        task_id,
        RunStatus::Failed,
        "session-9",
        "implement the thing",
        started_at,
        ended_at,
        ExitClass::UsageLimit,
        "hit the five-hour window",
        7_i64,
        1.2345,
        "/tmp/rimaia-runs/task/run.jsonl",
        "https://github.com/example/pr/9",
        resume_after,
        "main",
        "claude-sonnet-5",
        "high",
        "inherit",
        10_i64,
        1949_i64,
        163_145_i64,
        11_819_i64,
    )
    .execute(&pool)
    .await
    .expect("insert a fully populated run");

    let run = fetch_run(&pool, &id).await;

    assert_eq!(
        run,
        Run {
            id: id.clone(),
            task_id: task_id.clone(),
            attempt: 2,
            status: RunStatus::Failed,
            session_id: "session-9".to_string(),
            prompt: "implement the thing".to_string(),
            started_at,
            ended_at: Some(ended_at),
            exit_class: Some(ExitClass::UsageLimit),
            error_message: Some("hit the five-hour window".to_string()),
            num_turns: Some(7),
            cost_usd: Some(1.2345),
            log_path: "/tmp/rimaia-runs/task/run.jsonl".to_string(),
            pr_url: Some("https://github.com/example/pr/9".to_string()),
            resume_after: Some(resume_after),
            base_ref: Some("main".to_string()),
            model: Some("claude-sonnet-5".to_string()),
            effort: Some("high".to_string()),
            run_environment: Some("inherit".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(1949),
            cache_read_tokens: Some(163_145),
            cache_creation_tokens: Some(11_819),
        }
    );
}

#[tokio::test]
async fn a_run_round_trips_while_still_in_flight() {
    // The complement of the fully populated round trip: `ended_at` through
    // `resume_after` are `NULL` for the whole time a run is active, per the schema's own
    // comment. A struct built from an in-flight row has to come back with every one of
    // those fields `None`, not merely not error.
    //
    // ADR-0022's capture columns are `None` here for a stronger reason than
    // "not yet": seam-contract D18 makes NULL mean *not recorded*, and a run
    // still in flight has not recorded them. A zero here would be a claim that
    // it had spent nothing.
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let task_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Running).await;
    let id = new_id();
    let started_at: DateTime<Utc> = "2026-08-20T03:00:00Z".parse().expect("rfc3339");

    sqlx::query!(
        "INSERT INTO runs (id, task_id, attempt, status, session_id, prompt, started_at, log_path)
         VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7)",
        id,
        task_id,
        RunStatus::Running,
        "session-1",
        "implement the thing",
        started_at,
        "/tmp/rimaia-runs/task/run-1.jsonl",
    )
    .execute(&pool)
    .await
    .expect("insert an in-flight run");

    let run = fetch_run(&pool, &id).await;

    assert_eq!(
        run,
        Run {
            id: id.clone(),
            task_id: task_id.clone(),
            attempt: 1,
            status: RunStatus::Running,
            session_id: "session-1".to_string(),
            prompt: "implement the thing".to_string(),
            started_at,
            ended_at: None,
            exit_class: None,
            error_message: None,
            num_turns: None,
            cost_usd: None,
            log_path: "/tmp/rimaia-runs/task/run-1.jsonl".to_string(),
            pr_url: None,
            resume_after: None,
            base_ref: None,
            model: None,
            effort: None,
            run_environment: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        }
    );
}

#[tokio::test]
async fn a_schedule_round_trips_every_field_exactly() {
    let pool = test_pool().await;
    let id = new_id();
    let start_at: DateTime<Utc> = "2026-08-21T05:00:00Z".parse().expect("rfc3339");

    sqlx::query!(
        "INSERT INTO schedules (id, name, mode, cron, start_at, max_concurrency, enabled)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        id,
        "Weeknight run",
        ScheduleMode::Parallel,
        "0 22 * * 1-5",
        start_at,
        4_i64,
        false,
    )
    .execute(&pool)
    .await
    .expect("insert a fully populated schedule");

    let schedule = fetch_schedule(&pool, &id).await;

    assert_eq!(
        schedule,
        Schedule {
            id: id.clone(),
            name: "Weeknight run".to_string(),
            mode: ScheduleMode::Parallel,
            cron: Some("0 22 * * 1-5".to_string()),
            start_at: Some(start_at),
            max_concurrency: 4,
            enabled: false,
            // Task 013's four, all still NULL: this row was inserted by the
            // column list the initial schema shipped, which is exactly the
            // shape every row written before task 013 has.
            timezone: None,
            stop_at: None,
            last_fired_at: None,
            armed_at: None,
        }
    );
}

#[tokio::test]
async fn a_schedule_without_a_cron_or_start_at_takes_its_declared_defaults() {
    let pool = test_pool().await;
    let id = new_id();

    sqlx::query!(
        "INSERT INTO schedules (id, name, mode) VALUES (?1, 'run now', ?2)",
        id,
        ScheduleMode::Sequential,
    )
    .execute(&pool)
    .await
    .expect("insert a minimal schedule");

    let schedule = fetch_schedule(&pool, &id).await;

    assert_eq!(
        schedule,
        Schedule {
            id: id.clone(),
            name: "run now".to_string(),
            mode: ScheduleMode::Sequential,
            cron: None,
            start_at: None,
            max_concurrency: 2,
            enabled: true,
            timezone: None,
            stop_at: None,
            last_fired_at: None,
            armed_at: None,
        }
    );
}

// ---------------------------------------------------------------------------------
// Foreign keys — one test per FK, asserting the insert is rejected
// ---------------------------------------------------------------------------------

#[tokio::test]
async fn a_task_in_a_nonexistent_repository_is_rejected() {
    let pool = test_pool().await;
    let id = new_id();

    let result = sqlx::query!(
        "INSERT INTO tasks (id, repository_id, title, board_column, position, run_state, created_at, updated_at)
         VALUES (?1, 'no-such-repository', 'a task', 'ready', 1.0, 'idle', ?2, ?2)",
        id,
        NOW,
    )
    .execute(&pool)
    .await;

    assert_foreign_key_violation(result);
}

#[tokio::test]
async fn a_task_link_on_a_missing_task_is_rejected() {
    let pool = test_pool().await;
    let id = new_id();

    let result = sqlx::query!(
        "INSERT INTO task_links (id, task_id, label, url, position)
         VALUES (?1, 'no-such-task', 'label', 'https://example.com', 1.0)",
        id,
    )
    .execute(&pool)
    .await;

    assert_foreign_key_violation(result);
}

#[tokio::test]
async fn a_dependency_on_a_missing_task_is_rejected() {
    // `depends_on_task_id` names a task that does not exist.
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let task_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;

    let result = sqlx::query!(
        "INSERT INTO task_dependencies (task_id, depends_on_task_id) VALUES (?1, 'no-such-task')",
        task_id,
    )
    .execute(&pool)
    .await;

    assert_foreign_key_violation(result);
}

#[tokio::test]
async fn a_dependency_from_a_missing_task_is_rejected() {
    // The other end: `task_id` itself names a task that does not exist.
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let depends_on_id =
        insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;

    let result = sqlx::query!(
        "INSERT INTO task_dependencies (task_id, depends_on_task_id) VALUES ('no-such-task', ?1)",
        depends_on_id,
    )
    .execute(&pool)
    .await;

    assert_foreign_key_violation(result);
}

#[tokio::test]
async fn a_run_on_a_missing_task_is_rejected() {
    let pool = test_pool().await;
    let id = new_id();

    let result = sqlx::query!(
        "INSERT INTO runs (id, task_id, attempt, status, session_id, prompt, started_at, log_path)
         VALUES (?1, 'no-such-task', 1, 'running', 'session', 'prompt', ?2, '/tmp/log.jsonl')",
        id,
        NOW,
    )
    .execute(&pool)
    .await;

    assert_foreign_key_violation(result);
}

// ---------------------------------------------------------------------------------
// Cascade and restrict — task 002's "cascade behaviour on task delete", plus the
// asymmetry ADR-0008 requires
// ---------------------------------------------------------------------------------

#[tokio::test]
async fn deleting_a_task_cascades_its_links_edges_and_runs() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let task_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;
    // What this task depends on: an outgoing edge, which CASCADE must remove along with
    // the task. The task this one blocks is covered separately, below — that edge must
    // survive nothing, because the row it names is gone too, and the *refusal* to delete
    // a depended-upon task is its own test.
    let blocker_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;

    let link_id = insert_task_link(&pool, &task_id).await;
    let run_id = insert_run(
        &pool,
        &task_id,
        1,
        RunStatus::Succeeded,
        Some(ExitClass::Success),
    )
    .await;
    sqlx::query!(
        "INSERT INTO task_dependencies (task_id, depends_on_task_id) VALUES (?1, ?2)",
        task_id,
        blocker_id,
    )
    .execute(&pool)
    .await
    .expect("seed an outgoing edge");

    sqlx::query!("DELETE FROM tasks WHERE id = ?1", task_id)
        .execute(&pool)
        .await
        .expect("deleting a task with only outgoing edges must succeed");

    assert!(
        !row_exists(&pool, "task_links", &link_id).await,
        "the task's links must cascade"
    );
    assert!(
        !row_exists(&pool, "runs", &run_id).await,
        "the task's runs must cascade"
    );
    assert_eq!(
        outgoing_dependency_count(&pool, &task_id).await,
        0,
        "the task's own outgoing edge must cascade"
    );
    assert!(
        row_exists(&pool, "tasks", &blocker_id).await,
        "the task it depended on must survive; only the edge pointing at it is the task's own"
    );
}

#[tokio::test]
async fn deleting_a_task_other_tasks_depend_on_is_refused() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let blocker_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;
    let dependent_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;
    sqlx::query!(
        "INSERT INTO task_dependencies (task_id, depends_on_task_id) VALUES (?1, ?2)",
        dependent_id,
        blocker_id,
    )
    .execute(&pool)
    .await
    .expect("seed the edge");

    let result = sqlx::query!("DELETE FROM tasks WHERE id = ?1", blocker_id)
        .execute(&pool)
        .await;

    assert_foreign_key_violation(result);
    assert!(
        row_exists(&pool, "tasks", &blocker_id).await,
        "a refused delete must leave the task in place"
    );
}

#[tokio::test]
async fn deleting_a_repository_that_still_has_tasks_is_refused() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;

    let result = sqlx::query!("DELETE FROM repositories WHERE id = ?1", repository_id)
        .execute(&pool)
        .await;

    assert_foreign_key_violation(result);
    assert!(
        row_exists(&pool, "repositories", &repository_id).await,
        "a refused delete must leave the repository in place"
    );
}

// ---------------------------------------------------------------------------------
// Domain backstops — values outside the declared domain, refused by the schema
// ---------------------------------------------------------------------------------

#[tokio::test]
async fn an_unrecognised_board_column_is_refused() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let id = new_id();

    let result = sqlx::query!(
        "INSERT INTO tasks (id, repository_id, title, board_column, position, run_state, created_at, updated_at)
         VALUES (?1, ?2, 'a task', 'archived', 1.0, 'idle', ?3, ?3)",
        id,
        repository_id,
        NOW,
    )
    .execute(&pool)
    .await;

    assert_check_violation(result);
}

#[tokio::test]
async fn an_unrecognised_run_state_is_refused() {
    // `interrupted` in particular (seam-contract D9): it belongs to a run, never to a
    // task's `run_state`, and the CHECK is what makes that permanent.
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let id = new_id();

    let result = sqlx::query!(
        "INSERT INTO tasks (id, repository_id, title, board_column, position, run_state, created_at, updated_at)
         VALUES (?1, ?2, 'a task', 'ready', 1.0, 'interrupted', ?3, ?3)",
        id,
        repository_id,
        NOW,
    )
    .execute(&pool)
    .await;

    assert_check_violation(result);
}

#[tokio::test]
async fn the_source_column_rejects_a_spelling_outside_its_check() {
    // ADR-0019's three doors, and only these three. A fourth would be a
    // rename-copy-drop rebuild, so the CHECK is what makes the domain
    // permanent — the same argument the initial schema's header makes for
    // every other enum column.
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let id = new_id();

    let result = sqlx::query!(
        "INSERT INTO tasks (id, repository_id, title, board_column, position, run_state, created_at, updated_at, source)
         VALUES (?1, ?2, 'a task', 'ready', 1.0, 'idle', ?3, ?3, 'cli')",
        id,
        repository_id,
        NOW,
    )
    .execute(&pool)
    .await;

    assert_check_violation(result);
}

#[tokio::test]
async fn an_existing_row_backfills_to_ui() {
    // The migration's `DEFAULT 'ui'` is what backfills every row written before
    // task 010 — and every row written by a statement that predates the column,
    // which is what this insert stands in for. `ui` is the fact rather than a
    // guess: the board was the only writer that existed.
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;

    let task = fetch_task(&pool, &id).await;

    assert_eq!(task.source, MutationSource::Ui);
}

#[tokio::test]
async fn source_variants_round_trip_through_a_real_check_constraint() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;

    for source in [
        MutationSource::Ui,
        MutationSource::Mcp,
        MutationSource::System,
    ] {
        let id = new_id();
        sqlx::query!(
            "INSERT INTO tasks (id, repository_id, title, board_column, position, run_state, created_at, updated_at, source)
             VALUES (?1, ?2, 'a task', 'ready', 1.0, 'idle', ?3, ?3, ?4)",
            id,
            repository_id,
            NOW,
            source,
        )
        .execute(&pool)
        .await
        .unwrap_or_else(|error| panic!("{source:?} must satisfy the CHECK: {error}"));

        assert_eq!(fetch_task(&pool, &id).await.source, source);
    }
}

#[tokio::test]
async fn an_unrecognised_exit_class_is_refused() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let task_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;
    let id = new_id();

    let result = sqlx::query!(
        "INSERT INTO runs (id, task_id, attempt, status, session_id, prompt, started_at, exit_class, log_path)
         VALUES (?1, ?2, 1, 'failed', 'session', 'prompt', ?3, 'timeout', '/tmp/log.jsonl')",
        id,
        task_id,
        NOW,
    )
    .execute(&pool)
    .await;

    assert_check_violation(result);
}

#[tokio::test]
async fn a_task_cannot_depend_on_itself() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let task_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;

    let result = sqlx::query!(
        "INSERT INTO task_dependencies (task_id, depends_on_task_id) VALUES (?1, ?1)",
        task_id,
    )
    .execute(&pool)
    .await;

    assert_check_violation(result);
}

#[tokio::test]
async fn a_second_run_with_the_same_attempt_number_is_refused() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let task_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;
    insert_run(&pool, &task_id, 1, RunStatus::Running, None).await;

    let second = new_id();
    let result = sqlx::query!(
        "INSERT INTO runs (id, task_id, attempt, status, session_id, prompt, started_at, log_path)
         VALUES (?1, ?2, 1, 'running', 'session-2', 'prompt', ?3, '/tmp/log-2.jsonl')",
        second,
        task_id,
        NOW,
    )
    .execute(&pool)
    .await;

    assert_unique_violation(result);
}

// Note on "a blank title is refused": the migration constrains `tasks.title` with
// `NOT NULL` only — no `CHECK` on its length or content — so an empty string is a legal
// title at this layer. Refusing a *blank* one (as opposed to a missing one) is a
// validation rule with no schema hook to test; SQLite has nothing to enforce it, and
// task 002 is storage and models only. Left for whichever task owns task creation to
// enforce and test in its own service layer.

// ---------------------------------------------------------------------------------
// Enum text agreement
// ---------------------------------------------------------------------------------
//
// `db::models`'s own unit tests prove `Encode` fills its buffer with the word the
// `CHECK` expects, without ever opening a database. The seven tests below are that
// same claim proven the other way: a real `INSERT` through the real `CHECK`, for every
// variant of every enum the schema constrains. The eighth closes the remaining gap —
// that the literal bytes SQLite ends up holding are the same word `serde_json` puts on
// the wire, which is the invariant `src/types.ts` hand-mirrors and nothing else checks.

#[tokio::test]
async fn board_column_variants_round_trip_through_a_real_check_constraint() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;

    for column in [
        BoardColumn::NotReady,
        BoardColumn::Ready,
        BoardColumn::InReview,
        BoardColumn::Done,
    ] {
        let id = insert_task(&pool, &repository_id, column, RunState::Idle).await;
        let task = fetch_task(&pool, &id).await;
        assert_eq!(task.column, column);
    }
}

#[tokio::test]
async fn run_state_variants_round_trip_through_a_real_check_constraint() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;

    for state in [
        RunState::Idle,
        RunState::Queued,
        RunState::Running,
        RunState::Blocked,
        RunState::WaitingRetry,
        RunState::Failed,
        RunState::Cancelled,
    ] {
        let id = insert_task(&pool, &repository_id, BoardColumn::Ready, state).await;
        let task = fetch_task(&pool, &id).await;
        assert_eq!(task.run_state, state);
    }
}

#[tokio::test]
async fn run_status_variants_round_trip_through_a_real_check_constraint() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let task_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;

    for (attempt, status) in [
        RunStatus::Running,
        RunStatus::Succeeded,
        RunStatus::Failed,
        RunStatus::Cancelled,
        RunStatus::Interrupted,
    ]
    .into_iter()
    .enumerate()
    {
        let id = insert_run(&pool, &task_id, attempt as i64 + 1, status, None).await;
        let run = fetch_run(&pool, &id).await;
        assert_eq!(run.status, status);
    }
}

#[tokio::test]
async fn exit_class_variants_round_trip_through_a_real_check_constraint() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;
    let task_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;

    for (attempt, class) in [
        ExitClass::Success,
        ExitClass::UsageLimit,
        ExitClass::Transient,
        ExitClass::Interrupted,
        ExitClass::Fatal,
        ExitClass::Cancelled,
    ]
    .into_iter()
    .enumerate()
    {
        let id = insert_run(
            &pool,
            &task_id,
            attempt as i64 + 1,
            RunStatus::Failed,
            Some(class),
        )
        .await;
        let run = fetch_run(&pool, &id).await;
        assert_eq!(run.exit_class, Some(class));
    }
}

#[tokio::test]
async fn strategy_mode_variants_round_trip_through_a_real_check_constraint() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;

    for mode in [
        StrategyMode::Default,
        StrategyMode::Manual,
        StrategyMode::Planned,
    ] {
        let id = insert_task_with_strategy(&pool, &repository_id, mode, None).await;
        let task = fetch_task(&pool, &id).await;
        assert_eq!(task.strategy_mode, mode);
    }
}

#[tokio::test]
async fn strategy_source_variants_round_trip_through_a_real_check_constraint() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;

    for source in [StrategySource::User, StrategySource::Planner] {
        let id =
            insert_task_with_strategy(&pool, &repository_id, StrategyMode::Planned, Some(source))
                .await;
        let task = fetch_task(&pool, &id).await;
        assert_eq!(task.strategy_source, Some(source));
    }
}

#[tokio::test]
async fn schedule_mode_variants_round_trip_through_a_real_check_constraint() {
    let pool = test_pool().await;

    for mode in [ScheduleMode::Sequential, ScheduleMode::Parallel] {
        let id = insert_schedule(&pool, mode).await;
        let schedule = fetch_schedule(&pool, &id).await;
        assert_eq!(schedule.mode, mode);
    }
}

#[tokio::test]
async fn the_text_sqlx_stores_agrees_with_the_word_serde_puts_on_the_wire() {
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;

    for (column, wire) in [
        (BoardColumn::NotReady, "not_ready"),
        (BoardColumn::Ready, "ready"),
        (BoardColumn::InReview, "in_review"),
        (BoardColumn::Done, "done"),
    ] {
        let id = insert_task(&pool, &repository_id, column, RunState::Idle).await;
        assert_eq!(wire_text(column), wire, "what serde writes");
        assert_eq!(
            stored_text(&pool, "tasks", "board_column", &id).await,
            wire,
            "what sqlx stores"
        );
    }

    for (state, wire) in [
        (RunState::Idle, "idle"),
        (RunState::Queued, "queued"),
        (RunState::Running, "running"),
        (RunState::Blocked, "blocked"),
        (RunState::WaitingRetry, "waiting_retry"),
        (RunState::Failed, "failed"),
        (RunState::Cancelled, "cancelled"),
    ] {
        let id = insert_task(&pool, &repository_id, BoardColumn::Ready, state).await;
        assert_eq!(wire_text(state), wire, "what serde writes");
        assert_eq!(
            stored_text(&pool, "tasks", "run_state", &id).await,
            wire,
            "what sqlx stores"
        );
    }

    for (mode, wire) in [
        (StrategyMode::Default, "default"),
        (StrategyMode::Manual, "manual"),
        (StrategyMode::Planned, "planned"),
    ] {
        let id = insert_task_with_strategy(&pool, &repository_id, mode, None).await;
        assert_eq!(wire_text(mode), wire, "what serde writes");
        assert_eq!(
            stored_text(&pool, "tasks", "strategy_mode", &id).await,
            wire,
            "what sqlx stores"
        );
    }

    for (source, wire) in [
        (StrategySource::User, "user"),
        (StrategySource::Planner, "planner"),
    ] {
        let id =
            insert_task_with_strategy(&pool, &repository_id, StrategyMode::Planned, Some(source))
                .await;
        assert_eq!(wire_text(source), wire, "what serde writes");
        assert_eq!(
            stored_text(&pool, "tasks", "strategy_source", &id).await,
            wire,
            "what sqlx stores"
        );
    }

    let task_id = insert_task(&pool, &repository_id, BoardColumn::Ready, RunState::Idle).await;
    let mut attempt = 0_i64;

    for (status, wire) in [
        (RunStatus::Running, "running"),
        (RunStatus::Succeeded, "succeeded"),
        (RunStatus::Failed, "failed"),
        (RunStatus::Cancelled, "cancelled"),
        (RunStatus::Interrupted, "interrupted"),
    ] {
        attempt += 1;
        let id = insert_run(&pool, &task_id, attempt, status, None).await;
        assert_eq!(wire_text(status), wire, "what serde writes");
        assert_eq!(
            stored_text(&pool, "runs", "status", &id).await,
            wire,
            "what sqlx stores"
        );
    }

    for (class, wire) in [
        (ExitClass::Success, "success"),
        (ExitClass::UsageLimit, "usage_limit"),
        (ExitClass::Transient, "transient"),
        (ExitClass::Interrupted, "interrupted"),
        (ExitClass::Fatal, "fatal"),
        (ExitClass::Cancelled, "cancelled"),
    ] {
        attempt += 1;
        let id = insert_run(&pool, &task_id, attempt, RunStatus::Failed, Some(class)).await;
        assert_eq!(wire_text(class), wire, "what serde writes");
        assert_eq!(
            stored_text(&pool, "runs", "exit_class", &id).await,
            wire,
            "what sqlx stores"
        );
    }

    for (mode, wire) in [
        (ScheduleMode::Sequential, "sequential"),
        (ScheduleMode::Parallel, "parallel"),
    ] {
        let id = insert_schedule(&pool, mode).await;
        assert_eq!(wire_text(mode), wire, "what serde writes");
        assert_eq!(
            stored_text(&pool, "schedules", "mode", &id).await,
            wire,
            "what sqlx stores"
        );
    }
}

// ---------------------------------------------------------------------------------
// Timestamp text
// ---------------------------------------------------------------------------------

#[tokio::test]
async fn a_bound_timestamp_is_stored_with_a_numeric_offset_rather_than_z() {
    // The bytes, not the instant. Decoding round-trips both spellings, so no round-trip
    // test above can see this: sqlx passes `use_z: false` to chrono's `to_rfc3339_opts`
    // (sqlx-sqlite 0.8.6, `src/types/chrono.rs:69`) and therefore never writes `Z`,
    // while chrono's serde impl — what `src/types.ts` sees — always does. The two
    // spellings sort differently, `tasks.created_at` is the tie-break a rebalance orders
    // by, and board order is execution order (ADR-0007), so this is the format's pin:
    // change it and a second writer following the migration's header is now wrong.
    let pool = test_pool().await;
    let repository_id = insert_repository(&pool).await;

    for (instant, stored) in [
        // Whole seconds, then the two sub-second widths `SecondsFormat::AutoSi` picks
        // between — a trailing `.5` is written `.500`, not `.5`.
        ("2026-08-20T12:00:00Z", "2026-08-20T12:00:00+00:00"),
        ("2026-08-20T12:00:00.5Z", "2026-08-20T12:00:00.500+00:00"),
        (
            "2026-08-20T12:00:00.123456Z",
            "2026-08-20T12:00:00.123456+00:00",
        ),
    ] {
        let created_at: DateTime<Utc> = instant.parse().expect("rfc3339");
        let id = new_id();

        sqlx::query!(
            "INSERT INTO tasks (id, repository_id, title, board_column, position, run_state, created_at, updated_at)
             VALUES (?1, ?2, 'a task', 'ready', 1.0, 'idle', ?3, ?3)",
            id,
            repository_id,
            created_at,
        )
        .execute(&pool)
        .await
        .expect("insert a task with a bound timestamp");

        assert_eq!(
            stored_text(&pool, "tasks", "created_at", &id).await,
            stored,
            "what sqlx stores for {instant}"
        );
    }
}

// ---------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------

/// The text `serde_json` puts on the wire for one enum value, mirroring
/// `db::models::tests::on_the_wire` — duplicated rather than imported because that
/// helper is private to a `#[cfg(test)]` module of a different crate target.
fn wire_text<T: Serialize>(value: T) -> String {
    match serde_json::to_value(value).expect("an enum must always serialize") {
        serde_json::Value::String(text) => text,
        other => panic!("an enum must serialize to a JSON string, got {other}"),
    }
}

/// Read from sqlx's own bookkeeping table, not the schema, so a passing test proves the
/// migrator recognised what it had already applied — re-running a `CREATE TABLE` would
/// fail long before this could tell.
async fn applied_migration_versions(pool: &SqlitePool) -> Vec<i64> {
    // The `!` is the same nullability trap `db::mod`'s own test documents: sqlx declares
    // its own `version` column without `NOT NULL`.
    sqlx::query_scalar!(r#"SELECT version AS "version!" FROM _sqlx_migrations ORDER BY version"#)
        .fetch_all(pool)
        .await
        .expect("the migrator's own table must be readable")
}

/// One row per catalogue entry — table, index, or trigger — as SQLite itself would
/// recreate it. `sql` is `None` for the automatic indexes a composite primary key or a
/// `UNIQUE` constraint creates without an explicit `CREATE INDEX`.
#[derive(Debug, PartialEq)]
struct SchemaEntry {
    kind: String,
    name: String,
    tbl_name: String,
    sql: Option<String>,
}

async fn schema_snapshot(pool: &SqlitePool) -> Vec<SchemaEntry> {
    sqlx::query_as!(
        SchemaEntry,
        r#"SELECT type AS "kind!", name AS "name!", tbl_name AS "tbl_name!", sql
           FROM sqlite_master ORDER BY type, name"#
    )
    .fetch_all(pool)
    .await
    .expect("read sqlite_master")
}

async fn fetch_repository(pool: &SqlitePool, id: &str) -> Repository {
    sqlx::query_as!(
        Repository,
        r#"SELECT id, name, path, default_branch, worktree_root, allow_unattended_runs,
            max_concurrency, created_at AS "created_at: DateTime<Utc>",
            credential_login, credential_label,
            credential_added_at AS "credential_added_at: DateTime<Utc>"
           FROM repositories WHERE id = ?1"#,
        id,
    )
    .fetch_one(pool)
    .await
    .expect("read back the repository")
}

async fn fetch_setting(pool: &SqlitePool, key: &str) -> Setting {
    sqlx::query_as!(
        Setting,
        "SELECT key, value FROM settings WHERE key = ?1",
        key
    )
    .fetch_one(pool)
    .await
    .expect("read back the setting")
}

async fn fetch_task(pool: &SqlitePool, id: &str) -> Task {
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
    .fetch_one(pool)
    .await
    .expect("read back the task")
}

async fn fetch_task_link(pool: &SqlitePool, id: &str) -> TaskLink {
    sqlx::query_as!(
        TaskLink,
        "SELECT id, task_id, label, url, position FROM task_links WHERE id = ?1",
        id,
    )
    .fetch_one(pool)
    .await
    .expect("read back the task link")
}

async fn fetch_task_dependency(
    pool: &SqlitePool,
    task_id: &str,
    depends_on_task_id: &str,
) -> TaskDependency {
    sqlx::query_as!(
        TaskDependency,
        "SELECT task_id, depends_on_task_id FROM task_dependencies
         WHERE task_id = ?1 AND depends_on_task_id = ?2",
        task_id,
        depends_on_task_id,
    )
    .fetch_one(pool)
    .await
    .expect("read back the dependency")
}

async fn fetch_run(pool: &SqlitePool, id: &str) -> Run {
    sqlx::query_as!(
        Run,
        r#"SELECT id, task_id, attempt, status AS "status: RunStatus", session_id, prompt,
            started_at AS "started_at: DateTime<Utc>", ended_at AS "ended_at: DateTime<Utc>",
            exit_class AS "exit_class: ExitClass", error_message, num_turns, cost_usd, log_path,
            pr_url, resume_after AS "resume_after: DateTime<Utc>", base_ref,
            model, effort, run_environment, input_tokens, output_tokens,
            cache_read_tokens, cache_creation_tokens
           FROM runs WHERE id = ?1"#,
        id,
    )
    .fetch_one(pool)
    .await
    .expect("read back the run")
}

async fn fetch_schedule(pool: &SqlitePool, id: &str) -> Schedule {
    sqlx::query_as!(
        Schedule,
        r#"SELECT id, name, mode AS "mode: ScheduleMode", cron,
            start_at AS "start_at: DateTime<Utc>", max_concurrency,
            enabled AS "enabled: bool", timezone, stop_at,
            last_fired_at AS "last_fired_at: DateTime<Utc>",
            armed_at AS "armed_at: DateTime<Utc>"
           FROM schedules WHERE id = ?1"#,
        id,
    )
    .fetch_one(pool)
    .await
    .expect("read back the schedule")
}

/// A minimal `repositories` row; most tests only need its id as the foreign key
/// `tasks.repository_id` requires.
async fn insert_repository(pool: &SqlitePool) -> String {
    let id = new_id();
    sqlx::query!(
        "INSERT INTO repositories
            (id, name, path, default_branch, worktree_root, allow_unattended_runs, created_at)
         VALUES (?1, 'rimaia', '/tmp/rimaia', 'main', '/tmp/rimaia-worktrees', 0, ?2)",
        id,
        NOW,
    )
    .execute(pool)
    .await
    .expect("insert a repository fixture");
    id
}

/// A minimal `tasks` row with the two fields most tests vary: `board_column` and
/// `run_state`. Everything else is a fixed placeholder value no test in this file reads.
async fn insert_task(
    pool: &SqlitePool,
    repository_id: &str,
    column: BoardColumn,
    run_state: RunState,
) -> String {
    let id = new_id();
    sqlx::query!(
        "INSERT INTO tasks (id, repository_id, title, board_column, position, run_state, created_at, updated_at)
         VALUES (?1, ?2, 'a task', ?3, 1.0, ?4, ?5, ?6)",
        id,
        repository_id,
        column,
        run_state,
        NOW,
        NOW,
    )
    .execute(pool)
    .await
    .expect("insert a task fixture");
    id
}

/// A `tasks` row with an explicit strategy, for the two enum columns [`insert_task`]
/// leaves on their schema default (`strategy_mode`) or `NULL` (`strategy_source`).
async fn insert_task_with_strategy(
    pool: &SqlitePool,
    repository_id: &str,
    strategy_mode: StrategyMode,
    strategy_source: Option<StrategySource>,
) -> String {
    let id = new_id();
    sqlx::query!(
        "INSERT INTO tasks
            (id, repository_id, title, board_column, position, run_state, strategy_mode, strategy_source, created_at, updated_at)
         VALUES (?1, ?2, 'a task', 'ready', 1.0, 'idle', ?3, ?4, ?5, ?6)",
        id,
        repository_id,
        strategy_mode,
        strategy_source,
        NOW,
        NOW,
    )
    .execute(pool)
    .await
    .expect("insert a task fixture with an explicit strategy");
    id
}

async fn insert_task_link(pool: &SqlitePool, task_id: &str) -> String {
    let id = new_id();
    sqlx::query!(
        "INSERT INTO task_links (id, task_id, label, url, position)
         VALUES (?1, ?2, 'label', 'https://example.com', 1.0)",
        id,
        task_id,
    )
    .execute(pool)
    .await
    .expect("insert a task link fixture");
    id
}

async fn insert_run(
    pool: &SqlitePool,
    task_id: &str,
    attempt: i64,
    status: RunStatus,
    exit_class: Option<ExitClass>,
) -> String {
    let id = new_id();
    sqlx::query!(
        "INSERT INTO runs (id, task_id, attempt, status, session_id, prompt, started_at, exit_class, log_path)
         VALUES (?1, ?2, ?3, ?4, ?5, 'do the thing', ?6, ?7, ?8)",
        id,
        task_id,
        attempt,
        status,
        id,
        NOW,
        exit_class,
        id,
    )
    .execute(pool)
    .await
    .expect("insert a run fixture");
    id
}

async fn insert_schedule(pool: &SqlitePool, mode: ScheduleMode) -> String {
    let id = new_id();
    sqlx::query!(
        "INSERT INTO schedules (id, name, mode, max_concurrency, enabled) VALUES (?1, 'nightly', ?2, 2, 1)",
        id,
        mode,
    )
    .execute(pool)
    .await
    .expect("insert a schedule fixture");
    id
}

async fn row_exists(pool: &SqlitePool, table: &str, id: &str) -> bool {
    // The one place this file builds SQL at runtime rather than through `sqlx::query!`:
    // the table name varies and a bind parameter cannot stand in for an identifier.
    // Every other query in this file has a query!-checkable fixed shape (seam-contract
    // D5); this one genuinely does not.
    let query = format!("SELECT count(*) FROM {table} WHERE id = ?1");
    let count: i64 = sqlx::query_scalar(&query)
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("count rows");
    count > 0
}

async fn outgoing_dependency_count(pool: &SqlitePool, task_id: &str) -> i64 {
    sqlx::query_scalar!(
        "SELECT count(*) FROM task_dependencies WHERE task_id = ?1",
        task_id,
    )
    .fetch_one(pool)
    .await
    .expect("count outgoing edges")
}

/// The raw text one column of one row holds, read without an enum override — the half
/// of the wire-agreement invariant a decoded fetch cannot see.
async fn stored_text(pool: &SqlitePool, table: &str, column: &str, id: &str) -> String {
    // Runtime SQL for the same reason `row_exists` needs it: `table` and `column` vary
    // per call and neither can be a bind parameter.
    let query = format!("SELECT {column} FROM {table} WHERE id = ?1");
    sqlx::query_scalar(&query)
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read the raw column text")
}

fn assert_foreign_key_violation<T: std::fmt::Debug>(result: sqlx::Result<T>) {
    let error = result.expect_err("a foreign key that names no row must be rejected");
    assert!(
        error.to_string().contains("FOREIGN KEY constraint failed"),
        "expected a foreign key violation, got: {error}"
    );
}

fn assert_check_violation<T: std::fmt::Debug>(result: sqlx::Result<T>) {
    let error = result.expect_err("a value outside the declared domain must be rejected");
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "expected a CHECK violation, got: {error}"
    );
}

fn assert_unique_violation<T: std::fmt::Debug>(result: sqlx::Result<T>) {
    let error = result.expect_err("a duplicate attempt number must be rejected");
    assert!(
        error.to_string().contains("UNIQUE constraint failed"),
        "expected a UNIQUE violation, got: {error}"
    );
}
