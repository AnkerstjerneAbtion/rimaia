---
id: "001"
title: App shell and backend skeleton
milestone: mvp
status: ready
landed: "#2"
depends_on: []
adrs: ["0002", "0015"]
size: M
---

# App shell and backend skeleton

## Goal

Turn the scaffolded counter demo into the structural shell of the real app: a Cargo
workspace with all logic in a Tauri-free core crate, a thin Tauri shell, and a frontend
with the navigation the product needs.

## Why now

Every later task adds code to this structure. Establishing it once, before there is
anything to move, is cheap; doing it on task 008 is not.

## Scope

**Workspace split (ADR-0015)**

- Convert to a Cargo workspace:
  - `crates/core/` — `rimaia-core`, all logic. **Must not depend on `tauri`.**
  - `src-tauri/` — the shell: commands, window, tray, state wiring. Thin.
- The crate boundary is what makes `cargo test -p rimaia-core` run with no WebKit or GTK,
  and what stops business rules from drifting into the Tauri layer where the MCP server
  cannot reach them.

**Core (`crates/core/src/`)**

- Modules with clear ownership:
  - `db/` — pool, migrations, models (task 002 fills this)
  - `repo/` — repository registration and git inspection
  - `tasks/` — task service layer
  - `worktree/` — git worktree operations
  - `runner/` — Claude Code process supervision
  - `scheduler/` — run queue
  - `mcp/` — MCP server
- Dependencies: `tokio` (full), `sqlx` (runtime-tokio, sqlite, chrono, uuid),
  `serde`/`serde_json`, `anyhow`, `thiserror`, `tracing`, `uuid`, `chrono`.
- One error type (`thiserror`) that serializes cleanly — no `String` errors crossing any
  boundary.
- A `Clock` trait (real + injectable) from the start, so nothing timestamps or schedules
  against the wall clock directly (ADR-0015).

**Shell (`src-tauri/src/`)**

- `commands/` — Tauri command handlers, thin wrappers over core services.
- Application data directory resolution via Tauri's path API, created at startup.
- `tracing-subscriber` initialized to stderr and to a rolling log file in the app data
  directory.
- A single `AppState` (`SqlitePool` + config) managed by Tauri and injected into commands.

**Frontend (`src/`)**

- Remove the counter demo.
- App layout: left navigation (Board, Runs, Settings), main content area.
- Routing between the three views (route state is enough; no router library required).
- A typed Tauri command wrapper module — every backend call goes through it, so the
  serialization boundary has one place to be wrong.
- Baseline styling. Dark and light both legible; no design system.

## Out of scope

- Any real data. Views render empty states.
- The board itself (005), settings content (006), runs content (008).

## Acceptance criteria

- `npm run tauri dev` opens a window with the three views and no counter.
- `cargo check --workspace` and `tsc --noEmit` are clean.
- **`crates/core` compiles and its tests run with no WebKit/GTK installed**, and
  `grep -r "tauri" crates/core/Cargo.toml` finds nothing.
- Core modules exist with at least a doc comment stating their responsibility.
- App data directory is created on first launch and its path is visible in Settings.
- An error returned from a command renders as a readable message in the UI, not
  `[object Object]`.

## Notes

Module boundaries are the point, and the crate split makes the compiler enforce them.
`commands/` stays thin — it exists so the MCP server (010) can call the same services
without going through Tauri.

Test tooling comes next, in task 019. Write this task's code so it is testable; the
harness lands immediately after.
