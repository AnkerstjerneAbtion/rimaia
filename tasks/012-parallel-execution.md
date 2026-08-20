---
id: "012"
title: Parallel execution
milestone: v0.2
status: ready
depends_on: ["009"]
adrs: ["0010", "0005"]
size: M
---

# Parallel execution

## Goal

Run several independent tasks at once, bounded by a concurrency limit and by a
per-repository cap.

## Why now

An evening of independent tasks across several repositories finishes far sooner in
parallel, and worktree isolation (ADR-0005) already makes it safe at the git level.

## Scope

- Run mode setting: `sequential` (default) or `parallel` with `max_concurrency`
  (default 2).
- **Per-repository concurrency capped at 1** unless explicitly overridden per repository.
  Two agents in one repo will fight over ports, test databases, and lockfiles — git
  isolation does not help with those. Parallelism across repositories is the safe default.
- Scheduler fills available slots from the ordered `ready` list, respecting both caps and
  dependency gating.
- Slot release on any terminal state, including `waiting_retry` once 014 lands.
- Runs view shows N concurrent runs side by side, each with its own live log; switching
  between them is one click and does not lose scroll position.
- Global resource guard: a configurable ceiling regardless of mode, so a mis-set value
  cannot spawn ten agents.

## Out of scope

- Automatic concurrency tuning based on machine load.
- Detecting that two tasks touch the same files.

## Acceptance criteria

- With `max_concurrency = 3` and tasks across three repositories, three run concurrently.
- Two `ready` tasks in the *same* repository run sequentially by default, and concurrently
  once that repository's override is enabled.
- Cancelling one run does not disturb the others.
- Live logs stay attributed to the correct run under concurrency.
- Dependency gating still holds: a dependent task never starts before its dependency
  succeeds, regardless of free slots.

## Notes

The per-repository cap is the important detail here. It is the difference between
parallelism that works and an evening of tests failing on port conflicts.
