# 22. What a run is remembered by, and what survives pruning

- **Status:** Accepted
- **Date:** 2026-09-01

## Context

A `runs` row currently records how one attempt went: status, exit class, turn count,
cost, timings, the composed prompt, and where its transcript lives. That is enough to
review a run the next morning, which is what ADR-0013 asked for.

It is not enough to answer questions *about runs in general* — what a month cost against
the subscription that paid for it, which models the work actually ran on, whether runs are
getting longer. Three things stand in the way, and they are not the same kind of problem.

**The model a run used is not recorded anywhere.** `tasks.model` is the model a task would
use *now*. A planner rewrites it, and a human editing the card rewrites it again, so the
column answers a question about the present and cannot answer one about the past. A run
that executed on Opus in August reads as Haiku today if somebody changed the card.

**Token counts are dropped on the floor.** The `result` event carries usage, and
`runner::outcome` reads what it needs and discards the rest. Cost in dollars survives;
what produced it does not — so "input versus output", "how much was cache", and any
attempt to reason about price changes are unanswerable.

**Pruning has no stated relationship to history.** Task 015 adds a prune action over run
logs, and task 016 adds one over worktrees. Whether "prune" means *delete the transcript
file* or *delete the run* has never been decided, and the two answers give completely
different products: one where a year of history costs a few kilobytes a run, and one where
reclaiming disk silently destroys the record.

The forcing detail: **history cannot be backfilled.** Every run executed before this
decision lands is permanently missing its model and its tokens. Deferring the capture
until an analytics page is built does not delay the cost — it decides that the first N
months are blank.

## Decision

**A `runs` row is the permanent record of one attempt. Its transcript is a cache.**

Three parts.

### 1. A run records what it ran as, at the time it ran

New columns on `runs`, written once by `finish_run` and never updated:

| Column | Why it cannot be derived later |
| --- | --- |
| `model` | `tasks.model` is the present tense; a planner or a human rewrites it |
| `effort` | same |
| `run_environment` | `inherit` or `strict_local` was a setting when the run started, and settings change |
| `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens` | dropped from the `result` event today; the only source is the run that is ending |

These are nullable, because every row written before the migration honestly has none, and
because a run that dies before its `result` never learns them. **NULL means "not
recorded", never zero** — an analytics view that averages a NULL as zero is lying about
the past, and the seam contract should say so.

### 2. Pruning removes transcripts, never rows

`prune` (task 015) deletes **JSONL files** and sets a marker on the row. It does not delete
`runs` rows, and neither does task 016's worktree cleanup.

A transcript is tens of megabytes and is read a handful of times, in the week after a run,
by someone chasing a specific question. A row is a few hundred bytes and is read forever.
Treating them the same because they arrive together is the mistake this entry exists to
prevent — reclaiming disk should not cost the record of what was spent.

Task 015 already has the marker: its "runs rows whose log file is missing are marked, not
trusted" is exactly this state, and pruning reaches it deliberately rather than by
accident.

**Deleting a task still cascades to its runs.** That is a person saying "this never
happened", which is different from the disk being full, and `ON DELETE CASCADE` on
`runs.task_id` already says so.

### 3. Analytics reads, and never writes

There is no aggregates table, no rollup job, and no counter maintained on the side. Every
number an analytics view shows is a `SELECT` over `runs` at read time.

A few hundred rows a month is nothing to SQLite, and a maintained counter is a second
source of truth that drifts the first time a write path forgets it — the same argument
seam-contract D12 makes for computing the board's counts in the query rather than caching
them. If a query ever becomes slow enough to matter, an index is the answer before a table
is.

## Consequences

- **Capture lands before the page that uses it, and should.** The columns are worth adding
  as soon as the next migration is written; the analytics view can arrive whenever. The
  reverse order throws away months of data for no gain.
- One migration, adding seven nullable columns. Seam-contract D4 caps the count and names
  each exception, so this needs an entry there.
- `finish_run` grows the only write. It is already the single writer of a run's terminal
  state, which is where this belongs.
- **Analytics over a period is honest only for the period since the columns existed.** A
  view showing "models used" across a range that predates the migration must say the
  earlier part is unrecorded rather than silently reporting a smaller total.
- Comparing spend against a subscription price needs the price, which Rimaia does not know
  and cannot discover. It becomes a setting the user types, and it is a number *they*
  assert — the app should present it as their figure, not as a fact it verified.
- Keeping rows forever is a real, if small, growth: a run a day for five years is under two
  megabytes. Worth stating so nobody re-litigates it as a leak.

## Alternatives considered

- **Derive the model from the task.** Free, and wrong in the one direction that matters: it
  silently rewrites history whenever a card is edited, and the wrongness is invisible
  because the number still looks plausible.
- **Parse the transcripts on demand.** No schema change, and it makes analytics depend on
  the files pruning is designed to delete — so the feature would degrade exactly as the
  product is used more.
- **A rollup table maintained on write.** Fast, and a second source of truth. Rejected for
  D12's reason: the first write path that forgets to update it produces numbers that are
  wrong and confident.
- **Store the whole `result` event as JSON.** Tempting, and it defers the schema question
  rather than answering it. Named columns are queryable and typed; a blob is a promise to
  write a parser later.
