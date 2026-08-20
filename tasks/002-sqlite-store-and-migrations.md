---
id: "002"
title: SQLite store and migrations
milestone: mvp
status: ready
depends_on: ["001"]
adrs: ["0003", "0007"]
size: M
---

# SQLite store and migrations

## Goal

The complete persistence layer: schema, migrations, connection pool, and typed models —
including the tables that later milestones need, so subsequent work is additive rather
than a migration chain.

## Why now

Everything from task 003 onward reads or writes this.

## Scope

- `sqlx` migrations in `src-tauri/migrations/`, applied at startup before the window
  opens. Startup fails loudly on migration error.
- Pool configured with `WAL`, `foreign_keys = ON`, `busy_timeout`, `synchronous = NORMAL`.
- Checked-in `.sqlx` offline query cache so the build does not require a live database.
- Schema (see ADR-0003 and ADR-0007):

  - `repositories` — id, name, path, default_branch, worktree_root,
    allow_unattended_runs, created_at
  - `settings` — key, value (holds `base_instructions`, scheduler config)
  - `tasks` — id, repository_id, title, plan, extra_instructions, column, position (REAL),
    run_state, branch, worktree_path, model, effort, created_at, updated_at
  - `task_links` — id, task_id, label, url, position
  - `task_dependencies` — task_id, depends_on_task_id (composite PK, both FKs)
  - `runs` — id, task_id, attempt, status, session_id, prompt, started_at, ended_at,
    exit_class, error_message, num_turns, cost_usd, log_path, pr_url, resume_after
  - `schedules` — id, name, mode, cron, start_at, max_concurrency, enabled

- Indices on the queries that will actually run: `(column, position)` per repository,
  `runs(task_id, attempt)`, `tasks(run_state)`.
- Typed Rust models with `serde` derives, and enums for `column`, `run_state`,
  `exit_class`, `mode` — stored as text, parsed on read, never raw strings in business
  logic.
- Fractional position helpers: `position_between(before, after)` and a rebalance routine
  when the gap gets too small to represent.
- Startup reconciliation hooks (stubs at this stage): tasks left `running`, worktree paths
  that no longer exist, `runs` rows whose log file is missing.

## Out of scope

- Business logic. This task is storage plus models only.

## Acceptance criteria

- Fresh launch creates the database and applies all migrations.
- Second launch is a no-op.
- Integration tests over an in-memory database cover: insert/read for every table, foreign
  key enforcement, cascade behaviour on task delete, `position_between` including the
  rebalance path.
- `cargo sqlx prepare --check` passes in CI-equivalent conditions (no live database).

## Notes

Write the whole schema now, including `task_dependencies` and `schedules`, even though
nothing reads them until v0.2. Migrations are append-only once shipped; guessing right
here is cheaper than a migration chain later.
