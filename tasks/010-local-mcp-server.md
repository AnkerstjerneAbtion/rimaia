---
id: "010"
title: Local MCP server
milestone: v0.2
status: ready
landed: "#5"
depends_on: ["004", "006"]
adrs: ["0006"]
size: L
---

# Local MCP server

## Goal

Expose task management over MCP so a Claude Code session anywhere on the machine can hand
a finished plan to Rimaia without the user leaving that session.

## Why now

This is the workflow. The MVP proved the loop; this makes it usable daily.

## Scope

**Server**

- Streamable HTTP MCP server bound to `127.0.0.1` (loopback hard-coded), default port
  `4517`, port configurable for collisions.
- Starts with the app, stops with it. Startup surfaces a port-in-use error instead of
  failing silently.
- Serves via `axum` on the app's Tokio runtime, sharing the `SqlitePool`.
- Uses the official Rust MCP SDK (`rmcp`) if it fits cleanly; otherwise implement the
  JSON-RPC surface directly over `axum`. Either way the transport is a thin adapter over
  the task service from 004 — **no business logic in this module**.

**Tools** (ADR-0006)

`list_repositories`, `create_task`, `update_task`, `list_tasks`, `get_task`, `move_task`,
`add_task_link`, `remove_task_link`, `set_task_dependencies`, `get_base_instructions`.

- Tool descriptions state *when* to call them, not only what they do.
- Input schemas are strict; validation errors return the actual problem ("column must be
  one of …", not "invalid input").
- Every mutation is attributed `source = "mcp"` and emits `tasks:changed`, so the board
  updates live.

**Setup UX**

- Settings → MCP shows the server URL, status, and a copyable
  `claude mcp add --transport http rimaia http://127.0.0.1:4517/mcp`.
- A "test connection" action that exercises the endpoint the way a client would.

## Out of scope

- Authentication (ADR-0006 documents the loopback trust boundary).
- Tools for starting or cancelling runs — planning agents create work, they do not
  schedule it. Revisit only if the need is real.

## Acceptance criteria

- After the one-line `claude mcp add`, a separate Claude Code session can create a task
  with a plan, links, and a target column, and it appears on the board within a second.
- `get_task` round-trips a multi-thousand-word Markdown plan without corruption.
- Invalid input produces a specific, actionable error to the calling agent.
- Creating a task over MCP that violates a service invariant is rejected identically to
  the UI path — verified by a test that exercises both against the same case.
- Stopping the app makes the server unreachable with a normal connection error.

## Notes

The "same invariants, both paths" test is the point of this task's design. If it is hard
to write, the service boundary from task 004 leaked and should be fixed here rather than
worked around.
