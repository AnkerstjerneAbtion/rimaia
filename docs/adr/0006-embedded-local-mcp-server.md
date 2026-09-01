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

---

## Amendment, 2026-08-28 — an eleventh tool, and a scoped route for runs

Task 020 is the first time a **run** talks to this server rather than a planning session.
ADR-0016 promised runs "a scoped MCP handle to their own task" and never said what scoped
means on the wire; this amendment says. The decision above is otherwise unchanged — same
transport, same loopback bind, same port, same service layer underneath.

### `set_task_strategy` is the eleventh tool, not a field on `update_task`

| Tool | Purpose |
| --- | --- |
| `set_task_strategy` | Record a proposed execution strategy: model, effort, workflow shape, phase breakdown, rationale |

Folding it into `update_task` is the obvious move, and it is wrong on three counts.

- **Different shape.** `update_task` is a patch of independent scalar fields, erasing
  through a `clear: [field]` list (seam-contract D16.5). A strategy is one envelope that is
  only coherent whole — a `workflow` of `multi_agent` with no `phases` is not a partial
  update, it is an invalid one — and writing it stamps `strategy_source` and
  `strategy_updated_at` as a consequence. A patch tool that sometimes also stamps
  provenance is a patch tool with a special case in it.
- **Different audience.** `update_task` is for an operator's planning session editing a
  card it already knows about. `set_task_strategy` is called by a bounded planner run
  whose entire output is that one call, at the end of a session that was given nothing else
  to do.
- **Different validation.** The write is refused when the task is not in `planned` mode,
  and refused when it would overwrite a `user` strategy with a `planner` one. Neither rule
  has anything to say about the other twelve fields of a patch, so on a merged tool they
  become conditional on *which fields the caller happened to send* — validation that keys
  off the shape of the request rather than off a value.

There is also a cost to the merge that this ADR already names: tool descriptions "state
*when* to call them, not just what they do — that measurably improves tool selection". One
description serving both "patch any field of an existing task" and "you are a planner; this
is how you answer" is accurate for neither caller, and the run that most needs to pick
correctly is the cheap, low-effort one.

The *mode* is a different thing from the proposal, and it does go on `update_task`:
`strategy_mode` is a plain enum an operator sets from either door, so it is a patch field
like any other. What does not fit a patch is the planner's document.

**The table is otherwise still closed.** Eleven tools; `delete_task` and every run
operation remain deliberately absent, for the reasons the Decision section gives.

### `/mcp/run/{token}` — a second route, for runs

`/mcp` stays exactly as it is. It is the URL the user pasted into `claude mcp add`, this
ADR fixes it, and re-scoping it would break every registered session. Runs get a **second
route** instead, minted per run and revoked when the run ends:

```
--mcp-config {"mcpServers":{"rimaia":{"type":"http","url":"http://127.0.0.1:4517/mcp/run/<token>"}}}
```

The token resolves to one task id, and every handler's first statement authorizes the tool
against the scope it was reached through. An unknown or revoked token is a bare 404 — it is
not an oracle for which tokens exist.

| Tool | `Operator` (`/mcp`) | `Run { task_id }` (`/mcp/run/{token}`) |
| --- | --- | --- |
| `get_task`, `update_task`, `set_task_strategy`, `add_task_link`, `remove_task_link` | ✔ | ✔ **its own task only** |
| `get_base_instructions`, `list_repositories` | ✔ | ✔ (no task to scope) |
| `create_task`, `move_task`, `set_task_dependencies`, `list_tasks` | ✔ | ✘ refused |

`move_task` is refused because the runner owns where a card lands when a run finishes, and
a run moving its own card to `done` would be marking its own homework. `list_tasks` because
a run has no business enumerating someone's board. `create_task` and
`set_task_dependencies` because a run spawning or reordering work is orchestration, which
ADR-0016 declines to build. None of these are new capabilities being added and then
withdrawn: they are the operator surface, declined for a narrower caller, in the same way
this ADR declines `delete_task` for both.

### The threat model, stated honestly

**The token is in argv, and therefore visible in `ps` to the same user.** That is not a
widening. The Consequences above already put the boundary at "anything on the machine that
can reach loopback", and ADR-0012 already grants the run arbitrary bash — a secret a
process could read out of its own process table protects nothing from that process.

**The token's job is to stop the confused deputy**, not to stop an attacker. The realistic
failure is a run that is prompt-injected by a file it read, or simply mistaken about which
card it is working on, addressing a task that is not its own. Before this route the only
thing standing between that run and someone else's board was a task id in a sentence in the
prompt, which the model may or may not still be attending to twenty turns later. After it,
the task id is on the server value and the check is a function call. That is the whole of
what changed, and it is worth having.

If the trust boundary itself ever changes, the remedy this ADR already names — a
shared-secret header — is orthogonal and composes: it would protect `/mcp` and
`/mcp/run/{token}` alike, and neither replaces the other.

Mechanism, and why a header-carried token was rejected instead of a path segment, is
seam-contract D17.
