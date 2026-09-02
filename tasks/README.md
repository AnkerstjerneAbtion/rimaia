# Rimaia task backlog

**This table is the order. The number is a stable id, not a position** — same rule the app
itself uses for board cards (ADR-0007). New tasks are appended with the next free number
and placed in the table where they belong. Never renumber.

Tasks 001, 019, 002–009 are the MVP walking skeleton (see
[ADR-0014](../docs/adr/0014-mvp-scope-and-non-goals.md)). The MVP is done when a plan
written in the app runs unattended, produces a branch with commits and a PR, and is
reviewable in the app afterwards.

| Order | # | Task | Milestone | Depends on | Landed |
| --- | --- | --- | --- | --- | --- |
| 1 | [001](001-app-shell-and-backend-skeleton.md) | App shell and backend skeleton | MVP | — | #2 |
| 2 | [019](019-test-harness-and-ci.md) | Test harness and CI | MVP | 001 | #2 |
| 3 | [002](002-sqlite-store-and-migrations.md) | SQLite store and migrations | MVP | 001 | #3 |
| 4 | [003](003-repository-registration.md) | Repository registration | MVP | 002 | #3 |
| 5 | [004](004-task-crud-and-service-layer.md) | Task CRUD and service layer | MVP | 002 | #3 |
| 6 | [005](005-kanban-board-ui.md) | Kanban board UI | MVP | 004 | #3 |
| 7 | [006](006-base-instructions-and-prompt-composition.md) | Base instructions and prompt composition | MVP | 004 | #3 |
| 8 | [007](007-git-worktree-service.md) | Git worktree service | MVP | 003 | #3 |
| 9 | [008](008-claude-code-runner.md) | Claude Code runner | MVP | 006, 007, 019 | #3 |
| 10 | [009](009-sequential-run-queue.md) | Sequential run queue | MVP | 008 | #3 |
| 11 | [010](010-local-mcp-server.md) | Local MCP server | v0.2 | 004, 006 | #5 |
| 12 | [020](020-per-task-execution-strategy.md) | Per-task execution strategy | v0.2 | 010 | #7 |
| 13 | [011](011-task-dependencies-and-blocking.md) | Task dependencies and blocking | v0.2 | 009 | — |
| 14 | [012](012-parallel-execution.md) | Parallel execution | v0.2 | 009 | #16 |
| 15 | [023](023-batch-strategy-planning.md) | Batch strategy planning as a preflight | v0.2 | 020, 012 | — |
| 16 | [013](013-run-scheduling.md) | Run scheduling and windows | v0.2 | 009 | — |
| 17 | [014](014-usage-limit-resilience.md) | Usage-limit resilience and resume | v0.2 | 009, 019 | — |
| 18 | [015](015-run-history-and-log-viewer.md) | Run history and log viewer | v0.3 | 009 | #10 |
| 19 | [016](016-worktree-lifecycle-and-cleanup.md) | Worktree lifecycle and cleanup | v0.3 | 007, 009 | — |
| 20 | [017](017-morning-review-flow.md) | Morning review flow | v0.3 | 015 | — |
| 21 | [018](018-preflight-doctor-and-packaging.md) | Preflight doctor and packaging | v0.3 | 008 | #17 |
| 22 | [022](022-per-repository-git-credentials.md) | Per-repository git credentials | v0.3 | 003, 008 | — |
| 23 | [021](021-review-and-fix-loop.md) | Review-and-fix loop | v0.4 | 015, 017, 020 | — |
| 24 | [024](024-analytics.md) | Analytics — what the queue has actually done | v0.4 | 015 | — |

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
status: ready              # ready | not-ready — readiness to START, not doneness
landed: "#3"               # the PR that landed it. Absent until it has landed.
depends_on: ["003"]
adrs: ["0005"]
size: M                    # S | M | L
---
```

**`status` is not doneness — `landed` is.** `status` says whether a task is ready to be
*picked up*: `ready` means an agent could start it now, `not-ready` means something about the
task itself is still undecided. A finished task stays `ready`, because nothing about it became
unready. Completion is a separate axis and gets a separate field, for the same reason ADR-0007
refused to make execution state a fifth board column: two dimensions, two fields.

**A task is done when its PR is merged, and the PR reference is the record.** Write
`landed: "#3"` — not a commit SHA, which a squash-merge invalidates, and not a date, which says
nothing about where to look. The PR number survives the merge and points at the review, the
discussion and the commits at once. Omit the field entirely until then; an empty or `false`
value is a third state nobody needs.

**Landed is not the same as proven.** A merged PR means the acceptance criteria hold as far as
the tests can establish. Where a criterion needs a human — a real unattended run, a layout at
1280px, a screen reader — the task file says so and the PR body carries the checklist. See
[ADR-0014](../docs/adr/0014-mvp-scope-and-non-goals.md) for the MVP's own version of this
distinction.

**Ids and references are quoted strings, always.** Unquoted `007` is octal in YAML and
parses as `7`, while `008` is not valid octal and parses as the string `"008"` — the same
field would come back as two different types across the backlog.

Body sections: **Goal**, **Why now**, **Scope**, **Out of scope**, **Acceptance criteria**,
**Notes**. Acceptance criteria are the contract — a task is done when they all hold, and
not before.
