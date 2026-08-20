# Rimaia task backlog

**This table is the order. The number is a stable id, not a position** — same rule the app
itself uses for board cards (ADR-0007). New tasks are appended with the next free number
and placed in the table where they belong. Never renumber.

Tasks 001, 019, 002–009 are the MVP walking skeleton (see
[ADR-0014](../docs/adr/0014-mvp-scope-and-non-goals.md)). The MVP is done when a plan
written in the app runs unattended, produces a branch with commits and a PR, and is
reviewable in the app afterwards.

| Order | # | Task | Milestone | Depends on |
| --- | --- | --- | --- | --- |
| 1 | [001](001-app-shell-and-backend-skeleton.md) | App shell and backend skeleton | MVP | — |
| 2 | [019](019-test-harness-and-ci.md) | Test harness and CI | MVP | 001 |
| 3 | [002](002-sqlite-store-and-migrations.md) | SQLite store and migrations | MVP | 001 |
| 4 | [003](003-repository-registration.md) | Repository registration | MVP | 002 |
| 5 | [004](004-task-crud-and-service-layer.md) | Task CRUD and service layer | MVP | 002 |
| 6 | [005](005-kanban-board-ui.md) | Kanban board UI | MVP | 004 |
| 7 | [006](006-base-instructions-and-prompt-composition.md) | Base instructions and prompt composition | MVP | 004 |
| 8 | [007](007-git-worktree-service.md) | Git worktree service | MVP | 003 |
| 9 | [008](008-claude-code-runner.md) | Claude Code runner | MVP | 006, 007, 019 |
| 10 | [009](009-sequential-run-queue.md) | Sequential run queue | MVP | 008 |
| 11 | [010](010-local-mcp-server.md) | Local MCP server | v0.2 | 004, 006 |
| 12 | [020](020-per-task-execution-strategy.md) | Per-task execution strategy | v0.2 | 010 |
| 13 | [011](011-task-dependencies-and-blocking.md) | Task dependencies and blocking | v0.2 | 009 |
| 14 | [012](012-parallel-execution.md) | Parallel execution | v0.2 | 009 |
| 15 | [013](013-run-scheduling.md) | Run scheduling and windows | v0.2 | 009 |
| 16 | [014](014-usage-limit-resilience.md) | Usage-limit resilience and resume | v0.2 | 009, 019 |
| 17 | [015](015-run-history-and-log-viewer.md) | Run history and log viewer | v0.3 | 009 |
| 18 | [016](016-worktree-lifecycle-and-cleanup.md) | Worktree lifecycle and cleanup | v0.3 | 007, 009 |
| 19 | [017](017-morning-review-flow.md) | Morning review flow | v0.3 | 015 |
| 20 | [018](018-preflight-doctor-and-packaging.md) | Preflight doctor and packaging | v0.3 | 008 |
| 21 | [021](021-review-and-fix-loop.md) | Review-and-fix loop | v0.4 | 015, 017, 020 |

## Before task 001

A throwaway spike, half a day, code discarded: create a worktree, spawn
`claude -p --output-format stream-json --verbose --session-id <uuid> --permission-mode
bypassPermissions`, feed a prompt on stdin, print events.

Verifies the assumptions in ADR-0004 and ADR-0011 before seven tasks of scaffolding are
built on them, and — the reason it is not optional — **produces the first CLI fixtures for
task 019**. Keep the captured JSONL; throw the code away.

## Task file format

Front matter is machine-readable so this backlog can be imported into Rimaia itself once
the MCP server exists (task 010). Keep it valid YAML.

```yaml
---
id: "007"
title: Git worktree service
milestone: mvp             # mvp | v0.2 | v0.3 | v0.4
status: ready              # ready | not-ready
depends_on: ["003"]
adrs: ["0005"]
size: M                    # S | M | L
---
```

**Ids and references are quoted strings, always.** Unquoted `007` is octal in YAML and
parses as `7`, while `008` is not valid octal and parses as the string `"008"` — the same
field would come back as two different types across the backlog.

Body sections: **Goal**, **Why now**, **Scope**, **Out of scope**, **Acceptance criteria**,
**Notes**. Acceptance criteria are the contract — a task is done when they all hold, and
not before.
