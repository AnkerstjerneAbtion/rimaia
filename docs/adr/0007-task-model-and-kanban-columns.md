# 7. Task model, four Kanban columns, position as priority

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

A task is the unit of handoff between planning (done by the user, with an agent) and
execution (done by an agent, unattended). It has to carry everything an agent needs to
work with no further conversation, and everything the user needs to review the result the
next morning.

The board also has to answer "what runs next" without a separate priority field to keep in
sync.

## Decision

### Columns

Four columns, in board order:

| Column | Meaning |
| --- | --- |
| `not_ready` | Captured, not yet safe to hand to an agent (plan missing or incomplete) |
| `ready` | Plan complete; eligible to be picked up by a run |
| `in_review` | An agent finished successfully; awaiting human review |
| `done` | Reviewed and accepted |

Only `ready` feeds the run queue. `in_review` is where the morning review starts.

### Run state is separate from column

Execution status is a **separate field** on the task, not a fifth column:

`idle` · `queued` · `running` · `blocked` · `waiting_retry` · `failed` · `cancelled`

The card stays in `ready` while it runs and moves to `in_review` when the run succeeds.
Failures stay in `ready` with `run_state = failed` and an error surfaced on the card.

Two dimensions, two fields. A card that failed twice and is now waiting for a usage-limit
reset is still "ready to be implemented" — the column says where it is in *your* process,
the run state says where it is in *the machine's*.

### Task fields

| Field | Notes |
| --- | --- |
| `title` | Short, human-scannable |
| `plan` | Markdown. The implementation plan produced during planning |
| `extra_instructions` | Short free text appended after the plan (ADR-0009) |
| `repository_id` | Which registered repo this runs against |
| `column`, `position` | Board placement; position is priority within the column |
| `run_state`, `branch`, `worktree_path` | Execution state (ADR-0005) |
| `links[]` | Zero or more `{label, url}` — Asana, GitHub issue, doc, anything |
| `depends_on[]` | Edges to other tasks (ADR-0008) |
| `model`, `effort` | Optional per-task overrides |

### Ordering

`position` is a **fractional float** within `(column, repository)`. Inserting between two
cards takes the midpoint; no neighbours are rewritten. A rebalance pass renormalizes to
evenly spaced integers when adjacent positions get too close to represent.

**Board order is execution order.** Sequential runs take `ready` tasks top-down. There is
no separate priority field, because two orderings always diverge.

## Consequences

- Dragging a card up is the whole priority interaction. Nothing else to keep in sync.
- The board stays exactly as specified — four columns — while running, failed, and
  blocked states are still visible, as badges on the card.
- A "Runs" view is needed alongside the board to show what is executing right now with
  live logs; the board alone under-serves that (ADR-0013).
- Fractional ordering makes drag-and-drop a single-row update, at the cost of a rebalance
  path that must exist and be tested.
- Failed tasks accumulate in `ready` unless the user acts. That is deliberate: a failure
  should interrupt the morning review, not hide in a column.

## Alternatives considered

- **A fifth `in_progress` column.** Truer to Kanban orthodoxy, but the user specified
  four, and it forces failed/blocked/waiting into either more columns or a badge anyway —
  so the badge is doing the work regardless.
- **Integer `position` with reindexing.** Every reorder rewrites a column's worth of rows;
  more write contention and more chances to corrupt order under concurrent MCP writes.
- **Separate `priority` integer.** Two sources of truth for one question.
