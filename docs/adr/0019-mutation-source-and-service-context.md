# 19. Mutation source, and where it lives on the service context

- **Status:** Accepted
- **Date:** 2026-08-26

## Context

[ADR-0006](0006-embedded-local-mcp-server.md) says that every mutation arriving over the
MCP server is "attributed (`source = "mcp"`)". Task 010's scope repeats it. Neither says
what attribution *is*: a column, a log field, an audit table, or a parameter — and the four
answers have very different costs.

By the time the MCP server lands there are three writers of the same tables through the
same services: the Tauri commands the user drives, the scheduler running the board
overnight, and MCP tool calls from Claude Code sessions elsewhere on the machine.
[ADR-0018](0018-core-to-shell-change-events.md) already established the shape a
cross-cutting capability takes in this codebase — it travels on `ServiceContext` with the
pool and the clock, because it is an ambient property of *being* a service rather than an
argument each caller remembers to pass. The question this record settles is whether
"which door did this write come through" is that kind of capability, and what part of it
is durable.

There is also a bookkeeping problem. Seam-contract [D4](../seam-contract.md#d4--migration-file-numbering)
fixes the MVP at exactly two migrations and forbids a third outright, so any answer
involving a column has to say why it is allowed to be the exception and why the
prohibition still binds everything else.

## Decision

### `MutationSource` is a field on `ServiceContext`

```rust
pub enum MutationSource { Ui, Mcp, System }

pub struct ServiceContext {
    pub pool: SqlitePool,
    pub clock: Arc<dyn Clock>,
    pub changes: broadcast::Sender<ChangeEvent>,
    pub tail: broadcast::Sender<RunTail>,
    pub source: MutationSource,
}
```

ADR-0018's three-field snippet was illustrative and is now two fields out of date: the
struct gained `tail` in seam-contract D14 and gains `source` here. **This record fixes the
current shape**; a later field is a later record, not a silent edit to this one.

`ServiceContext::new` takes the source rather than defaulting it, and there is deliberately
no `Default` impl. Every plausible default is wrong somewhere — `Ui` is wrong for the
scheduler, `System` is wrong for the shell — and a field that is wrong by omission is worse
than one the compiler makes you name.

### Each subsystem re-sources its own clone

`scheduler::build` and `mcp::build` each call `ctx.with_source(...)` on the context they
are handed, and hold the re-sourced clone. The shell builds exactly one `Ui` context in
`setup()` and never thinks about the field again.

This is the same argument ADR-0018 makes about publishing. Being the scheduler is not a
fact a caller passes in at each call site; it is what that subsystem *is*, for every write
it will ever make. A parameter would put "was this attributed correctly?" at the call site
— dozens of them, each able to be wrong on its own — where the context puts it at the one
place a subsystem is constructed.

`with_source` returns `Self { source, ..self.clone() }`, so the clone keeps the *same*
broadcast senders. That matters more than it looks: a context that published somewhere
else would be a board that never refreshes for MCP writes, which is precisely the
requirement ADR-0006 and ADR-0018 exist to satisfy. It has its own test.

### `tasks.source` is creation provenance, and is never rewritten

A third migration adds:

```sql
ALTER TABLE tasks
    ADD COLUMN source TEXT NOT NULL DEFAULT 'ui'
    CHECK (source IN ('ui', 'mcp', 'system'));
```

`create_task` binds `ctx.source`. **No other statement writes the column, ever.** The value
on a row answers "where did this task come from", which is a question about the task, and
not "who touched it last", which is a question about an event.

### ADR-0006's "every mutation is attributed" is satisfied by the tracing span

This is the load-bearing sentence of this record, and without it the schema reads as a
deviation from ADR-0006 rather than an implementation of it.

Every mutating service function carries
`#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), ...))]`. That is what
makes the ADR-0006 sentence literally true for *every* mutation — moves, patches, link
edits, dependency writes, run-state transitions — not only for the one that happens to
create a row. `<app-data>/logs/` holds the attribution; the column holds provenance. Read
paths get no span: they are not mutations, and instrumenting a board read once per
`tasks:changed` is noise that would bury the lines that matter.

## Consequences

- A task's origin is visible on the board and in the `sqlite3` CLI, which is what makes
  "a session on this machine wrote to my queue overnight" answerable at all.
- The column is `NOT NULL DEFAULT 'ui'` and therefore backfills every pre-existing row
  correctly and in O(1): before this migration the board was the only writer that existed,
  so `ui` is not a guess, it is the fact.
- Adding a fourth source later is a `CHECK` constraint change, which SQLite cannot alter in
  place — it is the twelve-step table rebuild `db::models` warns about. The three values
  are the three doors that exist by design (ADR-0006, ADR-0010, ADR-0018); a fourth would
  be a new kind of writer, which is an architectural change anyway.
- `ServiceContext::new` gaining a parameter is a compile error at every construction site,
  which is the point: three of them exist (the shell, the test harness, and the scheduler's
  re-source), and each had to answer the question.
- Nothing enforces that a mutating function carries the span. Same class of gap ADR-0018
  accepts for publishing, and mitigated the same way: mutations go through few services.

### The D4 exception

Seam-contract D4 fixes the MVP at two migrations and forbids a third. This record is the
**named exception**: `src-tauri/migrations/20260826120000_task_source.sql` is the third and
the last, added by task 010.

The prohibition still binds every other task, and for the reason D4 gives — two agents in
separate worktrees each reaching for "the next timestamp" collide silently, and
append-only (ADR-0003) means a renumber is not available as a repair. This exception is
safe from that failure specifically because it is taken here, in a record, before the
migration was written, and because task 010 is the only task in flight that has one. A task
that believes it needs a fourth still stops and asks.

## Alternatives considered

- **A last-writer column.** Rejected, and this is the alternative most worth naming
  explicitly, because it is what "every mutation is attributed" sounds like it is asking
  for. It decays: the moment a task runs, the scheduler stamps it `system`, and the answer
  to "did an agent put this on my board" is gone. It also needs every `UPDATE` in five
  modules to remember to write the column, with no compiler check that any of them did —
  so its most likely steady state is a column that is *sometimes* the last writer, which
  is worse than either honest answer.
- **A `task_events` audit table.** The complete answer, and genuinely better if the
  question were ever "show me this task's history". It is a table, a write on every
  mutation, a retention policy in a process that runs all night, and a migration nobody
  asked for. The tracing span already answers the same question for a single-user desktop
  app, at zero schema cost, and it is already being written to disk.
- **`source` as a parameter on every service call.** Rejected for the reason ADR-0018
  rejected the same shape for publishing: it makes an ambient property of a subsystem into
  a per-call-site decision that can be wrong one call at a time.
- **Span only, with no column.** Tempting — it needs no migration and satisfies ADR-0006's
  sentence on its own. But provenance is a property of a task the user will want on screen
  (task 017's morning review, and the "what did the agent queue while I slept" question),
  and reconstructing it by grepping a rolling log file is not a feature, it is an excuse.
