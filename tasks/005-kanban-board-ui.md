---
id: "005"
title: Kanban board UI
milestone: mvp
status: ready
landed: "#3"
depends_on: ["004"]
adrs: ["0007", "0018"]
size: L
---

# Kanban board UI

## Goal

The board: four columns, drag to reorder and to move between columns, and a task detail
panel where plans are written and read.

## Why now

It is how tasks get created and prioritized before there is an MCP server, and priority
order is what the queue consumes in task 009.

## Scope

**Board**

- Four columns in order: Not ready for implementation · Ready for implementation ·
  In review · Done.
- Repository filter (all repositories, or one).
- Drag and drop within a column (reorder) and across columns (move). Optimistic update,
  reconciled against the backend response; a rejected move snaps back with a reason.
- Card shows: title, repository, a run-state badge, link count, dependency indicator, and
  relative time of last activity.
- Run-state badges are visually distinct and unambiguous: `running` (animated),
  `queued`, `blocked`, `waiting_retry`, `failed`, `cancelled`. `idle` shows nothing.
- Empty column states that say what belongs there.
- Live refresh on `tasks:changed`.

**Task detail panel**

- Title, repository selector.
- Plan editor: Markdown textarea with a preview toggle. This is the main writing surface —
  it needs to be pleasant for a long plan, not a one-line input.
- Extra instructions: short textarea, with a note that it is appended after the plan.
- Links: add, edit, remove, reorder. `{label, url}`; label defaults to the URL host.
- Model and effort overrides, both optional, defaulting to Rimaia's own defaults. Plain
  dropdowns here; task 020 replaces this with the full execution-strategy control
  (ADR-0016), so keep it in one component.
- Read-only run info: branch, worktree path, last run outcome.
- Delete, with confirmation.

**Keyboard**

- `n` new task, `/` search titles, `Esc` closes the panel, arrows move focus between
  cards.

## Out of scope

- Dependency editing UI (011).
- Run log display (008).
- Bulk operations.

## Acceptance criteria

- Dragging a card to the top of `ready` makes it the next task the queue will pick.
- Order survives restart.
- Moving a task with an empty plan to `ready` is rejected, with the reason shown.
- A task created from another window (or later, over MCP) appears without a manual
  refresh.
- A 400-line plan is comfortable to write and read in the panel.
- The board is usable at 1280px wide.

## Notes

Prefer a small headless drag-and-drop library (`dnd-kit`) over hand-rolling pointer
events; the cross-column drop and keyboard accessibility are more work than they look.

Ordering is the load-bearing interaction — it is the priority mechanism (ADR-0007). Spend
the polish there rather than on card decoration.
