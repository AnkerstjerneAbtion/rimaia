//! The typed rows, and the enums they hold (ADR-0003).
//!
//! One struct per table in `src-tauri/migrations/20260820120000_initial_schema.sql`,
//! column for column. **That file is the source of truth.** Every enum below
//! spells exactly the domain its `CHECK` names, and SQLite can neither drop nor
//! widen a `CHECK` — so a variant added here without a rename-copy-drop table
//! rebuild is a runtime constraint failure at the `INSERT`, with nothing to warn
//! about it at compile time. Change the schema first, or do not change the enum.
//!
//! Rows serialize but do not deserialize. A row is what the store hands *out*; it
//! is never what a caller hands in, because a caller supplies a subset with its
//! own optionality — task 004's patch types are their own shape. The enums do
//! deserialize, because they genuinely are inputs: `move_task(column:
//! BoardColumn)` takes one off the wire.
//!
//! The wire shape is `camelCase` keys with `snake_case` values, matching `AppInfo`
//! and [`ErrorCode`](crate::ErrorCode) and mirrored by hand in `src/types.ts`. The
//! keys answer to TypeScript; the values answer to SQLite's `CHECK` constraints as
//! well, which is why they are not camelCase too.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A new identifier for a row in any table.
///
/// One helper rather than a `Uuid::new_v4().to_string()` at each insert site, so
/// the shape is stated once: every id column is `TEXT` holding a hyphenated UUID,
/// and never the `uuid::Uuid` *type*, which sqlx maps to `BLOB` on SQLite —
/// aliasing a TEXT column as `Uuid` compiles and then fails at runtime on a
/// 36-byte slice, and storing real blobs would make the file unreadable in the
/// `sqlite3` CLI that ADR-0003 counts as a feature (seam-contract D10).
pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// Where a card is in the *user's* process (ADR-0007).
///
/// Four, and there is no fifth: running, failed and blocked are badges drawn from
/// [`RunState`], not places on the board. Two dimensions, two fields — a card that
/// failed twice and is waiting on a usage-limit reset is still "ready to be
/// implemented", so it stays in [`Ready`](BoardColumn::Ready) and the failure
/// shows on it.
///
/// Only `ready` feeds the run queue; `in_review` is where the morning review
/// starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum BoardColumn {
    NotReady,
    Ready,
    InReview,
    Done,
}

/// Where a task is in the *machine's* process (ADR-0007).
///
/// ADR-0007's seven, and only these seven. **`interrupted` is deliberately absent,
/// and adding it here is a bug rather than a fix.** A run that died with the app
/// carries that word on its own row, as [`RunStatus::Interrupted`] and
/// [`ExitClass::Interrupted`]; the task it belonged to lands on
/// [`Failed`](RunState::Failed) and stays in [`BoardColumn::Ready`], per
/// ADR-0007's failure rule. The card reads the word off its last run, not off its
/// own state (seam-contract D9). ADR-0007's list and task 005's badge list omit it
/// independently, which is a decision and not an oversight — and the schema's
/// `CHECK` now makes it permanent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Idle,
    Queued,
    Running,
    Blocked,
    WaitingRetry,
    Failed,
    Cancelled,
}

/// Why a run stopped (ADR-0011).
///
/// A closed set of six. The classifier maps a terminal event onto one of these and
/// never invents a seventh, because a termination it does not recognise is
/// [`Transient`](ExitClass::Transient) — retrying a fatal error a few times is
/// cheaper than abandoning a recoverable one, and a Claude Code update must not
/// break a queue. Classification reads `result.terminal_reason` together with
/// `subtype`, never the exit code alone: a SIGTERM-killed run still emits a
/// `result` and exits 143.
///
/// What each class *does* — wait for the reset, back off, resume once, stop — is
/// retry policy, and lives with the classifier (task 014) rather than with the
/// value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ExitClass {
    Success,
    UsageLimit,
    Transient,
    Interrupted,
    Fatal,
    Cancelled,
}

/// The coarse lifecycle of one attempt, as the Runs view queries it (ADR-0013).
///
/// `running` plus one value per terminal outcome, which collapses [`ExitClass`]'s
/// six onto four. `usage_limit`, `transient` and `fatal` all land on
/// [`Failed`](RunStatus::Failed), because whether a failure will be retried is the
/// *task's* business ([`RunState::WaitingRetry`]) and not this row's — the attempt
/// itself is over either way, and the class is still on the row for anyone who
/// wants the distinction. [`Interrupted`](RunStatus::Interrupted) is its own value
/// rather than a flavour of failure because seam-contract D9 puts that word on the
/// run, and startup reconciliation marks runs a crash left `running` with it and
/// offers them for resume (ADR-0010).
///
/// There is no `queued`: a `runs` row exists only once a process was spawned for
/// it, and what is waiting to start is a [`RunState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

/// How a task's model and effort get chosen (ADR-0016).
///
/// Not [`ScheduleMode`], which is ADR-0010's sequential-or-parallel run
/// configuration. The two share the word "mode" and nothing else.
///
/// [`Default`](StrategyMode::Default) is a decision, not an absence. The column is
/// `NOT NULL DEFAULT 'default'` precisely so that a NULL `model` or `effort` means
/// "not set" and does not also have to mean "explicitly set to the default".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum StrategyMode {
    Default,
    Manual,
    Planned,
}

/// Who last wrote a task's strategy (ADR-0016), so a bad morning result can be
/// traced to the proposal that produced it and not only to the implementation.
///
/// The column is nullable and this enum has no third `unset` variant: a task
/// nobody has decided anything about is `None`, which the type then forces every
/// reader to handle rather than letting one sentinel value drift into meaning two
/// things.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum StrategySource {
    User,
    Planner,
}

/// How many runs a schedule allows at once (ADR-0010).
///
/// Not [`StrategyMode`], which is ADR-0016's per-task model and effort selection.
/// The two share the word "mode" and nothing else.
///
/// Concurrency is a property of the run configuration and never of a task, which
/// is why [`Parallel`](ScheduleMode::Parallel) carries no number of its own and
/// [`Schedule::max_concurrency`] does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ScheduleMode {
    Sequential,
    Parallel,
}

/// Which door a mutation came through (ADR-0019).
///
/// Three values because there are three writers of these tables, and each is a
/// subsystem rather than a caller: the UI through the Tauri commands, an MCP
/// tool call from a Claude Code session elsewhere on this machine (ADR-0006),
/// and the run scheduler working the board (ADR-0010). It travels on
/// [`ServiceContext`](crate::ServiceContext) rather than as a parameter for the
/// reason ADR-0018 gives about publishing: being the scheduler is what a
/// subsystem *is*, not something each of its call sites decides.
///
/// On a `tasks` row this is **creation provenance and is never rewritten** — see
/// [`Task::source`]. ADR-0006's "every mutation is attributed" is carried by the
/// tracing span on each mutating service function, not by the column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MutationSource {
    Ui,
    Mcp,
    System,
}

impl MutationSource {
    /// The stored spelling, which is also what the tracing span records — one
    /// string, so a `source` in the log file and a `source` in the sqlite3 CLI
    /// are the same word.
    pub const fn as_str(self) -> &'static str {
        match self {
            MutationSource::Ui => "ui",
            MutationSource::Mcp => "mcp",
            MutationSource::System => "system",
        }
    }
}

/// A registered local git repository (ADR-0005).
///
/// The repository on disk is authoritative: this row records what Rimaia was told,
/// and startup reconciliation trusts the filesystem where the two disagree. Which
/// is also why there is no `remote_url` — `git remote get-url` answers it every
/// time and a cached copy can only go stale.
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Repository {
    pub id: String,
    pub name: String,
    /// `String`, not `PathBuf`: the column is `TEXT`, it is a string again the
    /// moment it crosses to the frontend, and callers that touch the filesystem
    /// take a `Path::new` on it.
    pub path: String,
    pub default_branch: String,
    pub worktree_root: String,
    /// ADR-0012's per-repository opt-in to `--permission-mode bypassPermissions`.
    /// Never widened without amending that ADR.
    pub allow_unattended_runs: bool,
    pub created_at: DateTime<Utc>,
}

/// One key/value application setting (ADR-0003).
///
/// Untyped on purpose. Which keys exist, what each holds and what happens when one
/// is absent are business rules, and they belong to the typed accessor task 006
/// ships (seam-contract D3). This is the row and nothing more.
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Setting {
    pub key: String,
    pub value: String,
}

/// The unit of handoff from planning to execution (ADR-0007).
///
/// It carries everything an agent needs to work with no further conversation, and
/// everything the user needs to review the result the next morning.
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub repository_id: String,
    pub title: String,
    /// `None` is precisely `not_ready`'s "captured, plan missing or incomplete".
    /// An empty string would be a second way to spell one state.
    pub plan: Option<String>,
    pub extra_instructions: Option<String>,
    /// The one field whose Rust and SQL names differ: `column` is a SQL keyword,
    /// so the table spells it `board_column`. Only the `FromRow` path needs
    /// telling — `rename_all = "camelCase"` leaves a single-word field alone, so
    /// the key the frontend sees is already `column`.
    #[sqlx(rename = "board_column")]
    pub column: BoardColumn,
    /// Fractional priority within `(repository_id, column)`: inserting between two
    /// cards takes the midpoint and rewrites no neighbours. Board order *is*
    /// execution order — there is no separate priority field (ADR-0007), and the
    /// arithmetic lives in [`crate::tasks`] rather than on this struct.
    pub position: f64,
    pub run_state: RunState,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub strategy_mode: StrategyMode,
    /// Free text, not an enum. ADR-0016 populates both dropdowns from
    /// configuration because models ship faster than releases do, so a closed set
    /// here would be a release blocker the first time Anthropic names something
    /// new.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The planner's proposal as opaque JSON text — phases, per-phase model and
    /// effort, agent counts, rationale (ADR-0016). Opaque because the workspace's
    /// `sqlx` is `default-features = false` without the `json` feature, so neither
    /// `sqlx::types::Json` nor `#[sqlx(json)]` exists here. Task 020 is the first
    /// code to look inside it; it either parses this with `serde_json` or turns
    /// that feature on, and either way this stays `TEXT` in the database.
    pub strategy_plan: Option<String>,
    pub strategy_source: Option<StrategySource>,
    pub strategy_updated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Which door created this task (ADR-0019). Written once, by
    /// [`crate::tasks::create_task`], and never updated — a patch over MCP
    /// leaves a `ui` task saying `ui`. Last, after `updated_at`, matching the
    /// column order the migration produced.
    pub source: MutationSource,
}

/// One external reference on a task — an Asana task, a GitHub issue, a doc
/// (ADR-0007).
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct TaskLink {
    pub id: String,
    pub task_id: String,
    pub label: String,
    pub url: String,
    /// Fractional for the same reason [`Task::position`] is: reordering must not
    /// rewrite the rows around it.
    pub position: f64,
}

/// One edge: `task_id` is blocked by `depends_on_task_id` (ADR-0008).
///
/// A dependency is satisfied when the run it names **succeeds**, not when a human
/// marks it done. That is deliberate and load-bearing; nothing reads this table
/// until task 011, and it is modelled now because migrations are append-only.
///
/// Both ends are plain `String` and therefore interchangeable in a signature.
/// Newtypes would catch exactly that mistake and were weighed and declined, at the
/// price of a type override on every id column in every `query_as!`
/// (seam-contract D10) — recorded so a later reader does not read it as an
/// oversight.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct TaskDependency {
    pub task_id: String,
    pub depends_on_task_id: String,
}

/// One attempt (ADR-0011), holding only what the UI queries.
///
/// The event stream itself is a JSONL file at [`log_path`](Run::log_path), which is
/// how ADR-0013 keeps megabytes of transcript out of every board query.
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub task_id: String,
    /// Unique within the task, enforced by an index rather than assumed: two
    /// writers racing to claim a task must not both record attempt 3.
    pub attempt: i64,
    pub status: RunStatus,
    /// Generated by Rimaia *before* the spawn, so `--resume` works even if the
    /// process dies before its `init` event (ADR-0004). Every attempt of one task
    /// shares it, which is what makes the retries resume rather than restart. A
    /// `String`, never a `Uuid`, for the reason [`new_id`] gives.
    pub session_id: String,
    /// The composed prompt verbatim (ADR-0009), so a morning review sees what the
    /// agent was actually asked rather than what it would be asked today.
    pub prompt: String,
    pub started_at: DateTime<Utc>,
    /// `ended_at` through `cost_usd` are `None` while the run is in flight.
    pub ended_at: Option<DateTime<Utc>>,
    pub exit_class: Option<ExitClass>,
    pub error_message: Option<String>,
    /// Both arrive on the terminal `result` event; neither is derived.
    pub num_turns: Option<i64>,
    pub cost_usd: Option<f64>,
    /// Known at row creation, being a pure function of the task and run ids
    /// (ADR-0013). A row whose file has vanished is marked at startup rather than
    /// trusted — a reconciliation rule, not a reason for this to be optional.
    pub log_path: String,
    pub pr_url: Option<String>,
    /// When the next attempt may start: the usage-limit reset plus jitter, or the
    /// current backoff step (ADR-0011).
    pub resume_after: Option<DateTime<Utc>>,
}

/// A named run configuration (ADR-0010).
///
/// Mode and concurrency are properties of the configuration, never of a task.
/// Nothing reads this table until task 013; it is modelled now for the same reason
/// [`TaskDependency`] is.
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub id: String,
    pub name: String,
    pub mode: ScheduleMode,
    /// A cron expression with a timezone, or a wall-clock time, or neither for
    /// "run now". Which combinations are legal is task 013's design, and is
    /// deliberately unconstrained here as it is in the schema.
    pub cron: Option<String>,
    pub start_at: Option<DateTime<Utc>>,
    /// Read only in [`ScheduleMode::Parallel`]. ADR-0010 caps *per-repository*
    /// concurrency at 1 regardless of this number, because two agents in one repo
    /// fight over ports, test databases and lockfiles.
    pub max_concurrency: i64,
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde::de::DeserializeOwned;
    use serde_json::json;
    use sqlx::encode::IsNull;
    use sqlx::sqlite::{Sqlite, SqliteArgumentValue};
    use sqlx::Encode;

    /// The text serde puts on the wire for one enum value.
    fn on_the_wire<T: Serialize>(value: T) -> String {
        match serde_json::to_value(value).expect("an enum must always serialize") {
            serde_json::Value::String(text) => text,
            other => panic!("an enum must serialize to a JSON string, got {other}"),
        }
    }

    /// The text sqlx hands SQLite for the same value — the half of the agreement
    /// serde cannot see.
    ///
    /// Still a pure test: `Encode` fills an argument buffer, and nothing here binds
    /// it to a statement or opens a database. What it catches is the one realistic
    /// drift, a variant given a `#[serde(rename)]` and no `#[sqlx(rename)]` or the
    /// reverse, which every database-side test would also catch but only after
    /// somebody wrote a query against the new variant.
    fn as_sqlite_text<'q, T: Encode<'q, Sqlite>>(value: T) -> String {
        let mut buffer = Vec::new();
        let is_null = value
            .encode(&mut buffer)
            .expect("a strong enum encodes without failing");
        assert!(
            matches!(is_null, IsNull::No),
            "a non-optional enum must never encode as NULL — the columns are NOT NULL"
        );
        match buffer.pop() {
            Some(SqliteArgumentValue::Text(text)) => text.into_owned(),
            other => panic!("a strong enum must encode as TEXT, got {other:?}"),
        }
    }

    /// Asserts the three-way agreement for one variant: serde, sqlx and the
    /// migration's `CHECK` are one string, and serde reads it back unchanged.
    ///
    /// `expected` is copied by hand out of
    /// `src-tauri/migrations/20260820120000_initial_schema.sql`, and that
    /// duplication *is* the test. SQLite cannot widen a `CHECK`, so a value this
    /// file spells differently from that one fails at the `INSERT` with nothing
    /// upstream to warn about it.
    fn agrees_with_the_schema<'q, T>(value: T, expected: &str)
    where
        T: Serialize + DeserializeOwned + Encode<'q, Sqlite> + Copy + std::fmt::Debug + PartialEq,
    {
        assert_eq!(on_the_wire(value), expected, "what serde writes");
        assert_eq!(as_sqlite_text(value), expected, "what sqlx writes");
        assert_eq!(
            serde_json::from_value::<T>(json!(expected)).expect("the wire form must parse back"),
            value,
            "what serde reads back"
        );
    }

    #[test]
    fn board_columns_round_trip_through_their_schema_spelling() {
        agrees_with_the_schema(BoardColumn::NotReady, "not_ready");
        agrees_with_the_schema(BoardColumn::Ready, "ready");
        agrees_with_the_schema(BoardColumn::InReview, "in_review");
        agrees_with_the_schema(BoardColumn::Done, "done");
    }

    #[test]
    fn run_states_round_trip_through_their_schema_spelling() {
        agrees_with_the_schema(RunState::Idle, "idle");
        agrees_with_the_schema(RunState::Queued, "queued");
        agrees_with_the_schema(RunState::Running, "running");
        agrees_with_the_schema(RunState::Blocked, "blocked");
        agrees_with_the_schema(RunState::WaitingRetry, "waiting_retry");
        agrees_with_the_schema(RunState::Failed, "failed");
        agrees_with_the_schema(RunState::Cancelled, "cancelled");
    }

    #[test]
    fn exit_classes_round_trip_through_their_schema_spelling() {
        agrees_with_the_schema(ExitClass::Success, "success");
        agrees_with_the_schema(ExitClass::UsageLimit, "usage_limit");
        agrees_with_the_schema(ExitClass::Transient, "transient");
        agrees_with_the_schema(ExitClass::Interrupted, "interrupted");
        agrees_with_the_schema(ExitClass::Fatal, "fatal");
        agrees_with_the_schema(ExitClass::Cancelled, "cancelled");
    }

    #[test]
    fn run_statuses_round_trip_through_their_schema_spelling() {
        agrees_with_the_schema(RunStatus::Running, "running");
        agrees_with_the_schema(RunStatus::Succeeded, "succeeded");
        agrees_with_the_schema(RunStatus::Failed, "failed");
        agrees_with_the_schema(RunStatus::Cancelled, "cancelled");
        agrees_with_the_schema(RunStatus::Interrupted, "interrupted");
    }

    #[test]
    fn strategy_modes_and_sources_round_trip_through_their_schema_spelling() {
        agrees_with_the_schema(StrategyMode::Default, "default");
        agrees_with_the_schema(StrategyMode::Manual, "manual");
        agrees_with_the_schema(StrategyMode::Planned, "planned");
        agrees_with_the_schema(StrategySource::User, "user");
        agrees_with_the_schema(StrategySource::Planner, "planner");
    }

    #[test]
    fn schedule_modes_round_trip_through_their_schema_spelling() {
        agrees_with_the_schema(ScheduleMode::Sequential, "sequential");
        agrees_with_the_schema(ScheduleMode::Parallel, "parallel");
    }

    #[test]
    fn interrupted_is_a_run_outcome_and_never_a_task_run_state() {
        // seam-contract D9, asserted from both sides so the next agent to reach
        // for the "missing" variant finds the reason rather than the gap. The
        // schema's CHECK refuses it too; this refuses it a layer earlier.
        assert!(
            serde_json::from_value::<RunState>(json!("interrupted")).is_err(),
            "`interrupted` belongs to the run, not the task"
        );
        agrees_with_the_schema(RunStatus::Interrupted, "interrupted");
        agrees_with_the_schema(ExitClass::Interrupted, "interrupted");
    }

    #[test]
    fn a_task_serializes_with_camel_case_keys_and_column_left_alone() {
        let task = Task {
            id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
            repository_id: "3f2b1c00-0000-4000-8000-000000000002".to_string(),
            title: "Wire the board to the store".to_string(),
            plan: Some("## Steps\n1. ...".to_string()),
            extra_instructions: None,
            column: BoardColumn::InReview,
            position: 1.5,
            run_state: RunState::WaitingRetry,
            branch: Some("rimaia/wire-the-board".to_string()),
            worktree_path: None,
            strategy_mode: StrategyMode::Planned,
            model: Some("opus".to_string()),
            effort: None,
            strategy_plan: Some(r#"{"phases":[]}"#.to_string()),
            strategy_source: Some(StrategySource::Planner),
            strategy_updated_at: None,
            created_at: timestamp("2026-08-20T12:00:00Z"),
            updated_at: timestamp("2026-08-20T12:30:00Z"),
            source: MutationSource::Mcp,
        };

        assert_eq!(
            serde_json::to_value(&task).expect("a row must always serialize"),
            json!({
                "id": "3f2b1c00-0000-4000-8000-000000000001",
                "repositoryId": "3f2b1c00-0000-4000-8000-000000000002",
                "title": "Wire the board to the store",
                "plan": "## Steps\n1. ...",
                "extraInstructions": null,
                // Not `boardColumn`: the SQL name never reaches the frontend, and
                // a single-word field is untouched by `rename_all`.
                "column": "in_review",
                "position": 1.5,
                "runState": "waiting_retry",
                "branch": "rimaia/wire-the-board",
                "worktreePath": null,
                "strategyMode": "planned",
                "model": "opus",
                "effort": null,
                // Opaque text on the way through, not a nested object.
                "strategyPlan": "{\"phases\":[]}",
                "strategySource": "planner",
                "strategyUpdatedAt": null,
                "createdAt": "2026-08-20T12:00:00Z",
                "updatedAt": "2026-08-20T12:30:00Z",
                // ADR-0019's provenance, `snake_case` on the wire like every
                // other enum value, because it answers to SQLite's CHECK too.
                "source": "mcp",
            })
        );
    }

    #[test]
    fn a_repository_serializes_its_booleans_and_timestamps_for_the_frontend() {
        let repository = Repository {
            id: "3f2b1c00-0000-4000-8000-000000000003".to_string(),
            name: "rimaia".to_string(),
            path: "/Users/someone/Code/My Projects/rimaia".to_string(),
            default_branch: "main".to_string(),
            worktree_root: "/Users/someone/Library/Application Support/com.rimaia.app/worktrees"
                .to_string(),
            allow_unattended_runs: true,
            created_at: timestamp("2026-08-20T12:00:00Z"),
        };

        assert_eq!(
            serde_json::to_value(&repository).expect("a row must always serialize"),
            json!({
                "id": "3f2b1c00-0000-4000-8000-000000000003",
                "name": "rimaia",
                "path": "/Users/someone/Code/My Projects/rimaia",
                "defaultBranch": "main",
                "worktreeRoot": "/Users/someone/Library/Application Support/com.rimaia.app/worktrees",
                "allowUnattendedRuns": true,
                // RFC 3339 UTC, which is byte-for-byte what the TEXT column holds.
                "createdAt": "2026-08-20T12:00:00Z",
            })
        );
    }

    #[test]
    fn new_id_produces_a_distinct_hyphenated_uuid_each_time() {
        // The shape matters as much as the uniqueness: it is what makes the
        // database file legible in the sqlite3 CLI (seam-contract D10).
        let first = new_id();
        let second = new_id();

        assert_ne!(first, second);
        assert_eq!(first.len(), 36);
        assert!(Uuid::parse_str(&first).is_ok(), "{first} is not a UUID");
    }

    fn timestamp(rfc3339: &str) -> DateTime<Utc> {
        rfc3339.parse().expect("a literal timestamp must parse")
    }
}
