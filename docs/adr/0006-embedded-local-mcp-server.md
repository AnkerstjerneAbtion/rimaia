# 6. Embedded local MCP server over HTTP

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

The intended workflow is: plan a task in a Claude Code session (in a project, in
Conductor, in an IDE), then hand the finished plan to Rimaia without leaving that session.
That handoff is an MCP server exposing Rimaia's task operations as tools.

Requirements:

- Reachable from arbitrary Claude Code sessions on the same machine.
- Configured once by the user, then always available.
- Writes go through the same code paths and invariants as the UI — no second
  implementation of "create a task".
- Never exposed off `localhost`.

Two transports are plausible: a **stdio** server (a small binary Claude Code spawns per
session, talking to the app over IPC or the database) or a **Streamable HTTP** server
hosted inside the running app.

## Decision

Embed a **Streamable HTTP MCP server in the Tauri backend**, bound to `127.0.0.1` on a
configurable port (default `4517`).

- The server starts with the app and shuts down with it. Registration is one-time:
  `claude mcp add --transport http rimaia http://127.0.0.1:4517/mcp`.
- Bind address is hard-coded to loopback. The port is configurable for collision, not
  the interface.
- MCP tools call the same service layer as the Tauri commands. The transport is a thin
  adapter; business rules (dependency cycles, column validity, ordering) live below it.
- Tool surface (v1):

  | Tool | Purpose |
  | --- | --- |
  | `list_repositories` | Discover registered repos and their ids |
  | `create_task` | Title, plan, repo, column, extra instructions, links, dependencies |
  | `update_task` | Patch any field of an existing task |
  | `list_tasks` | Filter by repository, column, run state |
  | `get_task` | Full task including plan, links, dependencies, last run |
  | `move_task` | Change column and/or position |
  | `add_task_link` / `remove_task_link` | Manage external references |
  | `set_task_dependencies` | Declare `blocked_by` edges, cycle-checked |
  | `get_base_instructions` | Let a planning agent see what will be prepended |

- Tool descriptions state *when* to call them, not just what they do — that measurably
  improves tool selection.
- Every mutation is attributed (`source = "mcp"`) and appears in the UI immediately via a
  Tauri event, so a task created from another session shows up on the board without a
  refresh.

## Consequences

- Tasks can only be created while Rimaia is running. Acceptable — it is a desktop app the
  user keeps open — and a clear connection error beats a silent queue divergence.
- One configuration step, one process, no per-session subprocess. No packaging of a second
  binary.
- Because the server shares the app's `SqlitePool` and service layer, an MCP write and a
  UI write cannot disagree about validity.
- Anything on the machine that can reach loopback can drive the server. On a single-user
  developer machine that is the same trust boundary as the shell. If that changes, add a
  shared-secret header — the transport already supports custom headers on the client side.
- Streamable HTTP is the transport Claude Code's `--transport http` expects; SSE-only
  clients are not supported and do not need to be.

## Alternatives considered

- **stdio MCP server binary.** Works when the app is closed (if it talks directly to
  SQLite), but bypasses the service layer, duplicates invariants, needs a second shipped
  binary, and gives no live UI updates. If offline task creation becomes important, the
  right shape is a thin stdio client that forwards to the HTTP server, not a second
  writer.
- **Writing tasks as files in a watched directory.** No validation, no responses, no
  errors surfaced back to the planning agent.
