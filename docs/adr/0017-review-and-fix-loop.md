# 17. Post-implementation review-and-fix loop

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

An unattended implementation run produces a branch. Whether that branch is any good is
currently discovered by a human, in the morning, which is the expensive part of the loop.

Much of what a morning review catches is mechanical: the tests were not run, a case in the
plan was missed, an obvious bug, a lint failure, a half-finished edit. An agent with fresh
context and a review brief catches most of it — and can fix it — while the human is
asleep. What should reach the morning is the work that needs judgement, not the work that
needs another pass.

This lands late. It only pays off once implementation runs are reliably producing
reviewable branches, and it multiplies token spend per task.

## Decision

After a successful implementation run, a task may enter a bounded **review-and-fix loop**
before it reaches `in_review`.

### The loop

```
implement → review → findings? → fix → review → … → clean, or loop budget spent
```

- **Review phase**: a fresh Claude Code session in the same worktree, with no
  implementation context, given the diff, the original plan, and the review instructions.
  Fresh context is the point — a session reviewing its own work grades itself generously.
- **Findings** are written back to the task through the scoped MCP handle from ADR-0016,
  each with severity and location.
- **Fix phase**: a run — resumed implementation session or fresh, configurable — that
  addresses the findings and commits.
- **Loop budget**: `max_review_loops`, default 2. Also bounded by the run window.
- **Exit**: review returns no findings above the configured severity → `in_review`, clean.
  Budget spent with findings remaining → `in_review` with the findings attached, flagged.
  Review run fails → `in_review` unreviewed, flagged. **The loop never sends a task to
  `done`**; a human still approves.

### Review instructions

A global `review_instructions` setting, alongside base instructions (ADR-0009), plus an
optional per-task override. Composed the same way, into the review prompt.

Because runs execute the user's own Claude Code, review instructions may simply invoke an
existing review skill or slash command by name. Rimaia does not ship a review methodology
— it schedules whatever the user already trusts.

### Configuration

Per task and per repository: loop enabled, `max_review_loops`, severity threshold for what
counts as blocking, model and effort for the review phase (a review is often worth more
effort than the implementation was), and whether the fix phase resumes or starts fresh.

### What the morning sees

The review history is part of the task: each loop's findings, what was fixed, what
remains. The review view (task 015) shows the final diff plus the unresolved findings —
so the human starts from "here is what the reviewer could not fix" rather than from
nothing.

## Consequences

- Morning review starts higher up the stack. This is the point.
- Token cost per task multiplies by roughly the loop count. Hence: off by default, opt-in
  per repository or per task, and explicitly bounded.
- Fresh-context review catches what self-review does not, at the cost of the reviewer not
  knowing why a decision was made. Findings are advisory to the fix phase, not commands —
  the fix run is told it may reject a finding with a reason, and the rejection is recorded.
- A loop that ping-pongs (fix introduces a new finding, review flags it, repeat) is
  bounded by the budget rather than by cleverness. Ping-ponging is itself a signal worth
  surfacing on the card.
- This composes with ADR-0016: the review phase is a natural place for a higher-effort
  model than the implementation used.
- Risk of false confidence — a task marked "reviewed, clean" that is not. Mitigated by
  never auto-advancing to `done`, and by showing loop count and findings history rather
  than a green tick.

## Alternatives considered

- **Review as a separate task type on the board.** Fits the existing model, and makes
  every implementation task require a manually created partner task. More board noise, and
  the dependency chain does the wrong thing on failure.
- **Self-review inside the implementation run** ("check your work before finishing").
  Nearly free, and the weakest form — same context, same blind spots. Worth keeping in the
  base instructions regardless; it is not a substitute.
- **Unbounded looping until clean.** Sounds better, ends with a task consuming the entire
  night on a finding it cannot fix.
- **Review without fix** (report only). Cheaper and safer, and leaves the human doing the
  mechanical work the loop exists to remove. Available as a configuration —
  `max_review_loops: 0` with review enabled — rather than as the design.
