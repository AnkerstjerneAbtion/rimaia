---
id: "004"
title: Task CRUD and service layer
milestone: mvp
status: ready
depends_on: ["002"]
adrs: ["0003", "0007", "0018"]
size: M
---

# Task CRUD and service layer

## Goal

The task service — every operation on a task, with its invariants — plus the Tauri
commands that expose it. This is the layer both the UI and the MCP server call.

## Why now

The board (005), the runner (008), and the MCP server (010) are all clients of this. If
invariants live in the command handlers instead of here, the MCP server will reimplement
them differently.

## Scope

**Service (`tasks/service.rs`)** — pure Rust, no Tauri types:

- `create_task` — repository, title, optional plan, column (default `not_ready`), extra
  instructions, links. Appends to the bottom of the target column.
- `get_task` — full task with links, dependencies, and last run summary.
- `list_tasks` — filter by repository, column, run state; ordered by position.
- `update_task` — patch semantics; only provided fields change.
- `delete_task` — refuses when other tasks depend on it (message names them).
- `move_task(column, before_id, after_id)` — computes the fractional position, rebalances
  when needed, in one transaction.
- `set_run_state` — the only path to `run_state`; validates transitions and rejects
  illegal ones (e.g. `idle → running` without going through `queued`).
- Link operations: add, update, remove, reorder.

**Rules enforced here, not above:**

- A task cannot move to `ready` with an empty plan.
- Moving to `done` from anywhere is allowed; the user is in charge of their own board.
- `title` is required and non-blank; `plan` is Markdown, unbounded.
- Every mutation stamps `updated_at` and emits a change event so all clients refresh.

**Commands (`commands/tasks.rs`)** — thin wrappers, serialization only.

**Events** — a single `tasks:changed` Tauri event carrying the affected task ids, so a
task created over MCP appears on the board without a poll.

## Out of scope

- Dependency edges beyond the storage and the delete guard (011 adds semantics).
- Board rendering (005).

## Acceptance criteria

- Unit tests over the service cover each rule above, including the illegal run-state
  transitions and the empty-plan guard.
- Reordering within and between columns produces correct order, and a forced rebalance
  case is tested.
- Deleting a task removes its links and dependency edges; deleting a depended-on task is
  refused.
- `tasks:changed` fires for every mutation, including those originating outside the UI.

## Notes

The service takes `&SqlitePool` and plain types. No `AppHandle`, no `tauri::State`. That
is what makes task 010 a thin adapter instead of a second implementation.
