# 14. MVP scope and non-goals

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

The purpose of the first version is to answer one question: **does handing plans to an
unattended agent overnight and reviewing the results in the morning actually produce
useful work?** Everything else — board polish, scheduling, integrations — is worth
building only if the answer is yes.

The risk is not that the Kanban board is hard. It is that the runner is: spawning Claude
Code headlessly in a worktree, streaming its output, and getting a reviewable branch out
of it. That is the part that must be proven first, and the fastest path to proving it
should not be blocked behind fourteen tasks of UI.

## Decision

### MVP — the walking skeleton (tasks 001–009)

Delivers the full loop, minimally:

- SQLite store with the complete schema (all of it, even where unused, so later work is
  additive).
- Register a local git repository.
- Create and edit tasks: title, plan, extra instructions, links.
- Four-column Kanban board with drag-to-reorder.
- Global base instructions with prompt composition.
- Worktree and branch per task.
- Claude Code runner: spawn, stream, classify, persist, live log.
- Sequential run queue over the `ready` column, started manually.

The MVP is complete when: a plan created in the app runs unattended and produces a branch
with commits and a PR, and the run is reviewable in the app the next morning.

Deliberately **not** in the MVP, in rough order of when they land:

- MCP server (task 010) — the handoff is the workflow, but for proving the loop, typing a
  plan into a textarea is equivalent.
- Dependencies and blocking (011).
- Parallel execution (012).
- Scheduling and run windows (013).
- Usage-limit resilience (014) — the first real overnight queue will need this, and it
  should be built once there is a real queue to test it against.
- Run history browsing, worktree cleanup, review flow, preflight checks (015–018).

### Non-goals, indefinitely

- **No chat interface.** Rimaia hands off plans and reports results. Conversation happens
  in Claude Code, where it belongs. This is a product boundary, not a scope cut.
- **No hosted or multi-user mode.** Single user, single machine (ADR-0002).
- **No Asana or GitHub API integration.** Links are URLs the user pastes or an agent
  attaches. Rimaia does not sync issue state.
- **No plan authoring assistance.** Plans arrive finished, from a Claude Code session.
- **No custom agent runtime.** Claude Code is the agent (ADR-0004).
- **No metered API key path.** The premise is the personal subscription.

## Consequences

- The riskiest component is proven by task 009, before effort goes into the features that
  depend on it being viable.
- Post-MVP tasks are additive: the schema, the runner seam, and the service layer are
  built for them from the start, so nothing has to be torn out.
- Nine tasks is more than a demo. That is deliberate — a runner without persistence or a
  board is not evidence that the workflow works, it is a script.
- The MVP has a real gap in daily use: without the MCP server, plans have to be pasted in
  by hand. Task 010 is first in line after the skeleton for exactly that reason.

## Alternatives considered

- **Runner-only spike first.** Faster to a demo, and the demo would not answer the
  question — one task run by hand proves the CLI works, not that the workflow does.
- **MCP server in the MVP.** It is the nicest part of the product and it does not reduce
  the central risk. Building it before the runner is proven risks polishing a handoff into
  something that doesn't work.
- **Full feature set before first use.** Weeks before the premise is tested, which is the
  failure mode this ADR exists to prevent.
