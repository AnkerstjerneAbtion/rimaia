//! Task 066: checkouts and worktree records move to the runner, and no board
//! DTO carries an absolute path (ADR-0028 point 2, ADR-0033 points 2 and 3).
//!
//! Real git in a `TempDir` throughout, and the machine store core's tests
//! reach (`MemoryMachine`, bound to `runner.db` by `machine_store_contract!`).
//! Each test says "the machine store" where it asserts on one.

// Every test that spawns the fake CLI is `#[cfg(unix)]`: it is a shell script,
// and Windows will not execute one. So are the fixture pieces only those tests
// reach, allowed rather than gated one by one, as in `archive.rs`.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pretty_assertions::assert_eq;
use rimaia_core::board::LeaseRef;
use rimaia_core::board::OwnerPresence;
use rimaia_core::db::{BoardColumn, MutationSource, RunState};
use rimaia_core::mcp::requests::{GetTaskRequest, ListTasksRequest};
use rimaia_core::mcp::RimaiaServer;
use rimaia_core::repo::{self, NewRepository, RepositoryPatch};
use rimaia_core::runner::events::transcript_path;
use rimaia_core::runner::{
    claim_manual_start, run_task, CancelSignal, ManualStart, RunRequest, RunTrigger, RunnerConfig,
};
use rimaia_core::runs::{self, transcript, PruneCriterion};
use rimaia_core::scheduler::{self, InFlight, SkipReason};
use rimaia_core::tasks::{self, NewTask, TaskFilter, TaskPatch};
use rimaia_core::testing::board::claim_run;
use rimaia_core::testing::{self, FakeCli, TempRepo, TestContext};
use rimaia_core::{worktree, AppPaths, ServiceContext};
use rmcp::handler::server::wrapper::{Json, Parameters};
use serde_json::{json, Value};
use tempfile::TempDir;

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// No board query reads a retired column
// ---------------------------------------------------------------------------

/// The columns task 065 drops: every per-machine fact the board's rows held
/// until this task, matched as whole identifiers.
const RETIRED: [&str; 10] = [
    "worktree_path",
    "worktree_root",
    "log_path",
    "on_archive",
    "on_archive_script",
    "credential_login",
    "credential_label",
    "credential_added_at",
    "path",
    "max_concurrency",
];

/// 041's adoption read, the one query allowed to name every retired column.
const ADOPTION_MARKER: &str = "-- machine_state adoption";
/// `start_run`'s insert, which still writes `runs.log_path` until 056.
const LOG_PATH_WRITE_MARKER: &str = "-- runs.log_path written until 056";

#[test]
fn no_board_query_reads_a_retired_column() {
    let cache = Path::new(env!("CARGO_MANIFEST_DIR")).join(".sqlx");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&cache)
        .expect("the board's offline query cache")
        .map(|entry| entry.expect("a cache entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    entries.sort();
    assert!(
        !entries.is_empty(),
        "the cache at {} is empty",
        cache.display()
    );

    let mut offences = Vec::new();
    for entry in &entries {
        let json: Value =
            serde_json::from_str(&std::fs::read_to_string(entry).expect("read a cache entry"))
                .expect("a cache entry is JSON");
        let query = json["query"].as_str().expect("an entry names its query");
        if query.contains(ADOPTION_MARKER) {
            continue;
        }
        let writes_log_path = query.contains(LOG_PATH_WRITE_MARKER);

        let text = identifiers(&strip_literals_and_comments(query));
        let columns: Vec<String> = json["describe"]["columns"]
            .as_array()
            .map(|columns| {
                columns
                    .iter()
                    .filter_map(|column| column["name"].as_str())
                    .flat_map(identifiers)
                    .collect()
            })
            .unwrap_or_default();

        for retired in RETIRED {
            let in_text = text.iter().any(|word| word == retired);
            let in_columns = columns.iter().any(|word| word == retired);
            let allowed_in_text = writes_log_path && retired == "log_path";
            if (in_text && !allowed_in_text) || in_columns {
                offences.push(format!(
                    "{} names `{retired}`:\n{query}",
                    entry.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
    }

    assert!(
        offences.is_empty(),
        "a board query reads a column task 065 drops; read it from the machine store \
         instead:\n\n{}",
        offences.join("\n\n")
    );
}

/// The query with every `'…'` string literal and every `--` or `/* */` comment
/// blanked out, so neither a literal nor a comment counts as a read.
fn strip_literals_and_comments(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    let mut chars = query.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                // SQL doubles a quote to escape it, which this reads as two
                // adjacent literals: blanked either way.
                for inner in chars.by_ref() {
                    if inner == '\'' {
                        break;
                    }
                }
                out.push(' ');
            }
            '-' if chars.peek() == Some(&'-') => {
                for inner in chars.by_ref() {
                    if inner == '\n' {
                        break;
                    }
                }
                out.push('\n');
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = ' ';
                for inner in chars.by_ref() {
                    if previous == '*' && inner == '/' {
                        break;
                    }
                    previous = inner;
                }
                out.push(' ');
            }
            other => out.push(other),
        }
    }
    out
}

/// Every identifier-shaped word, so `path` matches `path` and never
/// `worktree_path` or `json_each`.
fn identifiers(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn the_retired_column_check_sees_through_aliases_and_ignores_literals() {
    // The two ways a read could hide: an alias over a retired column, and a
    // column named in a literal or a comment, which is no read at all.
    let aliased = identifiers(&strip_literals_and_comments(
        r#"SELECT t.worktree_path AS "wt!" FROM tasks t"#,
    ));
    assert!(aliased.iter().any(|word| word == "worktree_path"));

    let quoted = identifiers(&strip_literals_and_comments(
        "SELECT id FROM tasks WHERE title = 'path' -- path\n /* log_path */",
    ));
    assert!(!quoted
        .iter()
        .any(|word| word == "path" || word == "log_path"));
}

// ---------------------------------------------------------------------------
// No board DTO carries an absolute path
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn no_board_dto_carries_an_absolute_path() {
    let f = Fixture::new().await;
    // A plan and titles with neither separator, so any `/` or `\` a string
    // carries came from somewhere else.
    let task = f.task("Wire the board", "1. Wire it").await;
    f.harness
        .prepare_worktree(&task)
        .await
        .expect("worktree::prepare");
    let cli = FakeCli::new();
    cli.replays(&task, "success", 0);
    let run = f
        .run(&cli, &task)
        .await
        .expect("a run finished from a fixture");

    let mut answers: Vec<(&str, Value)> = Vec::new();
    let mut record = |name: &'static str, value: Value| answers.push((name, value));

    record(
        "list_repositories",
        to_json(&repo::list(f.ctx()).await.expect("list")),
    );
    record(
        "update_repository",
        to_json(
            &repo::update(
                f.ctx(),
                &f.repository_id,
                RepositoryPatch {
                    name: Some("Renamed".to_string()),
                    default_branch: None,
                },
            )
            .await
            .expect("update"),
        ),
    );
    let created = tasks::create_task(
        f.ctx(),
        NewTask {
            repository_id: f.repository_id.clone(),
            title: "Another card".to_string(),
            plan: Some("2. Then this".to_string()),
            extra_instructions: None,
            column: Some(BoardColumn::NotReady),
            links: vec![],
        },
    )
    .await
    .expect("create");
    record("create_task", to_json(&created));
    record(
        "get_task",
        to_json(&tasks::get_task(f.ctx(), &task).await.expect("get")),
    );
    record(
        "list_tasks",
        to_json(
            &tasks::list_tasks(f.ctx(), TaskFilter::default())
                .await
                .expect("list"),
        ),
    );
    record(
        "update_task",
        to_json(
            &tasks::update_task(
                f.ctx(),
                &created.id,
                TaskPatch {
                    title: Some("Another card, renamed".to_string()),
                    ..TaskPatch::default()
                },
            )
            .await
            .expect("update"),
        ),
    );
    record(
        "list_runs_for_task",
        to_json(
            &runs::list_runs_for_task(f.ctx(), &task)
                .await
                .expect("runs"),
        ),
    );
    record(
        "list_runs",
        to_json(
            &runs::list_runs(f.ctx(), &f.paths, runs::RunFilter::default())
                .await
                .expect("runs"),
        ),
    );
    record(
        "get_run",
        to_json(
            &runs::get_run(f.ctx(), &f.paths, &run.id)
                .await
                .expect("run"),
        ),
    );

    let server = RimaiaServer::new(
        f.ctx().with_source(MutationSource::Mcp),
        testing::doctor::provider(),
        Some(testing::doctor::local_tools(f.harness.machine())),
    );
    let Json(view) = server
        .list_repositories()
        .await
        .unwrap_or_else(|error| panic!("{:?}", error.0));
    record("mcp list_repositories", to_json(&view));
    let Json(view) = server
        .get_task(Parameters(request::<GetTaskRequest>(
            json!({ "task_id": task }),
        )))
        .await
        .unwrap_or_else(|error| panic!("{:?}", error.0));
    record("mcp get_task", to_json(&view));
    let Json(view) = server
        .list_tasks(Parameters(request::<ListTasksRequest>(json!({}))))
        .await
        .unwrap_or_else(|error| panic!("{:?}", error.0));
    record("mcp list_tasks", to_json(&view));

    // Last, because it takes the card off the board.
    record(
        "archive_task",
        to_json(
            &tasks::archive_task(f.ctx(), Some(f.harness.machine()), &task)
                .await
                .expect("archive"),
        ),
    );

    let roots = f.roots();
    for (name, value) in &answers {
        let mut strings = Vec::new();
        walk_strings(value, &mut strings);
        for string in strings {
            assert!(
                !looks_absolute(&string),
                "{name} carries an absolute path: {string}"
            );
            for root in &roots {
                assert!(
                    !string.contains(root.as_str()),
                    "{name} carries the test's directory {root}: {string}"
                );
            }
        }
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("a DTO serializes")
}

fn walk_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(string) => out.push(string.clone()),
        Value::Array(items) => items.iter().for_each(|item| walk_strings(item, out)),
        Value::Object(fields) => fields.values().for_each(|field| walk_strings(field, out)),
        _ => {}
    }
}

/// `Path::is_absolute` on this platform, or a Windows drive prefix on any.
fn looks_absolute(string: &str) -> bool {
    let bytes = string.as_bytes();
    let windows_drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    Path::new(string).is_absolute() || windows_drive
}

// ---------------------------------------------------------------------------
// Worktree records
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_new_worktree_is_recorded_on_the_runner_and_not_the_board() {
    let f = Fixture::new().await;
    let task = f.task("Record me", "1. Branch").await;

    let prepared = f.harness.prepare_worktree(&task).await.expect("prepare");

    let record = f
        .harness
        .machine()
        .store
        .get_worktree(&task)
        .await
        .expect("read the machine store")
        .expect("the machine store records the worktree");
    assert_eq!(record.path, prepared.path);
    assert_eq!(record.repository_id, f.repository_id);
    assert!(Path::new(&record.path).is_dir());

    let (branch, worktree_path): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT branch, worktree_path FROM tasks WHERE id = ?1")
            .bind(&task)
            .fetch_one(&f.ctx().pool)
            .await
            .expect("read the board's row");
    assert_eq!(
        branch.as_deref(),
        Some(prepared.branch.as_str()),
        "the branch goes through BoardPort::record_branch"
    );
    assert_eq!(worktree_path, None, "the path never reaches the board");
}

#[tokio::test]
async fn a_new_worktree_writes_its_branch_through_the_board_port() {
    // `record_branch` is the only writer of the branch on this path: a port
    // that refuses it refuses the prepare, after the worktree is on disk.
    let f = Fixture::new().await;
    let task = f.task("Refused", "1. Branch").await;
    let lease = LeaseRef::new(task.clone(), 1, f.harness.solo.team_id.clone());
    let context = f
        .harness
        .board(&f.paths, &RunnerConfig::default())
        .preview(&task)
        .await
        .expect("preview");

    let error = worktree::prepare(
        f.harness.machine(),
        &testing::board::Unwired,
        &lease,
        &context,
    )
    .await
    .expect_err("the port refused the branch");

    assert_eq!(error.to_string(), "this test does not plan");
    assert_eq!(
        tasks::get_task(f.ctx(), &task)
            .await
            .expect("get")
            .task
            .branch,
        None
    );
}

// ---------------------------------------------------------------------------
// A repository with no checkout here
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_repository_not_set_up_on_this_computer_refuses_to_run() {
    let f = Fixture::new().await;
    let task = f.task("Elsewhere", "1. Run it").await;
    f.harness
        .machine()
        .store
        .remove_checkout(&f.repository_id)
        .await
        .expect("forget this machine's checkout");
    let refusal = format!("\"{}\" is not set up on this computer", f.repository_name);

    let cli = FakeCli::new();
    cli.replays(&task, "success", 0);
    let config = f.config(&cli);
    let board = f.harness.board(&f.paths, &config);
    let error = claim_manual_start(
        f.harness.starter(OwnerPresence::AtRunner),
        board.as_ref(),
        f.harness.machine(),
        &f.paths,
        &config,
        &InFlight::new(),
        ManualStart {
            task_id: task.clone(),
            continue_session: false,
        },
    )
    .await
    .err()
    .expect("Run now is refused");

    assert_eq!(error.to_string(), refusal);
    let detail = tasks::get_task(f.ctx(), &task).await.expect("get");
    assert_eq!(detail.task.run_state, RunState::Idle, "no claim");
    assert_eq!(detail.task.branch, None);
    assert_eq!(detail.last_run, None, "no runs row");
    assert_eq!(f.harness.worktree_path(&task).await, None, "no worktree");
    assert_eq!(cli.started(), Vec::<String>::new());

    let status = worktree::status(f.ctx(), f.harness.machine(), &task)
        .await
        .expect_err("get_worktree_status refuses too");
    assert_eq!(status.to_string(), refusal);
    let diff = worktree::diff_summary(f.ctx(), f.harness.machine(), &task)
        .await
        .expect_err("and so does get_diff_summary");
    assert_eq!(diff.to_string(), refusal);
}

// ---------------------------------------------------------------------------
// Consent
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn an_unattended_run_needs_this_runners_consent() {
    // The team ceiling at 1 and this runner's consent at 0: refused, and the
    // queue skips the task.
    let f = Fixture::new().await;
    let task = f.task("Consent", "1. Run it").await;
    f.set_ceiling(true).await;
    repo::set_allow_unattended_runs(f.ctx(), f.harness.machine(), &f.repository_id, false)
        .await
        .expect("withdraw the consent");

    let cli = FakeCli::new();
    cli.replays(&task, "success", 0);
    let config = f.config(&cli);
    let board = f.harness.board(&f.paths, &config);
    // Asked from away from the machine, for the trigger every recording
    // echoes: the fixture reports the permission mode a queued run asks for,
    // and a manual one would stop it as a posture nobody chose. The consent
    // check is the same for both.
    let start = || ManualStart {
        task_id: task.clone(),
        continue_session: false,
    };
    let in_flight = InFlight::new();
    let error = claim_manual_start(
        f.harness.starter(OwnerPresence::Remote),
        board.as_ref(),
        f.harness.machine(),
        &f.paths,
        &config,
        &in_flight,
        start(),
    )
    .await
    .err()
    .expect("Run now is refused without consent");
    assert!(
        error
            .to_string()
            .contains("has not enabled unattended agent runs"),
        "{error}"
    );
    let detail = tasks::get_task(f.ctx(), &task).await.expect("get");
    assert_eq!(
        detail.task.run_state,
        RunState::Idle,
        "no run state written"
    );
    assert_eq!(detail.last_run, None);

    let plan = queue_plan(&f, &in_flight).await;
    let entry = plan
        .iter()
        .find(|entry| entry.task_id == task)
        .expect("listed");
    assert_eq!(entry.skip, Some(SkipReason::UnattendedRunsNotAllowed));
    assert!(
        scheduler::next_batch(&plan, &in_flight.counts(), 4, &HashMap::new()).is_empty(),
        "the queue never claims it"
    );

    // The ceiling at 0 and the consent at 1: this task does not read the
    // ceiling, which is task 045's, so the run proceeds and the queue offers
    // the task.
    f.set_ceiling(false).await;
    repo::set_allow_unattended_runs(f.ctx(), f.harness.machine(), &f.repository_id, true)
        .await
        .expect("give the consent");

    let plan = queue_plan(&f, &in_flight).await;
    let entry = plan
        .iter()
        .find(|entry| entry.task_id == task)
        .expect("listed");
    assert_eq!(entry.skip, None);
    assert_eq!(entry.queue_position, Some(1));

    let started = claim_manual_start(
        f.harness.starter(OwnerPresence::Remote),
        board.as_ref(),
        f.harness.machine(),
        &f.paths,
        &config,
        &in_flight,
        start(),
    )
    .await
    .unwrap_or_else(|error| panic!("Run now proceeds with consent: {error}"));
    let run = tokio::time::timeout(
        TEST_TIMEOUT,
        run_task(
            board.as_ref(),
            f.harness.machine(),
            &f.paths,
            &config,
            started.claim,
            RunRequest {
                cancel: CancelSignal::new(),
                in_flight: None,
            },
        ),
    )
    .await
    .expect("the run finishes")
    .expect("the run succeeds");
    assert_eq!(
        run.exit_class,
        Some(rimaia_core::db::ExitClass::Success),
        "{:?}",
        run.error_message
    );
}

// ---------------------------------------------------------------------------
// D13: a branch ties a task to its repository
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_task_with_a_recorded_branch_cannot_change_repository() {
    let f = Fixture::new().await;
    let other = f.second_repository().await;
    let branched = f.task("Branched", "1. Work").await;
    sqlx::query("UPDATE tasks SET branch = 'rimaia/branched' WHERE id = ?1")
        .bind(&branched)
        .execute(&f.ctx().pool)
        .await
        .expect("record a branch");

    let error = tasks::update_task(
        f.ctx(),
        &branched,
        TaskPatch {
            repository_id: Some(other.clone()),
            ..TaskPatch::default()
        },
    )
    .await
    .expect_err("a recorded branch keeps it where it is");
    assert_eq!(
        error.to_string(),
        format!(
            "cannot move \"Branched\" to another repository: it already has a branch, \
             rimaia/branched, in {}",
            f.repository_name
        )
    );

    // A task with no branch and no runs is a title and a plan, and moves.
    let fresh = f.task("Misfiled", "1. Work").await;
    let moved = tasks::update_task(
        f.ctx(),
        &fresh,
        TaskPatch {
            repository_id: Some(other.clone()),
            ..TaskPatch::default()
        },
    )
    .await
    .expect("a fresh task moves");
    assert_eq!(moved.repository_id, other);
}

// ---------------------------------------------------------------------------
// Removal, and registering again
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_repository_whose_deleted_tasks_left_worktrees_can_be_removed_and_registered_again() {
    let f = Fixture::new().await;
    let task = f.task("Deleted later", "1. Work").await;
    let prepared = f.harness.prepare_worktree(&task).await.expect("prepare");
    tasks::delete_task(f.ctx(), &task)
        .await
        .expect("delete the task, which removes no worktree");
    assert!(
        f.harness.worktree_path(&task).await.is_some(),
        "the record outlives the task, as on real installs"
    );

    repo::remove(f.ctx(), Some(f.harness.machine()), &f.repository_id)
        .await
        .expect("the removal forgets the deleted task's record, then the checkout");

    let store = &f.harness.machine().store;
    assert_eq!(
        store.get_checkout(&f.repository_id).await.expect("read"),
        None
    );
    assert!(store
        .list_worktrees()
        .await
        .expect("read")
        .iter()
        .all(|record| record.repository_id != f.repository_id));
    assert!(
        Path::new(&prepared.path).exists(),
        "the directory stays on disk, as a deleted task's always has"
    );

    let again = repo::register(
        f.ctx(),
        f.harness.machine(),
        &f.paths.worktrees_dir(),
        NewRepository {
            path: f.source.path().to_string_lossy().into_owned(),
            name: None,
            worktree_root: None,
        },
    )
    .await
    .expect("the same clone registers again");
    assert_ne!(again.id, f.repository_id);
    assert_eq!(
        store.list_checkouts().await.expect("read").len(),
        1,
        "only the new checkout"
    );
}

// ---------------------------------------------------------------------------
// Log paths are derived
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn a_run_log_is_found_by_its_ids_not_its_column() {
    let f = Fixture::new().await;
    let task = f.task("Logged", "1. Run it").await;
    let cli = FakeCli::new();
    cli.replays(&task, "success", 0);
    let run = f.run(&cli, &task).await.expect("the run");
    let derived = transcript_path(&f.paths, &task, &run.id);
    assert!(derived.is_file(), "the run wrote its transcript");

    sqlx::query("UPDATE runs SET log_path = '/nowhere/at/all.jsonl' WHERE id = ?1")
        .bind(&run.id)
        .execute(&f.ctx().pool)
        .await
        .expect("point the column somewhere that does not exist");

    let page_path = runs::transcript_of(f.ctx(), &f.paths, &run.id)
        .await
        .expect("the transcript reads find the run");
    assert_eq!(page_path, derived);
    let page = transcript::read_page(&page_path, 0, 10)
        .await
        .expect("the page");
    assert!(!page.entries.is_empty(), "the page reads the derived file");
    transcript::search(&page_path, "result")
        .await
        .expect("the search reads the derived file");
    transcript::summarize(&page_path)
        .await
        .expect("the summary reads the derived file");
    assert_eq!(
        runs::log_path_to_reveal(f.ctx(), &f.paths, &run.id)
            .await
            .expect("reveal finds it"),
        derived
    );
    assert_eq!(
        runs::log_path(f.ctx(), &f.paths, &task, &run.id)
            .await
            .expect("get_run_log_path derives it"),
        derived
    );
    assert!(
        runs::get_run(f.ctx(), &f.paths, &run.id)
            .await
            .expect("get_run")
            .log_available
    );

    let pruned = runs::prune_logs(f.ctx(), &f.paths, PruneCriterion::Task(task.clone()))
        .await
        .expect("prune");
    assert_eq!(pruned.runs_pruned, 1);
    assert!(
        !derived.exists(),
        "prune removed the file at the derived path"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_run_log_path_is_refused_for_another_tasks_run() {
    let f = Fixture::new().await;
    let task = f.task("Logged", "1. Run it").await;
    let other = f.task("Not that one", "1. Wait").await;
    let cli = FakeCli::new();
    cli.replays(&task, "success", 0);
    let run = f.run(&cli, &task).await.expect("the run");

    let error = runs::log_path(f.ctx(), &f.paths, &other, &run.id)
        .await
        .expect_err("the run is not that task's");
    assert_eq!(error.to_string(), format!("no run with id {}", run.id));
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

struct Fixture {
    harness: TestContext,
    source: TempRepo,
    data: TempDir,
    paths: AppPaths,
    repository_id: String,
    repository_name: String,
}

impl Fixture {
    async fn new() -> Self {
        let harness = TestContext::new().await;
        let source = TempRepo::init();
        let data = tempfile::Builder::new()
            .prefix("rimaia-checkouts-")
            .tempdir()
            .expect("a temp data directory");
        let paths = AppPaths::new(data.path());
        paths.create_all().expect("the app data directories");
        let registered = repo::register(
            &harness.context,
            harness.machine(),
            &paths.worktrees_dir(),
            NewRepository {
                path: source.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register the clone");
        repo::set_allow_unattended_runs(&harness.context, harness.machine(), &registered.id, true)
            .await
            .expect("this runner's consent");

        Self {
            harness,
            source,
            data,
            paths,
            repository_id: registered.id,
            repository_name: registered.name,
        }
    }

    fn ctx(&self) -> &ServiceContext {
        &self.harness.context
    }

    fn config(&self, cli: &FakeCli) -> RunnerConfig {
        RunnerConfig {
            program: cli.program(),
            ..RunnerConfig::default()
        }
    }

    async fn task(&self, title: &str, plan: &str) -> String {
        tasks::create_task(
            self.ctx(),
            NewTask {
                repository_id: self.repository_id.clone(),
                title: title.to_string(),
                plan: Some(plan.to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a task")
        .id
    }

    /// A second repository on the board, with nothing on disk behind it.
    async fn second_repository(&self) -> String {
        let id = rimaia_core::db::new_id();
        sqlx::query(
            "INSERT INTO repositories (id, team_id, name, default_branch, created_at)
             VALUES (?1, ?2, 'elsewhere', 'main', ?3)",
        )
        .bind(&id)
        .bind(&self.harness.solo.team_id)
        .bind(testing::test_epoch())
        .execute(&self.ctx().pool)
        .await
        .expect("seed a repository");
        id
    }

    /// Sets the board's team ceiling, which nothing reads before task 045.
    async fn set_ceiling(&self, allow: bool) {
        sqlx::query("UPDATE repositories SET allow_unattended_runs = ?1 WHERE id = ?2")
            .bind(allow)
            .bind(&self.repository_id)
            .execute(&self.ctx().pool)
            .await
            .expect("set the ceiling");
    }

    async fn run(&self, cli: &FakeCli, task_id: &str) -> rimaia_core::Result<rimaia_core::db::Run> {
        let config = self.config(cli);
        let board = self.harness.board(&self.paths, &config);
        let claim = claim_run(board.as_ref(), task_id, RunTrigger::Queued, false).await?;
        tokio::time::timeout(
            TEST_TIMEOUT,
            run_task(
                board.as_ref(),
                self.harness.machine(),
                &self.paths,
                &config,
                claim,
                RunRequest {
                    cancel: CancelSignal::new(),
                    in_flight: None,
                },
            ),
        )
        .await
        .expect("a run must finish inside the test timeout")
    }

    /// Every directory this test made, raw and canonicalized: no board DTO
    /// may carry any of them.
    fn roots(&self) -> Vec<String> {
        let mut roots = Vec::new();
        for dir in [self.data.path(), self.source.path()] {
            roots.push(dir.to_string_lossy().into_owned());
            if let Ok(canonical) = std::fs::canonicalize(dir) {
                roots.push(canonical.to_string_lossy().into_owned());
            }
        }
        roots
    }
}

/// The request an agent would send, deserialized through the real schema.
fn request<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("a well-formed request deserializes")
}

/// The plan the runner loop's status shows and its `Next` claims act on: the
/// board's plan over the repositories `scheduler::view::for_runner` lists
/// (task 042). The loop itself is `rimaia_runner`'s, which a core test cannot
/// name.
async fn queue_plan(f: &Fixture, in_flight: &InFlight) -> Vec<scheduler::QueueEntry> {
    let (repositories, _) = scheduler::for_runner(f.harness.machine(), in_flight)
        .await
        .expect("the runner's view");
    let runner = scheduler::RunnerView::new(
        f.harness.solo.runner_id.clone(),
        rimaia_core::runner::provider::ProviderId::ClaudeCode,
        repositories,
    );
    scheduler::plan(f.ctx(), &runner).await.expect("the plan")
}
