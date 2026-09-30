# 34. One API for the web and the desktop, split into board and local commands

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

The frontend has exactly one way to reach the backend: `src/lib/commands.ts` wraps 97
commands behind `invoke`, and `src/lib/events.ts` wraps every `listen` (seam-contract D7).
Apart from tests, one component bypasses them: `RepositoryAddForm.tsx` imports the native
folder picker directly. The shell forwards `ChangeEvent`s as `tasks:changed`,
`repositories:changed`, `runs:changed`, `settings:changed` and `schedules:changed`
(ADR-0018), and the live run tail as `runs:tail` (seam-contract D14).

After ADR-0027 the same React app runs in three places:

- **The browser**, which has only the server.
- **The connected desktop app**, which has the server and a local runner.
- **The solo desktop app**, which has both in one process.

The 97 commands are not all the same kind. Some are about the board: tasks, repositories as
the team knows them, runs as records, team settings. Some can only be answered by *this
machine*:

- the doctor;
- the checkout mapping;
- credentials;
- runner settings;
- opening a worktree in an editor;
- revealing a file;
- choosing a folder.

A browser can answer none of the second kind. A connected desktop must not send them to a
server that cannot answer them either.

## Decision

### 1. Every command is classified as board or local

`commands.ts` records, for each command, which kind it is:

- **Board commands** are served by the server: over HTTP when connected, in process when
  solo.
- **Local commands** are served by the desktop shell through `invoke`, in both desktop modes,
  and are unavailable in the browser.

`scripts/check-command-wiring.sh` grows to check the classification. Every board command must
have an HTTP route, every local command must be registered in the shell, and no command may
be unclassified. ADR-0021's parity rule extends with it: every board command is reachable
over HTTP and over MCP.

### 2. Board commands are HTTP, named like the commands

The server exposes `POST /api/v1/<command_name>` with the command's existing request and
response DTOs, serialised by the same serde types. Errors use the same `Error` and
`ErrorCode` the Tauri boundary uses (seam-contract D8). Nothing is translated between the two
transports, so a frontend handling an error from one handles it from both.

RPC named after the command rather than REST resources, because the command list is already
the contract, and a second vocabulary for the same operations would make the parity check
into a translation table. Both the Tauri command and the HTTP handler call the same
`rimaia-core` function, which is ADR-0006's rule and the only one that matters here.

The runner protocol (claim, heartbeat, report, finish; ADR-0031) uses the same naming, under
`/api/v1/runner/<command_name>`. It is authenticated only by runner tokens, and is not part of
the UI's command list. The one route whose body is not JSON is `append_transcript`
(ADR-0036). It takes the run id, lease generation and byte offset as query parameters and the
transcript bytes as the body, because re-encoding tens of megabytes as JSON buys nothing.

### 3. Events are one Server-Sent Events stream per client

`GET /api/v1/events` is an SSE stream carrying the same event names and payloads the Tauri
forwarder emits:

- ids only for changes (ADR-0018);
- a payload for `runs:tail` (seam-contract D14).

**This amends ADR-0018 in one respect: each `ChangeEvent` carries the team it belongs to.**
The team id is routing metadata, not row data, so ADR-0018's argument against payloads (a
second source of truth) does not apply to it. Without it, filtering would need a database
lookup per id, and that lookup fails for a task that has just been deleted.

Filtering and reliability:

- **Filtered per subscriber by team** (ADR-0029): a client receives only events for teams it
  may read.
- **Run tail is opt-in per run.** A client asks for a run's tail by id, so a board with twenty
  cards does not receive twenty live streams.
- **A dropped connection is recovered the way a lagged receiver is today.** On reconnect,
  the server sends each change event once with an empty id list, meaning "re-read this
  entity wholesale". That is ADR-0018's existing recovery, with no replay log.

SSE rather than WebSockets because every message flows server to client. Commands already
have their own requests, and SSE reconnects through proxies with no protocol of its own.

### 4. `commands.ts` and `events.ts` get a transport, and nothing else changes

Each wrapper picks its transport from the command's classification and the mode:

| Mode | Board commands | Local commands | Events |
| --- | --- | --- | --- |
| Solo desktop | `invoke` | `invoke` | Tauri `listen` |
| Connected desktop | HTTP, bearer desktop token (ADR-0030) | `invoke` | SSE for board events, Tauri `listen` for local ones |
| Browser | HTTP, session cookie | unavailable | SSE |

Components keep calling the same functions. The test suite keeps mocking at the
`@tauri-apps/api/core` boundary, with an equivalent HTTP mock added for the new transport.
Neither mocks the wrappers themselves, for the reason `StorageSection.test.tsx` gives. The
native folder picker, which `RepositoryAddForm.tsx` imports directly today, moves behind a
local command, so no component imports a Tauri plugin.

Solo mode keeps `invoke` for board commands. It is today's path, it has no network stack to
fail, and the contract suite from ADR-0027 point 5 is what guarantees the HTTP path and the
in-process path give the same answers.

### 5. The UI asks what the client can do

`get_client_capabilities` returns what the current client supports:

- whether a local runner is present;
- whether it can open editors or reveal files;
- which runner is local;
- the mode.

Only the client knows these facts, so it is **not** a board command. The desktop shell
answers it as a local command. In the browser, the HTTP transport answers it itself with the
browser's fixed answer: no local runner, no local actions. Components hide or disable actions
from this answer, never from checking which platform they are on. In the browser:

- "Open worktree" is replaced by the name of the machine that holds the worktree.
- Runner settings are read-only, showing each of the user's runners.
- The doctor shows each runner's last reported result instead of running checks locally.

### 6. The browser app is the same build, served by the server

`npm run build` produces one bundle. The server serves it at `/`, with the API at `/api/v1`
on the same origin, so browser sessions need no CORS configuration. The desktop app loads the
same bundle from its own resources. Its connected mode calls the server cross-origin, with a
bearer token and no cookies, which the server allows only for the Tauri origins.

### 7. The API is versioned from the first request

The `/v1` path segment is the major version. Runners and desktop apps also send their
protocol version on every request, and ADR-0037 decides what the server does with it. A
breaking change to a board command is a new command name or a new major version, never a
silent change of meaning.

## Consequences

- **The web app is mostly a transport change.** The components, the design (ADR-0024) and the
  DTOs are shared. The new work is the classification, the HTTP and SSE transports, the
  capability checks and the browser-specific states of desktop-only actions.
- **Every new command must declare a kind**, and the wiring check enforces it. That is an
  obligation per command, in the same spirit as ADR-0021's.
- **Two transports must behave identically.** The contract suite runs the board commands
  through both. A behaviour that differs is a bug in whichever one the suite shows is wrong.
- **The desktop shell becomes thinner when connected.** It serves only local commands and
  hosts the runner. The board logic runs on the server.

## Alternatives considered

- **REST resources.** Conventional and cacheable. It would mean designing a second naming
  scheme for 97 existing operations and keeping it in step with the first. The operations are
  commands with side effects, and naming them as such is honest.
- **GraphQL.** Flexible reads. It adds a schema layer, a client library and a second
  error-shape convention, for a UI whose reads are already shaped by the commands
  (seam-contract D12's bulk board read, for example).
- **WebSockets for everything.** One connection for commands and events. More state per
  client, reconnection logic to write, and harder to route through ordinary HTTP
  infrastructure. SSE plus plain requests keeps commands stateless.
- **The desktop app proxies board commands through the shell to the server.** Keeps the
  frontend single-transport. It adds a hop and a second place where authentication and
  errors are handled, and the frontend needs the HTTP transport for the browser anyway.
- **Use MCP as the API.** ADR-0021 already makes MCP a full surface. Rejected as the UI's
  transport because MCP's shapes are deliberately different from the commands' (ADR-0021
  lists why), and a UI has needs, such as streaming events and bulk reads, that MCP tools
  are not designed for.
