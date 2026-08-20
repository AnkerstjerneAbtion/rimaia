---
id: "009"
title: Sequential run queue
milestone: mvp
status: ready
depends_on: ["008"]
adrs: ["0010", "0007"]
size: M
---

# Sequential run queue

## Goal

Work the `ready` column top-down, one task at a time, unattended. **This closes the MVP
loop.**

## Why now

It is the difference between "I can run a task" and "I can leave and come back to results
in the morning".

## Scope

**Scheduler (`scheduler/`)**

- One long-lived Tokio task owning the queue for the process lifetime.
- Selection: highest-position `ready` task whose repository allows unattended runs and
  which is not already `queued`, `running`, or `waiting_retry`. Skips tasks whose repo has
  not opted in, with the reason surfaced on the card rather than silently.
- Claim-and-transition in a single database transaction, so the UI and the scheduler
  cannot double-start a task.
- Sequential only: the next task starts when the previous reaches a terminal state.
- Between tasks, re-read the board — a task dragged to the top mid-queue is picked up next.

**Control**

- Start queue, Pause (finish the current run, start nothing new), Resume, Stop
  (pause + cancel the running task).
- Queue state derived from the database, so it survives app restart. Tasks left `running`
  by a crash are marked `interrupted` at startup and surfaced for the user to act on.

**UI**

- Runs view: queue status, what is running, the ordered list of what is next, and what
  completed this session with its outcome.
- Board cards show `queued` position.

## Out of scope

- Parallelism (012), time-based triggers (013), retries (014), dependency gating (011) —
  though selection is written so 011 can add one predicate rather than rewriting it.

## Acceptance criteria

- Five tasks in `ready` run in board order without intervention; each ends in `in_review`
  or `failed`.
- A failing task does not stop the queue; the next task starts.
- Reordering the board mid-queue changes what runs next.
- Pause lets the current run finish and starts nothing new. Stop cancels it.
- Killing the app mid-queue and reopening it shows accurate state: one `interrupted` task,
  the rest untouched.
- Concurrent start attempts (UI button plus scheduler) never produce two processes for one
  task.

## Notes

**MVP boundary.** When this passes, use it for a real evening's work before continuing.
The next tasks should be ordered by what that evening actually reveals — the current
order (MCP first, then dependencies, then resilience) is the best guess, not a commitment.
