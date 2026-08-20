---
id: "017"
title: Morning review flow
milestone: v0.3
status: ready
depends_on: ["015"]
adrs: ["0007", "0008", "0013"]
size: M
---

# Morning review flow

## Goal

Turn "open the app and figure out what happened last night" into a single screen and a
short sequence of decisions.

## Why now

The workflow the product is named for. Once several nights of runs exist, the board alone
under-serves the review.

## Scope

**Overnight summary**

- Opening the app after a queue has run shows a digest: completed, failed, blocked,
  skipped; total duration and cost; the tasks needing attention first.
- Failures and blocked chains lead, because they are what changes the day's plan.

**Review queue**

- Walk `in_review` tasks one at a time, in board order, with per-task actions:
  - **Approve** → `done`
  - **Reject** → back to `ready` with a note appended to extra instructions for the next
    run
  - **Needs changes** → `ready`, with the note, keeping the existing worktree and branch
    so the next run continues rather than restarting
  - **Open PR** in the browser
  - **Open worktree** in the configured editor
- Keyboard-driven: approve, reject, next, previous.
- Each task shows the ADR-0013 review order — diff, commits, PR — without navigation.

**Chain awareness**

- When a reviewed task has dependents that already built on it (ADR-0008), the review view
  says so explicitly, and rejecting it warns which downstream tasks are affected.

**Optional**

- Configurable external editor command per repository, for "open worktree".
- A copyable summary of the night's results.

## Acceptance criteria

- After a queue of six tasks, the summary correctly reports each outcome.
- The review queue moves through `in_review` tasks with keyboard only.
- "Needs changes" preserves the worktree and branch, and the next run resumes there with
  the note included in its prompt.
- Rejecting a task with dependents warns and names them.
- Approving moves the task to `done` and advances to the next.

## Notes

This is the task where the product either feels good or feels like a database with a
board. Time spent here is well spent — but only after there are real nights of results to
design against.
