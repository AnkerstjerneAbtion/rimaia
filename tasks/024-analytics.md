---
id: "024"
title: Analytics — what the queue has actually done
milestone: v0.4
status: ready
depends_on: ["015"]
adrs: ["0022", "0013"]
size: M
landed: "#28"
---

# Analytics — what the queue has actually done

## Goal

One page answering what Rimaia has actually done: how much it has cost, what it ran on,
how long it took, and how that compares to the subscription paying for it.

## Why now

Two different reasons, and they pull in different directions — which is why **the capture
half of this task should land long before the page**.

The page itself is a v0.4 nicety. Nobody is blocked on it, and a queue that works is worth
more than a chart about one that does.

The **columns** are not a nicety and are not deferrable. ADR-0022's forcing detail is that
history cannot be backfilled: the model a run used, and the tokens it spent, exist only in
the moment the run ends. Every night the queue runs without them is a night permanently
missing from any chart this task eventually draws. If a migration is being written for
another reason before this task starts, these columns should ride along with it.

## Scope

**Capture (ADR-0022, do this first and separately if possible) — ✅ landed, see below**

- The seven nullable columns on `runs`: `model`, `effort`, `run_environment`,
  `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`.
- Written once by `finish_run`, never updated. NULL means *not recorded*, never zero.
- A seam-contract entry naming the migration, per D4.

> **The capture half has shipped; the page has not, and this task is not landed.**
> The columns arrived in
> `src-tauri/migrations/20260902120000_dependencies_parallelism_scheduling_and_capture.sql`
> (seam-contract D4's 2026-09-02 amendment names it), `finish_run` is their only
> writer, and **seam-contract D18** states the NULL rule ADR-0022 asked the seam
> contract to carry. `runner::events::TokenUsage` reads the four counts off the
> terminal `result` event's `usage` object; `runner::outcome::SpawnedAs` carries
> the other three, filled by `execute` because it is the only place the
> `Invocation` and the run's own `init` event are both in scope.
>
> It shipped early on ADR-0022's own argument: history cannot be backfilled, so
> every night the queue ran without these columns would have been permanently
> blank. **The page below is untouched and is what this task still owes.** Do not
> mark this task landed until it exists.

**The page**

Read-only, computed at read time — no aggregates table, no rollup (ADR-0022 part 3). A
period selector (this week, this month, all time, custom range) scopes everything on it.

*Need to know*

- Total spend for the period, and spend per day as a small chart.
- **Against the subscription.** A setting holds what the user pays per month; the page
  shows spend as a share of it, and what the same work would have cost at API prices if
  that is knowable. Present the subscription figure as *theirs* — Rimaia cannot verify it.
- Runs by outcome: succeeded, failed, cancelled, interrupted. A failure rate that is
  climbing is the single most useful thing on this page.
- Tasks completed — reached `in_review` or `done` — against tasks attempted.

*Nice to know*

- Total and median run duration; the longest single run, named and linked.
- Turns: total, median, and the distribution's shape rather than only its average.
- Model mix by run count and by spend — the two rank differently and that difference is
  the interesting part.
- Cost per completed task, which is the number that actually answers "is this worth it".
- Strategy mix: how many runs were `default`, `manual`, `planned`, and whether planned
  ones cost less per completed task than the default would have.

*Fun to know*

- Total unattended time — hours the queue was working while nobody watched. This is the
  product's whole pitch, stated as a number.
- Longest single overnight streak, and the busiest night.
- Most-worked repository, and most-attempted task.
- Total planner spend against total implementation spend — the overhead of deciding versus
  doing.

## Out of scope

- Any export, report or scheduled digest. If someone wants the numbers elsewhere they can
  reach the MCP surface or the SQLite file.
- Per-token pricing tables. Prices change, Rimaia would be wrong quietly, and `cost_usd`
  from the `result` event is already authoritative for what a run cost.
- Charts that need a charting dependency heavy enough to argue about. Prefer what can be
  drawn honestly with layout and a little SVG; a bar chart is not worth a bundle.
- Any write. This page never mutates a row (ADR-0022 part 3).

## Acceptance criteria

- Every figure on the page is derived from `runs` at read time; no aggregate is stored.
- A period that predates the capture columns is **labelled as partly unrecorded** rather
  than reported as a smaller total — a NULL is never averaged as a zero.
- Spend for a period matches the sum of `cost_usd` over the same rows, checked against a
  direct SQL query in a test.
- The subscription comparison is absent, not zero, until the user has entered a figure.
- Pruning run logs (task 015) changes nothing on this page — the rows survive, and a test
  proves it by pruning and re-reading the same totals.
- Deleting a task removes its runs from the figures, because that cascade is a person
  saying it never happened.
- The page opens without a spinner on a database with a few thousand runs.

## Notes

**The one number that will be wrong if nobody thinks about it** is cost per completed
task. A task with four failed attempts and one success cost all five runs, and dividing
total spend by successes is the only honest way to say so. The tempting version — cost of
the successful run — flatters the product and hides exactly the thing worth knowing.

Everything here is a `SELECT`. The work is deciding *which* numbers are worth a person's
attention, and that is a design problem rather than an engineering one: a page with forty
statistics is a page nobody reads twice. If a number would not change a decision or make
somebody smile, leave it out.
