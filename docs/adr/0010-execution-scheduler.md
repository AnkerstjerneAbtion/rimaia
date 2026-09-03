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

## Amendment, 2026-09-03 — what task 013 learned building the windows

Three refinements. None of them changes a decision above; each makes one of them
implementable in a way the original wording left open.

### A schedule is a standing instruction, and it survives quitting

The Control section says queue state is derived from the database and seam-contract D15
made "quitting always stops the queue" the rule. Both still hold: `queue_state` is written
`paused` on exit and a launch starts `paused`. But an **enabled `schedules` row is not queue
state** — it is an instruction the user gave in advance, and it survives quitting the way a
task on the board does.

So there are now two explicit ways to give the go signal, and they are the same signal: the
Start button, and a row that says "every night at 22:00". Neither is inferred. What follows
is that a schedule whose time passed while the app was closed **fires on next launch**, and
that quitting mid-window **closes the window** while leaving the schedule alone — the night
ends, the standing instruction does not. D15's 2026-09-03 amendment carries the detail,
including why a *crash* deliberately does not close the window when a deliberate quit does.

### Late firing coalesces, and is bounded by the window's own stop time

The Consequences say "the scheduler fires late rather than skipping". That sentence was
written about a machine asleep at 22:05, and read literally it also describes a machine
asleep for a week — which would owe seven fires.

It owes one. The scheduler asks for the **most recent** occurrence at or before now, so N
missed occurrences produce exactly one fire, and it is the newest. Honouring the oldest
instead is the reading that never runs at all: its window's stop time was several mornings
ago.

And the newest is bounded by that same stop time. A laptop opened at 11:00 on a schedule
that runs 22:00 to 06:00 has genuinely missed the night, and starting a full night of
unattended work in the middle of a working morning is not what firing late was meant to
mean. Such an occurrence is recorded as missed and **not** written to `last_fired_at`, which
means "it fired" and must not be used as a bookmark for something that did not.

A schedule with **no** stop time has no window to have missed, so it fires however late it
is, unqualified — which is the original sentence, still true wherever it was the only thing
that could be meant.

### Reaching the stop time is `pause`, not `stop`

The run-window paragraph already says reaching the stop time "stops *starting* new tasks;
in-flight runs are allowed to finish rather than being killed mid-edit". Stated as a verb,
because this codebase has both and they differ by exactly that: `stop` is pause **plus**
cancelling what is in flight. A window that closed with `stop` would kill a run three
minutes from a commit, which is the outcome that paragraph exists to forbid.

The window is cleared at the same moment, so the queue does not read a night that is over as
a night still in progress. `pause` and `stop` pressed by hand clear it too — Stop inside a
window means stop, not "stop until the timer looks again".

### One thing this amendment does not change

"Runs left `running` at a crash are eligible for resume" (above) still means *offered*, not
performed, and a schedule firing does not make it automatic. A fire runs the doctor, writes
the window and flips the switch; it moves no task's `run_state`. Whether any individual task
resumes stays ADR-0011's per-run decision, taken inside an open window with the switch on —
identically to what the Start button produces. Settled in D15's amendment.
