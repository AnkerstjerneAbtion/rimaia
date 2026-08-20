# 10. Execution scheduler: sequential or parallel, run windows

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

The user leaves the office and wants a queue of tasks to run through the evening and
night. They need to choose between "one at a time, in board order" (safer, and correct
when tasks touch the same code) and "several at once" (faster, and fine when tasks are
independent). They also need to start the queue now, or at a chosen time.

The scheduler is the component that decides *what runs next*, and it is the only component
allowed to move a task into `running`.

## Decision

A single **scheduler task** owns the run queue for the process lifetime.

### Modes

| Mode | Behaviour |
| --- | --- |
| `sequential` | One run at a time. Next task starts when the previous reaches a terminal state |
| `parallel` | Up to `max_concurrency` runs at once (default 2, user-configurable) |

Mode and concurrency are properties of the **run configuration**, not of a task.

### Selection

The next task is the highest-position `ready` task, in board order, that is:

- not already `running`, `queued`, or `waiting_retry`,
- not `blocked` by an unsatisfied dependency (ADR-0008),
- in a repository that is not at its own concurrency limit.

**Per-repository concurrency is capped at 1 even in parallel mode** unless the user
explicitly opts out. Two agents in two worktrees of the same repo is safe for git, but
they will fight over ports, test databases, and lockfiles. Parallelism across
*repositories* is the safe default; within one repo it is opt-in.

Selection and the transition to `running` happen in a single database transaction, so the
UI, the MCP server, and the scheduler cannot double-claim a task.

### Triggering

- **Run now** — starts immediately.
- **Start at** — a wall-clock time, typically "18:30 today". Fires once.
- **Recurring** — a cron expression with a timezone, for a nightly queue.

A run window may specify a stop time. Reaching it stops *starting* new tasks; in-flight
runs are allowed to finish rather than being killed mid-edit.

### Control

Pause (finish current, start nothing new), resume, cancel-one (SIGTERM then SIGKILL, task
goes to `failed` with `cancelled` reason), cancel-all. Queue state survives an app restart
by being derived from the database; runs that were `running` when the app died are marked
`interrupted` at startup and are eligible for resume (ADR-0011).

## Consequences

- Board order is the only priority mechanism, consistent with ADR-0007.
- Sequential mode is the safe default and the one that matches "implement these in this
  order".
- Parallel mode's real constraint is not git, it is the developer environment — hence the
  per-repository cap.
- Everything the scheduler decides is recomputable from the database, so a crash loses at
  most the in-flight process, and that is resumable.
- Wall-clock scheduling inherits the usual DST and sleep-mode hazards: a machine asleep at
  the trigger time misses the window. The scheduler fires late rather than skipping, and
  the UI shows the next fire time so a wrong cron expression is visible before the night,
  not after.

## Alternatives considered

- **Unbounded parallelism.** Saturates the subscription rate limit, thrashes the machine,
  and turns one usage-limit wall into N simultaneous failures.
- **Per-task scheduling.** Every task gets its own time. More flexible, far more state,
  and the user's actual need is "run this list tonight".
- **An OS-level scheduler (launchd/cron) starting a headless process.** Runs when the app
  is closed, but splits ownership of the queue across two processes and complicates the
  single-writer story from ADR-0003. Worth revisiting only if unattended-with-app-closed
  becomes a requirement.
