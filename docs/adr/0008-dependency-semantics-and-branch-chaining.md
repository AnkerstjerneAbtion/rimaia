# 8. Dependencies unblock on successful run, with branch chaining

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

Tasks can depend on each other: "add the API endpoint" must land before "call it from the
UI". Rimaia needs to refuse to start a task until its dependencies are satisfied.

The obvious definition — a dependency is satisfied when it reaches `Done` — breaks the
product's core loop. `Done` means *the user reviewed it*, and review happens in the
morning. An overnight queue of five chained tasks would run exactly one task and then
block until the human wakes up. The entire point is that the queue runs while nobody is
watching.

There is also a second question dependencies raise: what does the dependent task branch
*from*? If B depends on A and both branch from `main`, B does its work without A's code
present, and the PRs conflict.

## Decision

**A dependency is satisfied when the dependency's run completes successfully** — that is,
when it reaches `in_review` or `done`. Not when a human approves it.

**A dependent task's worktree branches from its dependency's branch, not from the default
branch.** With multiple dependencies, the task branches from the highest-priority
dependency's branch (board order) and the others are surfaced as an explicit warning that
the user should either merge them or serialize the work.

Supporting rules:

- Edges are stored as `task_dependencies(task_id, depends_on_task_id)`. Cycles are
  rejected at write time, in both the UI and the MCP server, with the offending path
  reported.
- A task whose dependencies are unsatisfied has `run_state = blocked` and is skipped by
  the queue rather than failing. The card shows which task is blocking it.
- If a dependency **fails**, its dependents stay blocked. A failed dependency is surfaced
  on every dependent card, so one morning glance shows the whole stalled chain.
- Deleting a task with dependents is refused; the edges must be removed first.
- Cross-repository dependencies are rejected. A dependency implies a shared branch base.

## Consequences

- Overnight chains actually run. This is the reason for the decision, and it is the whole
  behavioural difference between a working product and a demo.
- **Unreviewed work becomes the foundation for later work.** If A is subtly wrong, B
  builds on it and both need fixing. This is a real cost, accepted deliberately: it is the
  same tradeoff as a human stacking PRs, and the alternative is a queue that stops.
- Branch chaining makes the resulting PRs stacked. Reviewing them in order is natural;
  merging them out of order is not. The review view shows the chain (task 017).
- Post-MVP escape hatch: a per-task `require_review: bool` for work where building on
  unreviewed code is unacceptable. Deliberately not in MVP — adding it before the default
  has been lived with would be guessing.
- Multi-dependency tasks are the sharp edge. Warning plus documented behaviour for MVP;
  an explicit "merge dependencies into base" step is the eventual fix.

## Alternatives considered

- **Satisfied only at `Done`.** Correct in the strictest sense, useless in practice: it
  makes an overnight queue single-task.
- **Per-edge configurable semantics.** Two behaviours, twice the UI, and the user has to
  reason about which one each edge uses. Not worth it before there is evidence both are
  needed.
- **Branch every task from the default branch regardless.** Simpler worktree logic, and
  guarantees that dependent work is written against code that isn't there yet.
