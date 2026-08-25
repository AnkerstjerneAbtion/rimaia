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

---

## Amendment, 2026-08-20 — corrected against the crate split and the shipped identifier

The decision stands unchanged: SQLite, `sqlx`, one pool, schema owned by Rust. This ADR
predates ADR-0015's crate split and a bundle-identifier fix, and two things it states as
fact are wrong, a third it never says at all.

### The macOS path was an illustration, not a spec, and read as the latter

`~/Library/Application Support/dev.rimaia.app/rimaia.db` is wrong on two counts, one
cosmetic and one structural. The shipped identifier (`src-tauri/tauri.conf.json`) is
`com.rimaia.app`, not `dev.rimaia.app` — a stale value from before it was fixed. More to
the point, that path was never a string Rimaia formats. `crates/core/src/paths.rs` says why
in its own module doc: core *derives* paths, it does not discover them, because the
OS-specific lookup would need a `tauri` dependency in a crate that must not have one
(ADR-0015). The shell resolves the platform directory exactly once, in
`src-tauri/src/lib.rs`'s `setup()`, via Tauri's `app_data_dir()`, and hands it to
`AppPaths::new()`; every other path — the database file included — is `data_dir.join(...)`
from there. `AppPaths::db_file()` is the definition now; the path in the Decision section
was always meant to illustrate what that resolves to on one platform, not to specify it.

### Migrations: the crate split moved the code, not the files, and that is deliberate

ADR-0015 moved persistence into `crates/core/`. Both halves of the original claim are true
at once, and this ADR left the seam unstated. The pool, the migrator, the models and every
query are `rimaia-core`, under `crates/core/src/db/`. The migration **files** stay at
`src-tauri/migrations/` — not an oversight this ADR failed to update, but where they belong:
they are what the application ships and what `sqlx-cli` is pointed at, so shell tooling and
packaging find them without a crate-relative detour. `crates/core` embeds them at compile
time with `sqlx::migrate!("../../src-tauri/migrations")`, a path relative to
`CARGO_MANIFEST_DIR` and therefore independent of the working directory the build runs
from, plus a three-line `crates/core/build.rs` that `cargo:rerun-if-changed`s the
directory — `sqlx::migrate!` embeds files with `include_str!`, so rustc only watches the
files it found on the *previous* build, and adding the very first migration would not
otherwise force a rebuild. Task 002 ships both; as of this amendment `src-tauri/migrations/`
still holds only `.gitkeep`.

Core must own the migrator, not the shell: task 002 lands one
`rimaia_core::db::migrate(&pool)`, called by the app at startup and by the in-memory harness
in `crates/core/src/testing/db.rs`, so a test can never pass against a schema the running
app does not have.

Moving the files next to the code that reads them was considered and rejected. They belong
to the application, not the library — `sqlx-cli` and packaging point at
`src-tauri/migrations/` regardless of which crate embeds them at compile time — and a
rename would cost a path in three places (the `sqlx::migrate!` argument, `sqlx-cli`'s
`--source`, CI) and buy nothing.

### The offline cache lives at the workspace root

This ADR commits to checking in the `.sqlx` cache but predates the workspace and never says
where. One `.sqlx/` at the workspace root, generated with
`cargo sqlx prepare --workspace -- --all-targets`: `sqlx` looks in the crate directory and
then the workspace root when resolving the cache, and `rimaia-core` is the only crate that
holds queries, so the workspace root is where every consumer — `cargo check` on the
workspace, clippy, CI — finds it without a per-crate override.
