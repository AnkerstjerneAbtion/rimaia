---
id: "011"
title: Task dependencies and blocking
milestone: v0.2
status: ready
landed: "#15"
depends_on: ["009"]
adrs: ["0008"]
size: M
---

# Task dependencies and blocking

## Goal

Declare that a task is blocked by another, gate execution on it, and branch dependent work
from its dependency so chained tasks build on each other.

## Why now

Multi-task plans are the normal case for an evening queue, and without chaining the second
task writes code against an API that isn't there.

## Scope

**Semantics (ADR-0008)**

- A dependency is satisfied when its run **completes successfully** — `in_review` or
  `done`. Not on human review.
- A task with unsatisfied dependencies has `run_state = blocked` and is skipped by the
  scheduler, not failed.
- A failed dependency leaves dependents blocked, and is surfaced on every dependent card
  so a single glance shows the stalled chain.
- Cross-repository dependencies are rejected at write time. **Shipped by task 010**
  (seam-contract D16) in `tasks::dependencies::set_task_dependencies`.

**Graph**

- Cycle detection on every edge write, in the service layer so both UI and MCP get it,
  with an error naming the offending path. **Shipped by task 010** (seam-contract D16):
  `set_task_dependencies` is on ADR-0006's tool table, and a tool that stores an edge
  without checking for a cycle is not the tool that table names. Read
  `crates/core/src/tasks/dependencies.rs` before adding anything here — this task extends
  it, it does not re-implement it.
- `blocking_reason(task) -> Option<Vec<Task>>` for display.
- Deleting a task with dependents is refused (already in 004; extend the message with the
  dependency context).

**Branch chaining**

- `worktree::prepare` resolves the base ref: no dependencies → repository default branch;
  one or more → the branch of the highest-position dependency.
- Multiple dependencies: base from the highest-position one, and a visible warning on the
  task that the others are not merged into its base.
- The chosen base ref is recorded on the run so the review view can show it.

**UI**

- Dependency editor in the task detail panel: search and pick tasks in the same
  repository; shows resolved status per edge.
- Board cards show a blocked badge with the blocking task's title.
- Optional: a small chain visualization in the task panel. Nice, not required.

**Scheduler**

- One added predicate in selection. If this is more than a small change, task 009's
  selection needs refactoring first, not working around.

## Acceptance criteria

- A → B → C in `ready` run in dependency order in a single unattended queue run, with no
  human interaction, and C's branch contains A's and B's commits.
- Creating a cycle is rejected in both UI and MCP, naming the path. Task 010 landed this
  for the MCP path and for the service beneath it (seam-contract D16); what remains here is
  the UI path reaching the same function and the panel rendering the refusal.
- A failing A leaves B and C blocked, each showing A as the reason.
- A dependent task's worktree is created from its dependency's branch, verified by
  `git merge-base`.
- A task with two dependencies runs, bases off the higher-priority one, and shows the
  warning.

## Notes

The unreviewed-work risk is real and accepted (ADR-0008). If it bites in practice, the fix
is a per-task `require_review` flag — but wait until it actually bites.
