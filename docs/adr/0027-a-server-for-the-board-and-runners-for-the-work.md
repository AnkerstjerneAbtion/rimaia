# 27. A server for the board, runners for the work

- **Status:** Proposed
- **Date:** 2026-09-30

## Context

ADR-0002 made Rimaia one process on one machine for one person, and ADR-0014 listed "no
hosted or multi-user mode" as a non-goal indefinitely. Both were right for the question the
MVP had to answer: does handing plans to an unattended agent overnight produce useful work?
Rimaia has since built a good part of its own backlog, which answers that question well
enough to ask the next one.

The next one is a team. People plan together during the day. At night, each person's agents
work through the plans that person is responsible for, on that person's machine and
subscription. The board becomes a queue the whole team shares, and every member's computer
becomes a worker that pulls from it. A spare machine with nobody at it is just a worker with
no window.

ADR-0002's argument for staying local still holds, and it limits what can change. The Claude
Code CLI authenticates with a personal subscription on a personal machine. The repositories,
toolchains and `~/.claude` configuration that make a run behave like its owner's own Claude
Code are on that machine too. A hosted service still "could not use a personal subscription".
So the *work* cannot move. What can move is the *board*: the plans, the order, the
dependencies and the record of what ran.

The codebase is closer to this split than ADR-0002 would suggest:

- `rimaia-core` does not depend on Tauri (ADR-0015).
- Every UI call crosses `src/lib/commands.ts`, and every event crosses `src/lib/events.ts`
  (seam-contract D7).
- Change events carry ids, not rows (ADR-0018), so any number of clients can re-read safely.
- The MCP surface already has to match everything the UI can do (ADR-0021).
- ADR-0024 has already prepared the design for a browser.

The one assumption that runs through everything is that **one process owns everything**:

- The scheduler keeps its in-flight leases in an in-memory map (`scheduler/inflight.rs`).
- Startup recovery works on every task in `running` or `queued`, whoever started it.
  `startup::survey` finds them with no filter, and `scheduler/reconcile.rs` closes their open
  runs as `interrupted` and moves the tasks on (seam-contract D9).
- The queue's go signal and the usage-limit pause are global settings rows.

Any design where two machines share this state has to replace that assumption explicitly. It
cannot inherit it.

## Decision

**Rimaia splits into two roles. A server holds the board. Runners do the work. Solo use is
the same two roles in one process.**

### 1. The server owns the board

The server owns teams, users, repositories as the team knows them, tasks, positions,
dependencies, the record of every run, team settings and change events. It is the only
writer of that state and the only authority on who is running what (ADR-0031). It never
spawns an agent, never touches a git working tree and never holds a forge credential.

### 2. A runner owns execution

A runner owns checkouts, worktrees, the agent CLI behind the provider seam (ADR-0026),
per-repository credentials (ADR-0020), the transcript while it is being written, and every
setting that describes *this machine*: concurrency, run windows, the go signal and the
usage-limit pause. A runner belongs to one user and runs on that user's subscription.

### 3. Runners reach the board only through the server's API, never its database

A runner that shares the database inherits the one-process assumption, and today's code
breaks immediately under it:

- A laptop launching would mark every other machine's runs as interrupted.
- One person's usage limit would pause the whole team.

A direct connection also puts database credentials on every laptop, gives no per-user
authorisation and ties every client to the server's migration schedule. The API is the
boundary (ADR-0034).

### 4. Three ways to run it, one code path

| Mode | Server | Runner | Who it is for |
| --- | --- | --- | --- |
| **Solo** | Inside the desktop app, loopback only, one implicit team and user | Inside the desktop app | One person, with no Rimaia server and nothing sent to one. Today's product |
| **Connected** | A hosted instance | Inside the desktop app | A team member, or one person with a personal team (ADR-0029) |
| **Headless** | A hosted instance | `rimaia-runner`, no window | A spare machine that only does work |

The desktop app is solo or connected, chosen at first launch and changeable later. Solo is
not a separate product with its own rules. It is the server and a runner in one process, and
the runner talks to the server through the same interface in every mode (point 5). A rule
that holds in solo holds when connected, because it is the same code.

### 5. The runner depends on a board port, with two adapters

Runner code reaches the board through a trait (claim, heartbeat, report a transition, finish
a run, upload a transcript), not through services directly:

- **In process.** Solo mode uses an adapter that calls the `rimaia-core` services.
- **HTTP.** Connected and headless runners use an adapter that calls the server.

One contract test suite runs against both adapters, so they cannot drift. This is ADR-0006's
rule applied to a new boundary: one implementation of each business rule, with adapters
around it.

### 6. Crate layout

- `rimaia-core` stays free of Tauri and holds the logic for both roles.
- A new `rimaia-server` crate is the axum HTTP server and its binary. Axum already arrived
  with the MCP server.
- A new `rimaia-runner` crate is the runner loop, its local store (ADR-0028) and its binary.
- `src-tauri` becomes the shell that hosts a runner and, in solo mode, a server.

The compiler enforces the split the same way ADR-0015 made it enforce the core/shell split.
`rimaia-server` must not depend on `rimaia-runner`.

The names overlap: `rimaia_core::runner` is the existing agent-process layer (spawn, stream,
classify). The new `rimaia-runner` crate is the loop around it (claim, supervise, report).
The overlap is accepted because the paths never collide, and "runner" is the word the product
uses for the role.

### 7. The board is still a queue of plans

This ADR widens *who* uses the board, not *what* it is. Comments, mentions, notifications,
sprints and estimates are project management. They stay out until a separate decision
admits them. ADR-0014's other non-goals stand: no chat interface, no plan-authoring
assistance, no custom agent runtime, no metered API key path. Signing in with GitHub
(ADR-0030) is identity, not the issue sync ADR-0014 declines.

### What this supersedes

- **ADR-0002**, for connected and headless modes: "no network service, no account, no cloud
  sync", and the rejection of "a web app with a local agent". ADR-0002 still describes solo
  mode exactly.
- **ADR-0014**'s non-goal "no hosted or multi-user mode". Nothing else in ADR-0014.

The narrower amendments are in their own records. Each of those records names the sentence
it changes:

| Amended ADR | By | What changes |
| --- | --- | --- |
| 0003 SQLite store | 0028 | The server owns the board's file. Runners get a second store |
| 0005 Worktrees | 0033 | Worktrees stay local. The board records which runner holds one |
| 0006 Embedded MCP | 0035 | Loopback-only no longer applies to the hosted server |
| 0008 Dependencies | 0033 | A dependent branches from a recorded commit, not a local branch name |
| 0009 Prompt composition | 0032 | The prompt states who wrote the plan and whose machine runs it |
| 0010 Scheduler | 0031 | Claims are server-side leases. Queue control belongs to a runner |
| 0011 Resilience | 0031, 0033 | Retries are pinned to their runner. Connected `success` requires a pushed branch |
| 0012 Permissions | 0032 | Unattended consent is per runner, under a team ceiling, and given to a plan revision |
| 0013 Run logging | 0036 | Transcripts are uploaded. Review renders from an uploaded bundle |
| 0016 Execution strategy | 0031 | Planner runs are claimed and leased like implementation runs |
| 0017 Review loop | 0033 | The push postcondition applies to the loop's final commit |
| 0018 Change events | 0034 | An event carries its team, so the server can filter the fan-out |
| 0019 Mutation source | 0029, 0030 | `ServiceContext` gains the team scope and the acting user |
| 0020 Credentials | 0033 | Keyed by team repository and runner. Never on the server |
| 0021 MCP parity | 0034, 0035 | Parity is per command kind. Local commands have no hosted MCP form |
| 0022 Run memory | 0036 | Pruning happens on the server, under a team retention setting |
| 0025 Archiving | 0033 | The on-archive script is runner configuration |

## Consequences

- **The premise survives.** Agents still run locally, on their owner's subscription, in their
  owner's environment. The team shares plans and results, never compute or credentials.
- **Solo keeps working offline, with no account.** Keeping it is a requirement, not a
  fallback. It is also the answer for anyone whose code may not leave their machine.
- **Rimaia gains an auth boundary and a second deployment target.** ADR-0002 rejected exactly
  this. The cost is real: sign-in, sessions, tokens, hosting, backups and version skew between
  server and runners (ADR-0030, ADR-0037).
- **Every change to the board's services now has a network client.** The contract suite from
  point 5 is what keeps "works in solo" meaning "works connected".
- **The one-process assumption has to be removed, not worked around.** The in-memory lease
  map, the global startup recovery and the global queue settings are each replaced by a named
  mechanism in ADR-0031. A task that finds a fourth such assumption stops and says so.
- **Review becomes the bottleneck sooner.** N runners produce N nights of branches. Task 017
  (morning review) and task 021 (review-and-fix loop) matter more after this than before it.

## Alternatives considered

- **Share the database directly (desktop apps connect to one database).** Least new code on
  paper. Rejected for the reasons in point 3: today's recovery and pause logic breaks
  immediately, and it has no authorisation boundary. SQLite cannot be shared over a network
  anyway, so this would also mean Postgres on day one.
- **Hosted execution.** Runs on a server would need API keys and metered billing, which
  ADR-0014 rules out, and would lose the owner's toolchains and configuration, which ADR-0005
  and ADR-0012 rely on.
- **Local-first replicas with sync (CRDTs, or replicated SQLite).** Good for offline editing
  of a board. Wrong for a work queue: claiming a task must have exactly one winner, and a
  replicated store gives that only by adding a coordinator, which is a server by another
  name.
- **A runner that pulls from an existing board (Linear, GitHub Issues, Asana).** Smaller, and
  honest about not building project management. Rejected for now because what Rimaia adds is
  in its own task model: plans, execution strategy, dependencies that unblock on a successful
  run, and runs as first-class records. Mapping those onto another tool's fields loses what
  makes a night of runs trustworthy. An importer is still possible later.
- **Make the desktop app a thin client with no solo mode.** One fewer mode. Rejected by the
  requirement: solo is how most people will start, and it is the only mode where nothing
  leaves the machine.
