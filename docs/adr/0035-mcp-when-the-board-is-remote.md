# 35. MCP when the board is remote

- **Status:** Proposed
- **Date:** 2026-09-30

## Context

ADR-0006 embedded a Streamable HTTP MCP server in the desktop app, **bound to `127.0.0.1`
and never exposed off `localhost`**, with the loopback interface as its whole trust boundary.
Its 2026-08-28 amendment added a second route, `/mcp/run/{token}`, so a run could reach
exactly its own card. ADR-0021 made the MCP surface everything the UI can do, and split
authority into the **operator** endpoint and the **run-scoped** handle.

The MCP server is how plans get onto the board. A planning session in Claude Code writes the
plan and calls `create_task`. On a team (ADR-0027) the board is on a server, and planning
sessions run on every member's machine. They all need to reach the same board, and none of
them is on the server's loopback.

The run-scoped handle has a different shape. A run executes on a runner (ADR-0031) and needs
to reach its own card, which is on the server. Today the handle is a loopback URL passed as
`--mcp-config`. Task 031's implementation of ADR-0026 (PR #32) wraps it in a `RimaiaHandle`
value that each provider turns into its own configuration (ADR-0026 point 2's `SpawnPlan`).

## Decision

### 1. The server hosts the operator endpoint, behind a token

The hosted server serves the operator surface at `https://<server>/mcp`. It uses the same
rmcp Streamable HTTP transport, the same tool set, the same `snake_case` names and the same
service layer as the embedded server. It authenticates every request with a personal access
token (ADR-0030):

```
claude mcp add --transport http rimaia https://<server>/mcp \
  --header "Authorization: Bearer rmp_…"
```

This supersedes ADR-0006's "never exposed off `localhost`" **for the hosted server only**.
The embedded solo server keeps the loopback bind and the loopback trust boundary exactly as
ADR-0006 describes.

### 2. Tools take a team where a team is ambiguous

A token acts for a user who may be in several teams (ADR-0029):

- **Tools addressing an existing entity** (`get_task`, `update_task`, `move_task`, and so
  on) need no team argument. The entity's id determines its team, and ADR-0029's
  scoping decides whether the caller may see it.
- **Tools that list or create** (`list_tasks`, `list_repositories`, `create_task` without a
  repository id, `get_base_instructions`) take an optional `team`, by id or name. It may be
  omitted only when the token can reach exactly one team.
- **When `team` is omitted and the token reaches several,** the tool is refused with an
  error that lists the teams it could mean. A planning agent reads that and asks, or picks
  one.
- **A new `list_teams` tool** returns the teams the token can reach, so an agent can find
  out before it guesses.

Refusing, rather than defaulting to a "current team", because an agent that writes a
client's plan onto a personal board and reports success is the worst available outcome. The
error costs one extra call.

### 3. Tools gain assignment

`create_task` and `update_task` accept `assignee` (a team member's login). `list_tasks`
filters by it. A plan written by a person's own session is attributed to that person
(ADR-0032 point 3), which is what lets a runner run it with no extra acceptance.

### 4. A connected desktop keeps its loopback endpoint, behind a token

A connected desktop app still serves `http://127.0.0.1:<mcp_port>/mcp`, and forwards each tool
call to the server as its signed-in user. Once connected, the endpoint requires a personal
access token (ADR-0030 point 6):

- **Why.** ADR-0006's loopback boundary was safe because loopback reached one person's local
  board. Connected, it reaches every team that person is in, and any process on the machine
  could use it, an unattended run included (ADR-0032 point 6).
- **What it costs.** A registration made before connecting stops working until it carries the
  token. The app offers to update it in one step.

The loopback endpoint still earns its place. It is the only MCP route to this machine's local
commands (point 6), and one registration then covers both the board and the machine.

### 5. The run-scoped handle stays on the runner, and the server checks it too

A run still receives a loopback URL, `http://127.0.0.1:<port>/mcp/run/{token}`, served by its
runner. ADR-0026's `RimaiaHandle` and every provider's handling of it are unchanged. The
runner checks the tool against ADR-0021's `RunScope` table and forwards permitted calls to
the server with its runner token and the lease `generation` (ADR-0031). The server then
checks again that:

- the runner holds a live lease on that task;
- the tool is permitted for a run.

The check happens twice on purpose. The runner's check stops a confused or prompt-injected
run cheaply, before anything leaves the machine. The server's check means a runner, even one
with its own local code changed, can reach only the task it currently holds a lease on. The
property ADR-0006's amendment bought ("a run reaches its **own** card and nothing else")
holds across the network.

The run token itself never leaves the runner. The server sees a runner token plus a lease,
which is what it already trusts for every other report.

### 6. Two categories of capability have no hosted form

ADR-0021 makes every capability reachable over MCP. On the hosted server two categories are
not, and ADR-0021's parity rule applies to them per command kind (ADR-0034 point 1):

- **Starting a process.** The hosted server cannot spawn anything on a runner. It can only
  make work claimable (ADR-0031). "Plan this task now", which ADR-0021 records as a known gap
  (`plan_task_strategy` has never been an MCP tool), becomes a board tool that *requests* a
  strategy run for the assignee's runner to claim. It no longer starts one directly.
- **Reconfiguring one machine** (runner settings, credentials, the checkout mapping). These
  are local commands (ADR-0034) with no board form. Their MCP reach is the desktop's loopback
  endpoint, answered locally, not forwarded.

`delete_task` stays absent from both surfaces, for ADR-0006's reason.

## Consequences

- **Plans can be written from anywhere onto a shared board**, by any member's Claude Code,
  with the same tools as today. This is the handoff ADR-0006 built, extended to a team.
- **A leaked personal access token is a way to write plans onto its user's boards.**
  ADR-0032 limits what that achieves: the plans are attributed to the token's user, and they
  run only on that user's runners, or on runners of teammates who chose to trust that user.
  A plan written with it by a run is never trusted (ADR-0032 point 6). Tokens can be
  restricted to specific teams, expire, and are revoked from one page (ADR-0030).
- **Tool descriptions change.** The `team` argument and the refusal message are part of the
  surface an agent reads, and ADR-0021's note that descriptions matter more as the surface
  grows applies here directly.
- **Task 021's open question is unaffected in shape.** Whatever it chooses so that a
  review phase can write findings back through the run-scoped handle is chosen on the
  runner's side of point 5. The server's lease check applies either way.

## Alternatives considered

- **Keep MCP loopback-only, and have each desktop app proxy to the server.** No MCP on the
  public internet. Rejected as the only path because a planning session on a machine without
  Rimaia installed (a cloud Claude Code session, a colleague's laptop) could not reach the
  board, and remote planning is the point. Kept as point 4 for machines that do have it.
- **The run connects straight to the server with a run-scoped token.** Removes a hop.
  Rejected: it puts a server URL and a network-valid secret into the run's `argv` and
  environment, where ADR-0006's amendment already notes the run can read them. A loopback
  token is worthless off the machine. A server token is not.
- **A "current team" stored per token.** Fewer arguments. Rejected by point 2's reasoning:
  hidden state that decides where a client's plan lands is exactly what an agent will get
  wrong without noticing.
