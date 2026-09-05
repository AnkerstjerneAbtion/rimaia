---
id: "023"
title: Batch strategy planning as a preflight
milestone: v0.2
status: ready
depends_on: ["020", "012"]
adrs: ["0016", "0021", "0010"]
size: M
landed: "#28"
---

# Batch strategy planning as a preflight

## Goal

Plan a whole column — or a hand-picked set of cards — in one action, so the user can read
every proposal before leaving, rather than discovering in the morning that a task was
modelled wrong and ran with the wrong budget.

## Why now

Task 020 made planning a per-task thing that happens *inside* a run. That is the right
place for it, and it leaves a gap: the proposal only exists once the expensive run has
already started, so the cheap check that would have caught a badly-written plan arrives
too late to act on.

A planner costs about **4¢ and four turns** — measured, not estimated. An implementation
run of the same task costs on the order of a dollar. Spending 40¢ to see how ten cards are
modelled, before committing forty dollars of implementation, is the best-value check this
product can offer, and it is the one the user actually asked for: *"before I go home, run
the plan, to make sure everything is correctly modelled, just so I can fast verify."*

It goes here, next to task 012, because both are blocked on the same unanswered question
(see **Notes**) and answering it twice would be worse than answering it once.

## Scope

**Core**

- `strategy::plan_all(ctx, paths, config, selection) -> ...` in `crates/core/src/strategy/`
  or `crates/core/src/runner/strategy.rs`, driving the existing `plan_task` over a set.
- `PlanSelection`: a column, a repository, an explicit list of task ids, or a combination.
  Whatever shape it takes, the *filter* is a core concern so the board and the MCP server
  cannot disagree about what "the ready column" means.
- **Sequential by default.** One planner at a time. Concurrency here is task 012's
  `max_concurrency`, not a second knob — a preflight that saturates the subscription limit
  is worse than one that takes two minutes.
- Skips, each reported rather than silent: a task that already carries a proposal (task
  020's re-plan guard), a task whose resolved mode is not `planned`, a task the queue or
  another planner is already running, and a repository without the unattended-runs opt-in.
- Per-task outcome streamed or collected — planned, skipped with a reason, or failed with
  one — so both surfaces can show progress rather than a spinner.

**UI**

- A "Plan all" action on the board toolbar, scoped by the column filter already there,
  plus multi-select on cards for a hand-picked set.
- Live progress: which card is being planned, how many are left, and the running total
  spent, since the whole point is that the user chose to spend it.
- A summary at the end that is worth reading before going home: each card, its chosen
  model and effort, and its rationale in one line — with the ones that were skipped and
  why, because an empty column silently doing nothing is the failure mode.
- Cancellable mid-pass, leaving the proposals already written in place.

**MCP (ADR-0021)**

- `plan_tasks_strategy`, taking the same `PlanSelection`, operator scope only.
- This is what closes ADR-0021's named gap. `plan_task_strategy` was left off the tool
  surface because it spawns a process; once ownership moves into core (see **Notes**),
  both the single and batch forms become expressible, and the parity rule is satisfied in
  one move rather than two.

## Out of scope

- Planning as part of the scheduled queue's own start. Task 013 owns the pre-flight
  summary that runs when a schedule fires; this task is the manual, run-it-now version,
  and 013 should reuse `plan_all` rather than grow its own.
- Re-planning a task that already has a proposal. "Re-plan" is per-card and already
  exists; a batch pass that quietly overwrote proposals the user had accepted would be the
  opposite of a review aid.
- Any change to how a strategy is *decided*. That is ADR-0016 and task 020, unchanged.

## Acceptance criteria

- Planning the `ready` column plans every eligible card in it, sequentially, and reports a
  per-card outcome for all of them including the ones it skipped.
- A column with nothing eligible reports that plainly — not a success, not an error, and
  never a silent no-op.
- A card already carrying a proposal is skipped with that named as the reason, and its
  existing proposal is untouched.
- A pass and the queue cannot start two processes for one task, in either order, and this
  holds for the MCP path as well as the button.
- Cancelling mid-pass stops before the next planner and keeps every proposal already
  written.
- The same selection through the MCP tool and through the button produces the same set of
  tasks, proven by a test that drives both.
- The end-of-pass summary shows model, effort and rationale per card, and the total spent.

## Notes

**The blocker, named.** `plan_task` deliberately takes no `run_state` claim — a task being
planned is not a task being run — and relies on `RunRegistry` in `src-tauri` to stop a
second start. The queue does not consult that registry; it claims on the database row. So
today a planner and an implementation run *can* be started for the same task, and a batch
pass makes that easy rather than unlikely.

Fixing it means deciding **who owns "is this task already in flight" in `rimaia-core`**,
which is the same question task 012 needs for its slot map and task 014 needs for
`waiting_retry`. That is why this task depends on 012 and why bundling the two is worth
considering: 012 has to build the ownership, and this task is the first thing that would
otherwise duplicate it.

**Do not reach for parallelism to make this fast.** Ten cards at fifteen seconds is two and
a half minutes, once, while the user packs up. The temptation is to fan out; the cost is
that a preflight becomes the thing that trips the usage limit the evening's real work
needed.
