# 3. SQLite as the local store, schema owned by Rust

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

Rimaia's state is small but relational and long-lived: tasks, their ordering, a dependency
graph, repositories, links, runs, run attempts, schedules. Three independent writers touch
it — the UI (via Tauri commands), the MCP server (via other Claude Code sessions), and the
run scheduler (background tasks) — sometimes at the same moment.

Requirements:

- Survives app restarts and crashes mid-run.
- Supports queries the UI needs directly: ordered column contents, blocked-task
  resolution, run history per task.
- Handles concurrent writers from several async tasks in one process.
- Zero setup for the user.

## Decision

Use **SQLite** as the single source of truth, accessed from Rust via **`sqlx`** with the
async SQLite driver.

- The database file lives in the platform application data directory
  (`~/Library/Application Support/dev.rimaia.app/rimaia.db` on macOS), not in any user
  repository.
- Schema is defined by versioned migration files under `src-tauri/migrations/`, applied at
  startup. Migrations are append-only; never edit a migration that has shipped.
- Rust owns the schema. The frontend never sees SQL — it calls Tauri commands that return
  serde-serialized DTOs.
- `WAL` journal mode, `foreign_keys = ON`, and `busy_timeout` set at connection setup. One
  shared `SqlitePool` for the whole process.
- Long-form run transcripts are **not** stored in the database; they are JSONL files on
  disk with the path recorded in the `runs` row (see ADR-0013).

### Core tables

| Table | Purpose |
| --- | --- |
| `repositories` | Registered local git repos: path, default branch, worktree root |
| `settings` | Key/value app settings, including the global base instructions |
| `tasks` | Title, plan, extra instructions, column, position, run state, branch |
| `task_links` | Zero-or-more external references per task (label + URL) |
| `task_dependencies` | Edges: `task_id` is blocked by `depends_on_task_id` |
| `runs` | One row per attempt: status, session id, timings, exit reason, log path |
| `schedules` | Named run configurations: mode, start time, concurrency |

## Consequences

- Transactions give us correctness for the operations that matter: reordering a column,
  claiming the next task off the queue, recording a run transition.
- Because `sqlx` is async, the scheduler, the Tauri command handlers, and the MCP server
  all share one pool on the Tokio runtime without a blocking bridge.
- Compile-time-checked queries (`sqlx::query!`) require a live database or a checked-in
  `.sqlx` offline cache. We check in the offline cache so builds work without a database.
- The user can inspect and repair state with any SQLite tool. Useful during development;
  worth remembering when designing invariants — they must be enforced in code, not
  assumed from the writer.
- SQLite's single-writer model means write contention shows up as `SQLITE_BUSY` under
  load. `busy_timeout` plus short transactions handles our volume (one user, tens of
  tasks); we are not designing for more.

## Alternatives considered

- **`rusqlite`.** Simpler and synchronous, but every call from async code needs
  `spawn_blocking`, which spreads through the codebase.
- **`tauri-plugin-sql` (frontend-driven SQL).** Puts the schema and its invariants in the
  view layer, where the scheduler and MCP server cannot reach them. Wrong ownership.
- **JSON or TOML files.** No transactions, no ordered queries, and three concurrent
  writers make corruption a question of when.
- **An embedded key-value store (sled, redb).** Loses relational queries the UI needs and
  gains nothing for this data size.
