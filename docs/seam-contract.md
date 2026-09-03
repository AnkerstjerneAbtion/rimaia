# Seam contract

- **Status:** Living — entries are appended, never silently rewritten
- **Date:** 2026-08-20
- **Scope:** tasks 002–009 (the MVP), plus the later tasks an entry names

Eight MVP tasks share a database, an error type, a command boundary and a lockfile. Where two
of them touch the same seam, both need the same answer, and neither task file gives one. This
document gives it. Every entry below exists because an implementation agent would otherwise
have had to choose, and two agents choosing independently would not have chosen the same
thing.

**This is not an ADR.** ADRs shape the product and are argued at product scale; the decisions
here are too small or too local for that — which file a helper lives in, which task ships a
seed, how many migrations the MVP has. The distinction is not permanent: an entry that turns
out to be architectural graduates. Write the ADR, then leave a one-line pointer behind. D2
already did exactly that.

**An implementation task may not deviate from an entry silently.** Same rule CLAUDE.md states
for ADRs, for the same reason: the point is not that any one deviation was defensible, it is
that the next agent inherits the same decision. If an entry is wrong, stop and say so — amend
it, or write the ADR that supersedes it. Do not work around it in a commit.

---

## D1 — Where the fractional position math lives

**Question.** Task 002's scope lists `position_between(before, after)` and a rebalance routine.
Task 004's `move_task` "computes the fractional position, rebalances when needed, in one
transaction". Which task writes the arithmetic?

**Decision.** `crates/core/src/tasks/position.rs`, written by task 002. Task 004 calls it and
adds no math of its own: `move_task` owns the transaction and the neighbour lookup,
`position.rs` owns the numbers. The frontend never computes a position at all — task 005 sends
`before_id`/`after_id` and takes the resulting number back from the service.

**Why.** ADR-0015's must-have-tests list already names `tasks::position_between` and its
rebalance by that path, so the module is fixed by an accepted ADR rather than by preference.
Task 002 ships it with unit tests over pure floats, which is the only place the rebalance path
is cheap to force; task 004's integration tests then cover the transaction instead of
re-testing arithmetic. The alternative split — 002 storage, 004 math — leaves task 002's own
acceptance criterion ("`position_between` including the rebalance path") with nothing to test.

**Binds.** 002, 004, 005.

## D2 — How a change event crosses core → shell

**Question.** A `rimaia-core` service cannot depend on `tauri` (ADR-0015), and task 004 must
emit `tasks:changed` from inside one.

**Decision.** Not here — this one is architectural. **ADR-0018** in [`docs/adr/`](adr/README.md)
owns the seam between a core service mutation and a Tauri event, and every task that emits or
consumes one implements that ADR, not this file.

**Binds.** 004, 005, 008, 009.

## D3 — Who owns settings storage vs. the typed accessor

**Question.** Task 002 creates the `settings` key/value table. Task 006 needs
`base_instructions` with a seeded default; task 008 needs `run_environment`. Which task ships
the accessor, the key constants and the seed?

**Decision.** Task 002 ships the table and nothing else — no accessor, no constants, no seed.
Task 006 ships the typed `settings::get`/`settings::set`, the known-key constants
(`base_instructions`, `run_environment`) and the first-launch seed of the default base
instructions.

**Why.** Task 002's own Out of scope is "storage plus models only". An accessor that knows
which keys exist, what type each holds and what happens when one is absent is a business rule,
and business rules belong with the task that has the rules. Task 008 then reads
`run_environment` through task 006's accessor rather than through its own SQL, so
`inherit | strict_local` is parsed in one place instead of two.

**Binds.** 002, 006.

## D4 — Migration file numbering

**Question.** Which tasks add migration files, and under which names?

**Decision.** Exactly two migrations in the whole MVP, numbered up front:

```
src-tauri/migrations/20260820120000_initial_schema.sql   (task 002)
src-tauri/migrations/20260820120100_seed_settings.sql    (task 006)
```

No other task adds a migration. A task that believes it needs one stops and asks.

**Why.** sqlx orders migrations by version, and two tasks each reaching for "the next
timestamp" collide on one — in separate worktrees they collide silently, because neither sees
the other's file until merge. Migrations are append-only once shipped (ADR-0003), so a
renumber afterwards is not available as a fix. Two is enough because task 002's Notes already
require the whole schema now, including `task_dependencies` and `schedules` that nothing reads
until v0.2.

**Binds.** 002, 006, and every later task as a prohibition.

### Amendment, 2026-08-26 — a third migration, named

Task 010 needs `tasks.source` (ADR-0006: "every mutation is attributed"), which is a column,
which is a migration this entry forbids. [ADR-0019](adr/0019-mutation-source-and-service-context.md)
takes that decision and is the **named exception**:

```
src-tauri/migrations/20260826120000_task_source.sql   (task 010)
```

**The count is now three, and three is the whole list.** The prohibition above is otherwise
unchanged and still binds every other task: a task that believes it needs a fourth stops and
asks, for exactly the collision reason the body gives. Appended rather than edited, the way
D14's amendment was, because the original sentence bound every task written against it and
each should inherit both the rule and the one exception to it.


### Amendment, 2026-09-01 — a fourth, and a fifth reserved

Two more, and the reasons are different in kind.

```
src-tauri/migrations/20260828120000_dependencies_and_parallel.sql   (tasks 011 + 012, reserved)
src-tauri/migrations/20260901000000_backfill_strategy_mode.sql      (task 020)
```

The first is reserved rather than written: tasks 011 and 012 need `runs.base_ref`
and `repositories.max_concurrency`, and the name is claimed here so a later branch
cannot pick the same timestamp.

The second is not a schema change at all — it adds no column, index or constraint.
Task 020 gave `tasks.strategy_mode` a meaning it did not have, and in doing so
changed what an existing row means: `(model = 'opus', strategy_mode = 'default')`
used to say "spawn with opus" and now says "ignore opus and use the configured
default". The migration repairs rows written under the old reading. It is a
one-time data repair, and the alternative to writing it was shipping a branch that
silently changes which model six existing cards run with.

**The count is now five.** D4's prohibition is otherwise unchanged and still binds
every other task, for exactly the collision reason the body gives.

### Amendment, 2026-09-02 — one file for four tasks, and the fifth name retired

Two changes, and the first is a correction rather than an addition.

**`20260828120000_dependencies_and_parallel.sql` is retired, unwritten.** The
amendment above reserved it for tasks 011 and 012 and said the backfill was
"timestamped after the name reserved for tasks 011 and 012 … so the two cannot
collide" — a sentence that assumed the reserved file would be applied first. The
backfill shipped and the reservation did not, so on every database that has run
since, that assumption is unmeetable: `20260828120000` now sorts *before* an
already-applied migration.

sqlx would not object, which is why this had to be found by reading rather than by
running. `Migrator::run` iterates the source in version order and applies whatever
`_sqlx_migrations` does not already list; `validate_applied_migrations` errors only
on the reverse case, an applied version with no file on disk (sqlx-core 0.8.6,
`src/migrate/migrator.rs`). **There is no comparison against the maximum applied
version anywhere.** So the reserved name would have applied silently and out of
order — fourth on a fresh install, fifth on every existing one.

That divergence is the objection. `_sqlx_migrations` would stop being able to say
in what order a given database was built, and ADR-0003 counts reading this file
with any SQLite tool as supported rather than merely tolerated. `cargo sqlx migrate
run --target-version` would also refuse outright, with `VersionTooOld`, and
CLAUDE.md's local prepare loop reuses `target/sqlx-prepare.db` across runs — so
half the team would get one order and CI's fresh database the other.

Nothing was ever written under the retired name, so retiring it costs nothing and
serves the collision-avoidance purpose the reservation had. This entry's own rule
applies to itself: an entry that is wrong is amended, not worked around.

**The rule the mistake teaches, stated so it is inheritable:** a reserved migration
filename is only safe while nothing else can ship before it. A new timestamp must
sort after every migration already on disk, and reserving one in advance is a bet
on merge order that this repository cannot make.

**In its place, one file for four tasks:**

```
src-tauri/migrations/20260902120000_dependencies_parallelism_scheduling_and_capture.sql
```

It carries `runs.base_ref` (task 011, ADR-0008), `repositories.max_concurrency`
(task 012, ADR-0010), `schedules.timezone`, `stop_at`, `last_fired_at` and
`armed_at` (task 013 — ADR-0010 requires "a cron expression with a timezone" and an
optional stop time, and the initial schema shipped neither), and ADR-0022's seven
nullable capture columns on `runs` (task 024's capture half, whose own *Why now*
asks to ride along with exactly this kind of migration). Every column is inert
until the task that owns it lands, which is the same bet the initial schema made
with `task_dependencies` and `schedules`.

**The count is still five — but not the same five.** The list is now:

```
20260820120000_initial_schema.sql                                          (task 002)
20260820120100_seed_settings.sql                                           (task 006)
20260826120000_task_source.sql                                             (task 010)
20260901000000_backfill_strategy_mode.sql                                  (task 020)
20260902120000_dependencies_parallelism_scheduling_and_capture.sql         (011, 012, 013, 024)
```

D4's prohibition is otherwise unchanged and still binds every other task, for
exactly the collision reason the body gives: a task that believes it needs a sixth
stops and asks.

**Binds.** 011, 012, 013, 024, in addition to everything this entry already bound.

## D5 — Compile-time checked queries and the `.sqlx` cache

**Question.** `sqlx::query!` or the runtime `sqlx::query()`, and what enforces that the
committed cache is current? ADR-0003 answers the first half and places the cache; this entry
answers what CI does about it.

**Decision.** Use the `sqlx::query!` family — `query!`, `query_as!`, `query_scalar!` — for
every query with a fixed shape. Regenerate the cache ADR-0003's amendment places at the
workspace root with:

```bash
cargo sqlx prepare --workspace -- --all-targets
```

Do **not** add `sqlx-cli` or a `cargo sqlx prepare --check` step to CI. Run every local
verification command with `SQLX_OFFLINE=true` exported — `SQLX_OFFLINE=true cargo test -p
rimaia-core`, and the same for clippy and `cargo check`.

**Why.** Task 002's acceptance criterion names `cargo sqlx prepare --check`, but that command
requires a live `DATABASE_URL`, which contradicts its own parenthetical "(no live database)" —
so read it as satisfied by running it locally when the cache is generated, not by a CI job that
would need a `cargo install` and a scratch database on every run. The existing CI jobs already
set `SQLX_OFFLINE=true` at the workflow level and therefore already fail on a missing or
incomplete cache. The one hole that leaves — a cache whose *types* went stale while the query
text did not — is closed by the round-trip integration tests, which run the real SQL against a
real migrated database via `testing::db::test_pool`. `--all-targets` is load-bearing for the
same reason CLAUDE.md gives for clippy: without it the integration tests' queries are never
described, and the cache is incomplete in exactly the way that passes locally and fails in CI.

**Binds.** 002, 003, 004, 006, 007, 008, 009.

## D6 — Pre-approved npm dependencies

**Question.** Which runtime npm dependencies may the MVP add?

**Decision.** `@dnd-kit/core`, `@dnd-kit/sortable`, `@dnd-kit/utilities` and `react-markdown`
are approved, for task 005. `@tauri-apps/plugin-dialog` is approved, for task 003. No other
task adds a runtime npm dependency without asking.

**Why.** Task 005's own Notes name dnd-kit and say why: cross-column drop and keyboard
accessibility are more work than they look. The repo has no UI library by choice and that
stands — but a hand-rolled Markdown renderer, for the 400-line plans task 005's acceptance
criteria require to be comfortable to read, is more code and worse than the dependency. The
list is closed rather than a default because two tasks running near each other both editing
`package-lock.json` produce a generated-file conflict, and the natural way an agent resolves a
generated file — regenerate it — silently reverts the other.

`@tauri-apps/plugin-dialog` is a different kind of entry: task 003's scope names a "native
folder picker", which is not something the frontend can hand-roll, so the dependency is the
requirement rather than a convenience. It is listed here anyway because the prohibition above
is worth being literally true — an entry that says "the list is closed" while the tree carries
an unlisted dependency teaches the next agent that the list is advisory. Note it is four
coordinated edits, not one: `package.json`, `src-tauri/Cargo.toml`, the plugin init in
`src-tauri/src/lib.rs`, and a capability in `src-tauri/capabilities/default.json`.

**Binds.** 003, 005, and every other task as a prohibition.

### Amendment, 2026-09-03 — a fifth, for task 013

`@tauri-apps/plugin-notification` and its Rust half `tauri-plugin-notification` are
approved, for task 013's "optional OS notification when a scheduled queue starts and when it
finishes".

It is the same kind of entry `plugin-dialog` is: not a convenience, but the requirement
itself. The whole premise of a scheduled queue is that **the user is not at the machine** —
that is what "start a queue when I leave the office" means — so an in-window banner reaches
nobody, and there is no other surface that does. The four coordinated edits are as this
entry already lists them: `package.json`, `src-tauri/Cargo.toml`, the plugin init in
`src-tauri/src/lib.rs`, and a capability in `src-tauri/capabilities/default.json`.

**The count is now five, and five is the whole list.** The prohibition above is otherwise
unchanged. Recorded rather than added quietly for this entry's own stated reason: a list
that says "the list is closed" while the tree carries an unlisted dependency teaches the
next agent that the list is advisory.

Two things task 013 did **not** take, so the absences read as decisions:

- **No timezone package.** `chrono-tz`'s `TZ_VARIANTS` is exposed through a
  `list_timezones` command, so the list the picker offers and the list the service accepts
  come from one table. A bundled copy in TypeScript would be a second IANA database to keep
  in step with the first.
- **No date library.** `Intl.RelativeTimeFormat` and `toLocaleString` are the whole of what
  `date-fns` or `luxon` would have been added for, and both ship with the platform.

## D7 — The event-subscription seam in the frontend

**Question.** Three MVP tasks add Tauri events the UI listens to. Where does the frontend
subscribe?

**Decision.** `src/lib/events.ts` is the only module in the frontend that imports
`@tauri-apps/api/event`. Every event gets a named exported subscribe wrapper — payload typed
there, `unlisten` handed back to the caller — exactly as every command gets a named wrapper in
`src/lib/commands.ts`. Created by task 005 for `tasks:changed`, extended by 008 for run events
and 009 for queue state.

**Why.** Symmetry with the rule `src/lib/commands.ts` states in its own header: it is the only
module that imports `invoke`, so the serialization boundary has one place to be wrong instead
of one per component. Events have the same failure mode and a worse one — three components
each calling `listen` with their own inline payload type, one of which does not match what the
backend emits, and nothing typechecks the difference. One place to type a payload, one place a
test mocks.

**Binds.** 005, 008, 009.

## D8 — The error type does not grow

**Question.** Git subprocess failures, process spawn failures, worktree safety refusals,
validation failures — do these get new `ErrorCode` variants?

**Decision.** `ErrorCode` gains no new variants during the MVP. Git failures, process failures
and validation failures are `Error::invalid` when the user can fix the input, `Error::internal`
when they cannot, with a message the UI can render.

**Why.** `crates/core/src/error.rs` says in its own doc comment that the code is coarse
deliberately: it exists so the frontend can choose a presentation, not so it can reimplement
backend logic. Every variant added is a matching edit to `src/types.ts`'s `ErrorCode` union and
to whatever renders it — a cross-crate, cross-language change buying no behaviour the user can
act on differently. Specificity that *is* required lives in the message: task 003's "each
invalid case produces its own specific message" is a sentence, not a code.

**Binds.** 003, 004, 006, 007, 008, 009.

## D9 — What "interrupted" is

**Question.** Task 009 must show one `interrupted` task after a crash. ADR-0007 fixes seven run
states and `interrupted` is not among them; ADR-0011 lists `interrupted` as an exit class. Is
it a run state, a column, or the run's business?

**Decision.** `run_state` keeps exactly ADR-0007's seven values: `idle`, `queued`, `running`,
`blocked`, `waiting_retry`, `failed`, `cancelled`. `interrupted` is **not** one of them. A run
that died with the app is recorded on its `runs` row as `status = 'interrupted'` and
`exit_class = 'interrupted'` (ADR-0011's class); the task it belonged to lands in
`run_state = 'failed'` and stays in `ready`, per ADR-0007's failure rule. The card reads the
word "interrupted" off its last run, not off its own state.

**Why.** ADR-0007 fixes seven values and task 005's badge list independently omits
`interrupted` — two documents agreeing is a decision, not an oversight. Task 009's acceptance
criterion is a statement about what the user sees, and the card shows it. Two dimensions, two
fields is ADR-0007's whole argument: the column says where a card is in the user's process, the
run state where it is in the machine's, and *why* the machine stopped is the run's business.
This had to be settled before task 002 rather than discovered in task 009, because SQLite
cannot alter or drop a CHECK constraint — getting the domain wrong means a twelve-step table
rebuild against a migration that has already shipped.

**Binds.** 002, 004, 005, 008, 009.

### Amendment, 2026-09-03 — where the *task* lands, once something resumes it

One of this entry's conclusions no longer holds. It said a run that died with the app leaves
"the task it belonged to" in `run_state = 'failed'`. Since task 014 that is only true when
ADR-0011's retry budget is spent: **a crash-interrupted task lands `waiting_retry` with a due
`resume_after` when the budget allows, and `failed` otherwise.**

Everything else in the entry stands, and the parts that stand are the parts that mattered.
`run_state` still has exactly ADR-0007's seven values and gains no eighth — SQLite still cannot
widen a CHECK, so that remains permanent. `interrupted` is still not one of them. The run row
still carries `status = 'interrupted'` and `exit_class = 'interrupted'`, and the **card still
reads the word off its last run**, which was this entry's actual subject. Task 009's acceptance
criterion — "reopening shows accurate state: one `interrupted` task" — is unaffected, because
it is a statement about what the user sees and the word has not moved.

**Why it changed.** The original conclusion was reached under a condition that has since gone
away, and `scheduler::reconcile`'s own header said so at the time: the second hop
`WaitingRetry -> Failed` existed *only* "because nothing resumes waiting_retry yet". Now
something does. ADR-0010:57-59 and ADR-0011's startup reconciliation both ask for a crashed run
to be **offered** for resume, and leaving it `failed` was the strictly worse reading — the
worktree still has the commits, the session is still resumable, and the ADRs both say to offer
it. `reconcile::settle` therefore keeps the hop only when there is no `resume_after`.

**Offered, not performed**, which is what makes this safe under [D15](#d15): the exit path
writes `paused`, `QueueState::default()` is `Paused` and `from_stored` falls back to it, so a
task sitting due at 03:00 starts only when a human presses Start. Three independent guarantees,
all three asserted by
`a_launch_offers_a_crashed_run_for_resume_and_starts_nothing_until_the_queue_is_started`.

Recorded as an amendment rather than an edit because the original sentence bound tasks 002, 004,
005, 008 and 009, and each of them should inherit the corrected version *and* the reason. The
entry now binds 014 as well.

**Binds.** 002, 004, 005, 008, 009, 014.

## D10 — Identifiers are strings

**Question.** What Rust type is an id?

**Decision.** Every id is a `String` holding `Uuid::new_v4().to_string()`, generated behind one
helper (task 002 owns it, with the models). Columns are `TEXT`. No newtype wrappers, and never
`uuid::Uuid` as a column type.

**Why.** Two independent reasons. First, sqlx maps `Uuid` to `BLOB` on SQLite, not `TEXT` — so
declaring a TEXT id column as `Uuid` compiles and then fails at runtime, and storing real BLOBs
would make the database file unreadable in the `sqlite3` CLI, which ADR-0003 explicitly values
("the user can inspect and repair state with any SQLite tool"). Second, newtypes would
genuinely stop `task_id` and `depends_on_task_id` being interchangeable in a signature — a real
bug they would catch — but cost a type override on every id column in every `query_as!` across
tasks 004, 008 and 009. Recorded as a deliberate trade so a later reviewer does not read it as
an oversight and "fix" it.

**Binds.** 002, 003, 004, 007, 008, 009.

## D11 — What "startup fails loudly" means

**Question.** Task 002's scope says migrations are "applied at startup before the window opens.
Startup fails loudly on migration error." Loudly how?

**Decision.** The window never opens, the process exits non-zero, and the reason is written to
stderr and to the rolling log file under `<app-data>/logs/`. No modal dialog.

"Never opens" has to be *arranged*, because it is the opposite of what Tauri does unaided:
`setup()` builds every window declared in `tauri.conf.json` before it calls the user setup hook
(tauri 2.11.5, `src/app.rs:2524`), and both `create` and `visible` default to true. Left alone,
a migration failure therefore draws the full 1280x832 window, loads the frontend into it, and
only then panics — the user watches a window appear and vanish. So the mechanism is two halves,
and neither is meaningful alone: the main window is declared `"visible": false`, and
`src-tauri/src/lib.rs` shows it as the **last** statement of the setup hook, after every
fallible step has succeeded. Drop the config flag and the window is on screen while the
migration runs; drop the `show()` call and a *successful* startup leaves an app with no window
at all. This is what makes the first paragraph true rather than aspirational.

**Why.** A modal needs `tauri-plugin-dialog`, which is not a dependency, added for a path that
by definition already failed. `logging::init` runs before the database is opened in the setup
hook, so the file appender is open and synchronous by the time a migration can fail — the log
line that matters most is written. Every fallible step in that hook logs at `error` level
itself, before propagating: Tauri turns the returned `Err` into a panic at
`RuntimeRunEvent::Ready`, and `panic!` does not go through `tracing`, so a step that only
propagates leaves the log file holding nothing but the "rimaia starting" line. What this does
not solve is a double-clicked `.app`, where nobody reads stderr; that visible-failure story
belongs with task 018's preflight doctor rather than being invented inside task 002. Recorded
here so task 002 does not reach for a plugin and task 018 knows it inherits the problem.

**Binds.** 002, 018.

**Amendment (task 018).** Two things in the paragraph above are now wrong, and task 018 is
the task that inherited them, so it is the task that has to say so.

*The stated reason no longer holds.* "A modal needs `tauri-plugin-dialog`, which is not a
dependency" stopped being true at task 003, which added the plugin for the folder picker —
Rust, npm and `capabilities/default.json` all carry it today. The decision survives its
premise (a path that has already failed is not where to first reach for a plugin), but it
now rests only on that second argument, and a future reader should not be told a cost that
is no longer paid.

*The delegation was misplaced.* D11 hands "that visible-failure story" to task 018's
preflight doctor. **The doctor cannot take it.** The doctor is a command inside a running
app; it runs when startup has already *succeeded*. A migration failure is exactly the case
where no window opens, no command surface exists and nothing can be asked to check anything
— so no amount of doctor coverage reaches it. The two failures are disjoint: the doctor
prevents a *run* from failing at 2am, D11 is about the *process* failing at launch.

What task 018 therefore does and does not close:

- **Closed.** The environment half. Eight checks, a blocking refusal on `QueueHandle::start`,
  and a README that names `<app-data>/logs/rimaia.log` as the first place to look when a
  double-clicked bundle does not open — which is the only thing that helps a user who has no
  stderr, short of a dialog.
- **Still open.** The dialog itself. It is now cheap, and packaging is what makes it matter
  (a `.app` is precisely the case with nobody watching stderr), but the mechanism is
  `blocking_show()` on the setup hook's thread, and this task shipped without being able to
  run a bundled build to prove that does not deadlock on macOS. An unverified blocking call
  on the launch path is a worse failure than the silence it replaces. Recorded rather than
  guessed at; it wants its own task and a human at a real bundle.

## D12 — What the board's bulk read returns

**Question.** Task 005's card must show a link count, a dependency indicator, and — per [D9](#d9)
— the word "interrupted" read off the task's last run. Task 004 shipped `list_tasks` returning
bare `tasks` rows and `get_task` returning the full detail with links, dependencies and the last
run. Neither serves a board of fifty cards: the row has none of it, and the detail is one query
per visible card.

**Decision.** `list_tasks` returns a **summary projection**, not a `Task` row. One query per
board read, with the counts and the last-run fields computed by aggregate and correlated
subquery in SQL:

```
TaskSummary = every column of `tasks`
            + link_count: i64
            + dependency_count: i64
            + blocked_by_incomplete: bool   -- reserved for task 011; false until it lands
            + last_run: Option<{ status, exit_class, ended_at }>
```

`get_task` is unchanged and keeps returning the full detail with the link rows themselves. The
frontend card renders only from the summary; the panel renders from the detail.

**Why.** The alternative shapes are each worse in a specific way. Fetching `get_task` per card is
N+1 against the single SQLite writer on every `tasks:changed`, which arrives once per mutation —
a fifty-card board doing fifty reads per keystroke-driven autosave is the one performance
mistake this codebase can actually make. Denormalising a counter column onto `tasks` would need a
third migration, which [D4](#d4) forbids, and a counter maintained by triggers or by hand is a
second source of truth for something SQL computes correctly for free. Dropping the fields from
the card would silently contradict task 005's Scope and D9 — the card is the only place the word
"interrupted" is ever supposed to appear, since [D9](#d9) deliberately kept it out of
`run_state`, and a board that cannot show it makes D9's whole argument hollow.

`blocked_by_incomplete` ships as a constant `false` now rather than being added later, for the
same reason task 002 shipped `task_dependencies` and `schedules`: task 011 turns it into a real
predicate by changing one query, and the DTO and its TypeScript mirror are already in place. The
card does not render it yet — there is nothing true to render until task 011 computes it — so
that task adds the query and the badge together.

**Binds.** 004, 005, 011.

### Amendment, 2026-08-28 — the summary carries the effective strategy

Task 020 adds three fields to the projection: `effective_model`, `effective_effort` and
`effective_origin`, filled by applying the strategy precedence chain (task → repository →
global) to each row after the query. `TaskDetail` grows the same three.

They are computed in Rust rather than in the card, for the reason this document exists: the
chain is a business rule, and a TypeScript copy of it is a second implementation that will
disagree with the first. They are not a fourth counter subquery either — one board read
still costs one query plus two settings reads, so the argument above against N+1 is
unaffected. `effective_origin` rides along because the card renders an inherited value
differently from a chosen one, and reconstructing which link of the chain won from the
value alone is not possible.

The entry now binds 020 as well.

### Amendment, 2026-09-02 — the summary names the blocker

Task 011 adds a fifth field to the projection: `blocking_title: Option<String>`, the title of
the first unsatisfied dependency in the order
[ADR-0008](adr/0008-dependency-semantics-and-branch-chaining.md)'s 2026-09-02 amendment fixes
(column rank, then ascending `position`). `None` exactly when `blocked_by_incomplete` is false.

The card must **name** the blocking task, not merely flag one. Task 011's acceptance criterion
is "a failing A leaves B and C blocked, each showing A as the reason", and a title is the only
field that names it — a `title=` attribute on a badge does not satisfy "showing", because the
morning review it exists for is a glance down a column rather than a hover over each card.
Reading the name per card any other way would be the `get_task`-per-card N+1 this entry's body
rejects.

It is a second correlated subquery over `task_dependencies`, alongside the `EXISTS` that
computes `blocked_by_incomplete`. That does not disturb the argument above: the cost this
entry is about is *fifty reads per board read*, and a board read is still one query.
`task_dependencies` holds a handful of rows per task on one desktop user's board.

The field is derived on every read and never stored — see ADR-0008's amendment for why a
cached `run_state = blocked` would live on the wrong row. The entry now binds 011 for both
fields rather than only reserving one.

### Amendment, 2026-09-03 — the last-run summary carries `resume_after`

Task 014 adds a fourth field to `last_run`: `resume_after`, one more column on the correlated
subquery that already joins the latest attempt, mirrored on `LastRunSummary` in `src/types.ts`.

It pays for itself twice, which is why it is on the projection rather than fetched per card.
It is the **card badge** task 014's Scope asks for — "`waiting_retry` with the time it will
resume" — and a badge without the time cannot tell a task coming back at 06:12 from one whose
retries ran out. And it is what `scheduler::selection::skip_reason` reads to decide whether a
waiting task is *due*, which happens on every pass of the queue loop; a per-task query there
would be the N+1 this entry exists to refuse, on the hottest path in the product.

One board read still costs one query plus two settings reads, so the argument against N+1 is
unaffected, and nothing else about the shape changes. `blocked_by_incomplete` is untouched.

The entry now binds 014 as well.

## D13 — Whether a task can change repository

**Question.** Task 005's Scope lists "Title, repository selector" in the task detail panel, but
task 004 shipped `TaskPatchInput` without a `repositoryId` field. So the panel rendered the
repository as read-only text behind a comment admitting no ADR said it should be. Which is right?

**Decision.** A task's repository is reassignable **only while the task has no worktree and no
runs**. The guard lives in `tasks::update_task` in `rimaia-core` and refuses with a message
naming what blocks it; the panel shows a real selector, disabled with that same reason once it
is fixed. `TaskPatchInput` gains `repository_id`.

**Why.** Both halves of the original conflict were right about something. Reassignment genuinely
becomes unsafe the moment task 007 creates a worktree: ADR-0005 ties `branch` and
`worktree_path` to one repository, `runs` rows reference transcripts produced inside it, and
ADR-0008's branch chaining resolves a base ref within a repository — a task dragged to a
different repo after any of that is a task whose recorded state describes a place it no longer
lives. But *before* any of it, a task is a title and a plan, and mis-filing one is an obvious
mistake to want to undo without retyping the plan.

The guard belongs in the service, not the panel, because ADR-0006 makes a rule enforced in only
one of the UI path and the MCP path a bug — task 010 will expose `update_task` too. Disabling the
control in the UI is a courtesy on top of the refusal, never a substitute for it.

Recorded here rather than left as a code comment because the comment was reasoning from the
command surface — "`TaskPatchInput` has no `repositoryId`, therefore it is fixed" — which
inverts cause and effect. Task 004 simply had no reason to add the field; that is not a decision
anyone made.

**Binds.** 004, 005, 007.

## D14 — The mechanism for the live run tail

**Question.** [ADR-0018](adr/0018-core-to-shell-change-events.md) routes state changes from a
core service to the shell, and then explicitly declines to carry the live run tail: *"Task 008
picks the mechanism for the tail; it must not turn `ChangeEvent` into a data channel by adding a
payload-carrying variant."* So task 008 has to pick, and no ADR says what.

**Decision.** The same shape as ADR-0018, on a **separate channel**. `rimaia-core` owns a second
`tokio::sync::broadcast::Sender<RunTail>` alongside `changes` on `ServiceContext`. The runner
publishes to it as events arrive; the shell subscribes once in `setup()` and forwards to a
`runs:tail` Tauri event. Unlike `ChangeEvent`, `RunTail` **does** carry a payload — the run id,
elapsed time, turn count, the current tool call, and the most recent assistant text — because it
is a view, not a fact about stored state.

Two rules follow from that difference:

1. **A dropped tail message is nothing.** `RecvError::Lagged` on this channel is discarded and
   counted, never recovered. A `ChangeEvent` drop means "re-read"; a tail drop means the user
   missed a line of scrollback that is already on disk in the JSONL transcript. Do not build
   replay for it.
2. **The tail is never the source of truth for anything persisted.** The transcript file is
   (ADR-0013), and the `runs` row is. If the tail and the row disagree, the row wins.

**Why not reuse ADR-0018's channel.** Frequency. `ChangeEvent` fires once per committed mutation;
the tail fires many times per turn. Sharing one bounded broadcast means a chatty run can lag a
subscriber into dropping change events — and a dropped change event *does* have a consequence: a
card that stops refreshing until the next mutation. Separating them makes the two lag behaviours
independent and correct for their own kinds of loss, which is precisely the distinction ADR-0018
was protecting when it pushed the tail out.

**Why not polling a ring buffer through a command.** It would work, and it avoids a channel — but
it puts the refresh interval in the frontend, where it is either too slow to feel live or a
constant query loop against the single SQLite writer while a run is in flight.

### Amendment, 2026-08-21 — what "catch up" actually means

As first written this entry said the bounded ring buffer task 008's scope names is "what a client
reads to catch up when it starts watching mid-run". Task 008 implemented something different and
better, and the entry was wrong rather than the code.

**The catch-up is the latest snapshot, held by the shell.** The forwarder subscribes in `setup()`
and therefore has seen every `RunTail` since the run began, so it caches the most recent one per
run and a client that opens the Runs view mid-run asks for that. The in-core ring buffer
(`RunProgress`, `RECENT_ACTIVITY_CAPACITY`) still exists and still earns its place: it bounds what
a snapshot is built from and caps a verbose turn's contribution, in a process that runs all night.
It is not, and does not need to be, readable across the process boundary.

Scrollback during a run comes from the transcript file, not from memory. ADR-0013 already said so
— *"completed runs are read from the JSONL file on demand, paginated"* — and the same file is
being flushed line by line while the run is live, so there is nothing a second in-memory copy
would add except a second thing to keep consistent. The one-snapshot field list is also exactly
what ADR-0013 specifies the live view shows: current tool call, last assistant message, elapsed
time, turn count.

Recorded as an amendment rather than an edit because the original sentence bound task 009 and
task 015 too, and both should inherit the corrected version and the reason for it.

**Binds.** 008, 009, 015.

## D15 — What quitting does to the queue

**Question.** Task 009 makes the queue's state durable — "derived from the database, so it
survives app restart". Task 008's exit path SIGTERMs a run in flight. Together those leave a
question neither task answers: after the user quits, is the queue running when the app comes
back?

**Decision.** **Quitting always stops the queue.** Whether or not a run happened to be in flight
at that instant, the exit path sets the queue to stopped, so the next launch starts idle and
waits to be told to go.

**Why.** The alternative that shipped first was accidental rather than chosen: quitting mid-run
stopped the queue (because the cancel path stopped it) while quitting between runs left it
running (because nothing stopped it). Same user action, two outcomes, decided by whether a child
process existed at that millisecond.

Of the two consistent answers, stopping is the conservative one and it is what this codebase's
own reasoning already argued for in the mid-run case: a run the app just killed by quitting
should not silently restart itself on the next launch without the user asking again. Extending
that to the between-runs case costs one deliberate click in the morning. The opposite default —
resume on launch — means opening the app to check something starts spending money before the
window is drawn, and ADR-0012 makes those runs `bypassPermissions` in an opted-in repository.

The durability task 009 built is not wasted: what survives a restart is the board, the run
history and every task's state. It is only the *go* signal that does not, and that is the one
piece of queue state a human should own.

Revisit when task 013 lands run windows and scheduling — "start at 22:00" is a standing
instruction of exactly the kind this entry declines to infer, and once it exists the right
default may change.

**Binds.** 008, 009, 013.

### Amendment, 2026-09-03 — the revisit this entry asked for (task 013)

**Quitting still always stops the queue. A schedule is not queue state.** `queue_state` is
still written `paused` on exit and still starts `paused` on launch; `QueueState::default()`
is still `Paused` and `from_stored` still falls back to it. Nothing in the body is
withdrawn.

What changes is that `paused` no longer means "nothing will happen". An **enabled
`schedules` row is a standing instruction the user gave in advance**, which is exactly what
the body declined to *infer* — inferring "they probably want it running again" from a queue
that happened to be running is not the same act as reading a row that says "every night at
22:00". The go signal is still owned by a human; there are now two explicit ways to give it,
and the second one is a row they created, named, and can see the next fire time of.

Three consequences, and all three are asserted:

1. **A schedule whose time passed while the app was closed fires on next launch** — late,
   once, and not at all if that occurrence's own stop time has already passed. Firing the
   most recent missed occurrence rather than each of them is what makes "fires late rather
   than skipping" (ADR-0010) survive a laptop that was shut for a week.
   (`a_schedule_whose_time_passed_while_the_app_was_closed_fires_once_on_next_launch`,
   `a_schedule_that_missed_five_occurrences_fires_once_not_five_times`,
   `a_window_whose_stop_time_already_passed_does_not_open`.)

2. **Quitting mid-window closes the window.** The exit path already calls
   `QueueHandle::stop`, which now clears `active_run_window` alongside writing `paused`, so
   relaunching at 03:00 does not silently resume a night the user quit out of — while the
   schedule's *next* occurrence still fires, because the schedule is the standing
   instruction and the window is only one night of it.
   (`quitting_mid_window_closes_the_window_and_the_next_occurrence_still_fires`.)

   **A crash does not close it, and that asymmetry is deliberate rather than an oversight.**
   The body's objection was to one *user action* having two outcomes depending on whether a
   child process happened to exist; quitting and crashing are two different actions, and the
   difference between "I am done" and "the process died" is exactly the kind of thing a
   window should respect. A crash therefore leaves the window open and the launch after it
   still `paused`, so the user who presses Start gets their night back **with its stop time
   intact** rather than an unbounded queue. Nothing starts without them either way, which is
   the guarantee the body actually makes.

3. **`stop` and `pause` clear the window too.** Stop inside a window means stop, not "stop
   until the timer looks again" — and the timer would look again within the minute, because
   `tick_schedules` reads an open window as a night still in progress. Without this the
   Pause button would undo itself.

**The ADR-0010 / ADR-0011 tension, settled.** ADR-0010:57-59 says runs left `running` at a
crash are "eligible for resume", and task 014 made that real: `reconcile` lands such a run
in `waiting_retry` with a `resume_after` that is already due when ADR-0011's budget allows.
**Eligible is not automatic, and the schedule is not what makes it automatic.** A fire does
three things — run the doctor, write the window, flip the switch — and moves no task's
`run_state` at all. Whether any individual task resumes is ADR-0011's per-run decision,
taken by `selection::skip_reason` inside an **open window with the switch on**; a fire that
opens no window resumes nothing, however due the deadline is
(`a_schedule_firing_tonight_does_not_resume_a_run_last_night_crashed_on`).

The converse is asserted beside it, and matters as much: once the window *is* open, the
crashed run resumes **exactly as pressing Start resumes it**, `--resume` and all
(`a_schedule_that_does_open_a_window_resumes_exactly_what_start_would`). A standing
instruction that was quietly weaker than the button would be a third behaviour nobody asked
for, and this amendment's whole argument is that the two are the same go signal given two
ways.

The entry now binds 013 as an implementer rather than only as a revisit point.


## D16 — Task 010's cross-cutting choices

**Question.** Task 010 exposes the task service over MCP. ADR-0006 fixes the transport, the
loopback boundary, the port and the tool list, and stops there. Half a dozen smaller answers
are still needed, and tasks 011, 018 and 020 each inherit one or more of them.

**Decision.** Seven, taken together by task 010:

1. **All MCP tool JSON is `snake_case`**, in both directions — request fields and response
   fields. Not the `camelCase` the Tauri boundary uses.
2. **The port lives in `settings` under the key `mcp_port`, owned by
   `crates/core/src/mcp/settings.rs`.** Storage still goes through `db::settings::get`/`set`;
   what the key means, what an absent one means and what range is legal live with the module
   that owns it — the shape D3 fixed, and the same shape `scheduler/state.rs` uses for
   `queue_state`.
3. **The transport is `rmcp` 3.1.4 over `axum` 0.8**, not a hand-rolled JSON-RPC surface.
   `rmcp`'s reqwest client transport is also what `mcp::probe` uses for Test connection, so
   the probe cannot disagree with the server about the wire format. (`reqwest` is already in
   the workspace lockfile via `tauri`, with no TLS features; the probe adds none.)
4. **`set_task_dependencies` ships in 010, not 011**, with cycle detection and cross-repository
   rejection in `crates/core/src/tasks/dependencies.rs`. It is on ADR-0006's tool table, and a
   tool that stores an edge without checking for a cycle is not the tool that table names.
   Task 011 keeps blocking, branch chaining, the `blocked_by_incomplete` predicate and the UI.
5. **`update_task` over MCP erases a field through a `clear: [field]` list**, not by sending
   `null`. An LLM filling in every property of a schema sends `plan: null` and destroys four
   thousand words; an omitted field is a no-op. The two mistakes are not symmetric, so the
   destructive one is made to be deliberate. `plan` is not clearable over MCP at all.
6. **`list_tasks` omits plan text.** Fifty tasks times a multi-thousand-word plan is a context
   bomb in the caller's session. `get_task` is how an agent reads one plan.
7. **A busy MCP port is surfaced, not fatal to startup.** D11's fatality argument does not
   transfer: the remedy — Settings → MCP — lives behind the window a fatal bind would refuse to
   open, and task 018's "MCP port free" doctor row would be unreachable code.

**Why.** Each is a place where two tasks would otherwise choose independently and differently.
(1) is the convention MCP tool schemas are written in everywhere else, and mixing it with the
frontend's `camelCase` inside one process is a bug generator — so the DTOs in `mcp::responses`
are deliberately projections rather than the row types re-serialized. (2) keeps one settings
reader instead of two, which is the whole of D3. (3) is a dependency choice, and D6's argument
about a closed list applies to Cargo as much as to npm. (4) is scope, and the alternative —
011 adding cycle detection under a tool 010 already shipped — means 010 ships a tool that
corrupts the graph. (5) and (6) are the two places the MCP surface deliberately *differs* from
the command surface, which is exactly the kind of divergence that must be written down rather
than discovered in a diff; note that neither is a business rule enforced in one path only —
they are capabilities the adapter declines to expose, which ADR-0006 already does for
`delete_task` and every run tool. (7) is argued at length in ADR-0019's neighbourhood and
restated here because task 018 inherits it.

See also ADR-0019, which takes the `tasks.source` decision and the named exception to
[D4](#d4--migration-file-numbering) that its migration needs.

**Binds.** 010, 011, 018, 020.

## D17 — Task 020's cross-cutting choices

**Question.** Task 020 gives every task an execution strategy, and runs a planner for the
tasks that ask for one. ADR-0016 fixes the three modes, the injection-not-orchestration
boundary and the columns; the 2026-08-28 amendments to ADR-0004, 0006, 0009 and 0012 take
the four decisions large enough to be argued at product scale. What is left is a set of
smaller answers that tasks 021 and 016 inherit, and that a reviewer would otherwise meet in
a diff with nothing to check them against.

**Decision.** Nine, taken together by task 020:

1. **No fourth migration.** [D4](#d4--migration-file-numbering)'s count stays at three.
   `tasks` has carried all six strategy columns since `20260820120000_initial_schema.sql` —
   write-never, read-never until now — and everything else 020 stores is *configuration*:
   the per-repository and global defaults, the model and effort catalogue, the approval
   flag. `settings` is the configuration table
   ([D3](#d3--who-owns-settings-storage-vs-the-typed-accessor)), so none of it is a column.
   The named cost, so that nobody meets it as a bug: a settings key is not a foreign key and
   nothing cascades, so `repo::remove` gains an explicit `settings::delete` of that
   repository's default, plus a test that removing a repository leaves no orphan row behind.
2. **Per-repository defaults are keyed `strategy_default.<repository_id>`.** Four keys,
   owned by `crates/core/src/strategy/settings.rs`, in the shape
   [D3](#d3--who-owns-settings-storage-vs-the-typed-accessor) fixed and
   [D16](#d16--task-010s-cross-cutting-choices).2 repeated — storage through
   `db::settings::get`/`set`, meaning owned by the module:

   ```
   strategy_catalogue                  model list, effort list, and the planner's own budget
   strategy_default                    global StrategyDefaults JSON
   strategy_default.<repository_id>    per-repository StrategyDefaults JSON
   strategy_approval                   "automatic" | "manual" — stored and rendered by 020,
                                       read by nothing until the approval gate lands
   ```

   Absent, unparseable and explicitly-empty values follow the tolerance rule
   `RunEnvironment::from_stored` and `mcp::configured_port` already state: warn and fall
   back to a Rust default, never fatal. An explicitly empty list means no choices, not the
   default list.
3. **`strategy_plan` holds this envelope, version 1.** The column stays `TEXT`, parsed with
   `serde_json` — the workspace `sqlx` has no `json` feature, and `db::models`' own comment
   defers that choice to this task. **Task 021 reads this document**, which is why it is
   here verbatim rather than only in a Rust doc comment:

   ```json
   { "version": 1,
     "status": "proposed" | "failed",
     "model": "sonnet", "effort": "high",
     "workflow": "single_agent" | "multi_agent",
     "phases": [{ "name": "Schema", "model": "sonnet", "effort": "medium", "agents": 1, "summary": "…" }],
     "rationale": "…",
     "run": { "session_id": "…", "num_turns": 4, "cost_usd": 0.031, "error": null } }
   ```

   `run` carries the planner's own accounting because the planner has no `runs` row (5) and
   the panel still has to render "Planner: 4 turns, $0.03". A `failed` envelope is written
   on every planner failure, with `error` set — which is also what makes the re-plan guard
   (8) work.
4. **The scoped handle is a path-segment token in an inline `--mcp-config` JSON string.**
   ADR-0006's amendment fixes the route and the per-tool allow table — every tool, with
   `Operator` and `Run { task_id }` columns, "its own task only" for the five a run may
   call and an outright refusal for the four it may not. The mechanism is here. The runner
   mints a token per run against a shared `RunHandles` value, passes
   `--mcp-config {"mcpServers":{"rimaia":{"type":"http","url":"http://127.0.0.1:<port>/mcp/run/<token>"}}}`
   in argv, and revokes on `Drop`, so a cancelled or panicking run cannot leave a live token
   behind. `RunHandles` holds the bound endpoint as shared mutable state rather than a URL
   copied at startup, because `set_mcp_port` rebinds the server at runtime and a copy goes
   stale; that also removes an ordering constraint between `scheduler::build` and
   `mcp::build` in the shell. With no endpoint bound at all — the busy-port case
   [D16](#d16--task-010s-cross-cutting-choices).7 makes non-fatal — no `--mcp-config` is
   passed, no planner is started, and the message names Settings → MCP.

   Inline JSON rather than a temp file because `process.rs` earns its tests by pinning argv
   byte for byte and a temp path changes every run; secondarily because there is nothing to
   create, clean up, or leave inside a worktree where the run could stage it. A
   **header**-carried token was rejected: `StreamableHttpService`'s service factory is
   `Fn() -> Result<S, io::Error>` with no access to the request, so the scope would have to
   be pulled from request extensions inside each handler — a second parameter on all eleven,
   every direct-call test rewritten, and the scope living on the *request*, where a newly
   added tool can silently forget to read it. In the path it lives on the server value,
   where the type system carries it and one test can require every registered tool to
   declare a decision.
5. **A strategy run gets no `runs` row, and its transcript is `strategy-<uuid>.jsonl`.**
   Three independent reasons, any one sufficient: `finish_run` → `apply_to_task` moves a
   successful run's card to `in_review`, so recording the planner would file the card for
   review before the work happened; `idx_runs_task_attempt` is `UNIQUE(task_id, attempt)`,
   so `attempt` would come to mean "attempts, and also plannings", and the card
   [D12](#d12--what-the-boards-bulk-read-returns) specifies reads `last_run`, so the badge
   would show the planner's outcome instead of the implementation's; and telling the two
   apart needs a `runs.kind` column, which is the migration (1) declines to add. The
   transcript still lands on disk — `Transcript::create` touches no database — at
   `<data>/runs/<task-id>/strategy-<uuid>.jsonl`, beside the implementation run's.
   **Task 016's cleanup must recognise that prefix**: these files have no `runs` row, so
   anything that enumerates transcripts through the database misses them. It follows that
   the task walks the run-state machine exactly once, under one claim;
   `is_legal_run_state_transition` and its exhaustive table test are untouched, and the
   strategy run is deliberately given no claim of its own, which would need a
   `Running → Running` transition that is banned.
6. **A task in resolved `Default` mode ignores its own `model` and `effort`, and
   `update_task` flips the mode to `Manual` when either is set.** `tasks.strategy_mode` is
   `NOT NULL DEFAULT 'default'` and cannot spell "inherit", so `Default` on a task means
   *fall through* — repository, then global. That is what makes ADR-0016's "a repo of small
   tasks can default low without touching each card" work at all, and what lets a repository
   default to `planned`. The consequence is that a model left on a card would otherwise be
   silently ignored, so the service sets `strategy_mode = Manual` whenever `model` or
   `effort` arrives as a set, and back to `Default` when both become null. The rule is in
   `tasks::update_task`, not in a command, so the board and MCP get it identically
   (ADR-0006).
7. **"Accepted" is `strategy_source` flipping `planner` → `user`.** Accepting, editing and
   overriding a proposal are the same write with different payloads, and all three are a
   claim of authorship. No `accepted` column and no `approved_at`: the proposal stays on
   `strategy_plan` to be read, and the source says whose decision the run will execute. It
   is also what `set_task_strategy` checks before letting a planner overwrite a strategy a
   human has taken over.
8. **A recorded proposal suppresses further planning, whether it succeeded or failed.**
   `needs_planning(task)` is `mode == Planned && strategy_plan.is_none()`, and nothing else.
   Safety-critical rather than tidy: without it, a `planned` task whose planner fails is
   replanned on every queue pass, forever, paying for the same failure all night — which is
   the precise shape of overnight loss this product exists to prevent. Editing the plan text
   does not re-trigger. "Re-plan" in the panel clears the column, and is the only thing that
   does.
9. **The strategy prompt is these sections, in this order**: `# Your job` · `# Task context`
   · `# Plan` · `# Extra instructions` · `# Available models` · `# Available effort levels`
   · `# How to answer`. Composed by the rules ADR-0009 already fixes — level-1 headings, the
   same separator, empty sections omitted with their heading — and, per that ADR's
   2026-08-28 amendment, carrying no base instructions. `# How to answer` names the tool and
   its arguments and says to print nothing else: **there is no printed-JSON fallback.** A
   second way in would be a second writer with its own parser duplicating every invariant
   `set_task_strategy` enforces, which is the bug ADR-0006 exists to prevent; extracting it
   would mean a heuristic over free-form prose; and the scope check lives on the MCP path. A
   planner that reasons well and then forgets to call the tool is *detected* — the runner
   compares `strategy_updated_at` against a clock reading taken before the spawn, so nothing
   parses printed output anywhere — recorded as a failure, and falls back to the `default`
   chain.

**Why.** The same test as every entry here: could two agents have answered differently, and
would a reviewer be able to tell which was right? (1) and (2) decide whether 020 is a schema
change or a configuration change, and [D4](#d4--migration-file-numbering) makes that a
question no task gets to answer quietly. (3) is a wire format between two tasks written
months apart — 021 parses what 020 writes, and "read the struct" is not a contract when the
struct can be renamed. (4) is the security-relevant mechanism, and its rejected alternative
is recorded because a token in a URL path reads as laziness until the reason it is not is
written down. (5), (6) and (8) are each invisible in the happy path and expensive in the
failure path: a card filed for review by its own planner, a chosen model silently ignored, a
queue paying for one broken planner until morning. (7) and (9) are the two places 020's
surface deliberately differs from what a reader would guess — no approval column, and no way
to answer except the tool.

See also the 2026-08-28 amendments to [ADR-0004](adr/0004-drive-claude-code-via-headless-cli.md)
(the strategy run is always `strict_local`), [ADR-0006](adr/0006-embedded-local-mcp-server.md)
(the eleventh tool, the scoped route and its threat model),
[ADR-0009](adr/0009-prompt-composition.md) (the fifth prompt section) and
[ADR-0012](adr/0012-permission-posture-for-unattended-runs.md) (the planner's narrower
permission posture), which take the decisions this entry is too small for.

**Binds.** 020, 021, 016.

---

## D18 — What a NULL capture column means

**Question.** ADR-0022 adds seven nullable columns to `runs` and says of them: *"NULL
means 'not recorded', never zero — an analytics view that averages a NULL as zero is
lying about the past, **and the seam contract should say so**."* This is the entry it
asks for. What may a reader of `runs.model`, `runs.effort`, `runs.run_environment`,
`runs.input_tokens`, `runs.output_tokens`, `runs.cache_read_tokens` and
`runs.cache_creation_tokens` conclude from a NULL?

**Decision.** **Nothing except that the value was not recorded.** Three consequences,
and all three bind every future reader:

1. **A NULL is never coerced to zero, and never averaged as one.** `SUM` over a column
   with NULLs is a sum over the rows that have values, which is a different quantity
   from the total and must not be labelled as it. An aggregate that spans rows without
   the value **says so on screen** — ADR-0022: *"a view showing 'models used' across a
   range that predates the migration must say the earlier part is unrecorded rather
   than silently reporting a smaller total."*
2. **A NULL is never backfilled, guessed, or repaired.** Not from `tasks.model` (the
   present tense — a planner or a human rewrites it), not from the current
   `run_environment` setting (it was a setting when the run started and settings
   change), and not by re-reading the transcript (which task 015 is designed to
   delete, and which ADR-0022 part 2 permits precisely because the row survives it).
   There is no correct value to write; "not recorded" *is* the correct value.
3. **These seven are written exactly once, by `finish_run`, and never updated.** A run
   that dies before its terminal `result` event honestly never learns its token
   counts, and a second writer would be a second source of truth for a fact that has
   one moment of existence.

**Why.** Two rows with a NULL and a zero in `output_tokens` describe different worlds —
one where nothing was recorded and one where a run genuinely produced nothing — and
collapsing them is not a rounding error, it is a claim about history that is false.
The columns exist at all because history cannot be backfilled; a reader that fills the
gaps defeats the reason they were added early.

This is an entry rather than a doc comment because it binds three tasks that do not
share a module: task 008 writes the values, task 015's pruning must leave the row alone
while deleting the file beside it, and task 024's page is the reader the rule exists to
constrain. A comment in `runner/outcome.rs` reaches only the first.

**Binds.** 008, 015, 024.

---

## D19 — Where "is this task already in flight" lives

**Question.** Two things needed the same answer and each had its own. The queue held a
private `Option` for the run it was supervising; `src-tauri`'s `RunRegistry` held a
`HashMap` for runs a button had started, and deferred to the queue's `Option` through an
`attach_queue` back-reference. ADR-0021 named the underlying question and assigned it to
tasks 012 and 014. Where does it live, and what does the answer change?

**Decision.** **`rimaia_core::scheduler::inflight::InFlight`**, built once in `setup()`,
handed to `scheduler::build` and held on `AppState` as a clone of the same value. Five
choices come with it, and each is a thing a diff would otherwise not explain:

1. **It is a `build` parameter, not a sixth field on `ServiceContext`.** ADR-0019 fixes
   that struct's shape and says a later field is a later record — and it would be wrong
   anyway: store, clock, channels and attribution are things *any* service may use, while
   an in-flight map is only meaningful to something that can spawn. That is three call
   sites, not every function. The precedent is `RunnerConfig::run_handles`: one value
   built in `setup()`, handed to everything that needs it, no ordering constraint between
   the subsystems that take it.
2. **A `Lease` is RAII, and the RAII is the point.** `Drop` frees the slot on every path
   out of a supervising future, panics included. It replaces `src-tauri`'s hand-written
   `ReleaseOnDrop` guard, which existed for exactly that property but had to be remembered
   by each caller. Counting and inserting happen under **one** lock; a caller that asked
   for a count and then inserted would have written the double-start bug with extra steps.
3. **`QueueStatus.running_task_id: Option<String>` became `running_task_ids: Vec<String>`**,
   and `QueueHandle::in_flight_task_id` became `in_flight_task_ids` plus `holds`. Wire
   visible, mirrored in `src/types.ts`. A list from the start rather than an `Option` that
   changes shape the day a mode setting is flipped. The Runs view's session-outcome
   detector became a set difference for the same reason: "the one id changed" cannot see
   two runs ending between two reads, or one ending while another starts.
4. **`QueueHandle::stop` is scoped to `LeaseOwner::Queue`.** While the maps were separate
   this was true by accident; sharing one makes it a decision. Stopping the queue is a
   statement about the queue, and a run the operator started by hand in front of them is
   not part of it. Quitting *is* a statement about everything, which is why the exit path
   calls `cancel_all` as well.
5. **The caps bound the scheduler, not a human.** A button takes `acquire_unbounded`:
   subject to the per-task exclusion and to `CONCURRENCY_CEILING`, but not to
   `max_concurrency` or the per-repository cap. Those settings are properties of the *run
   configuration* (ADR-0010), and a person clicking "Run now" with the app in front of
   them is not the mis-set-configuration failure they exist for. The ceiling is a constant
   rather than a setting, because a ceiling a user can raise is not one.

**Why.** ADR-0006 makes a rule enforced in one adapter and not the other a defect, and
"one process per task" was living in `src-tauri`. That is also why ADR-0021 could not put
`plan_task_strategy` on the MCP surface — the MCP server cannot reach a `src-tauri` type.
And it could not grow: a slot map with per-repository caps is not an `Option`, and a
second door onto it is not a back-reference.

One bug closed on the way past, worth recording because nothing tests for its absence
directly: "Plan now" claimed in the shell while the queue claimed on the database row, so
a planner and a queued run genuinely could both start for one task. Task 023's Notes name
that hazard. It is fixed by there being one registry, not by a new check.

**Binds.** 009, 012, 014, 020, 023.

---

## D21 — Where "how many runs at once" lives, and what it hands task 013

**Question.** Task 012 needs a run mode and a concurrency limit. `schedules` has carried
`mode` and `max_concurrency` columns since the initial schema and nothing reads either;
ADR-0010 calls both "properties of the **run configuration**". D4 forbids a new migration.
So where does the queue read them from, and what happens when task 013 gives a *named
schedule* its own answer to the same question?

**Decision.** Five things, and each is a thing a diff would otherwise not explain.

1. **Two `settings` keys, `schedule_mode` and `max_concurrency`, owned by
   `scheduler::capacity`.** D3's shape, exactly as `scheduler::state` already uses it for
   `queue_state`: storage through task 006's `db::settings` accessor, the rules about the
   key with the module that has the rules. `ScheduleMode` is reused rather than a second
   enum invented — it is now both `schedules.mode` and this key's value, and one spelling
   for both is what stops the two drifting. Per-repository caps come from
   `repositories.max_concurrency`, the column the 2026-09-02 migration already shipped
   (D4's amendment names it); the default is **1**, per ADR-0010.

   **The reconciliation problem this leaves is task 013's, and it is named here so 013
   inherits it rather than discovering it.** Once a schedule can say "run this list in
   parallel, three at a time", there are two answers to "what mode is the queue in": the
   active schedule's, and this default. Neither is wrong. Which one wins while a window is
   open — and what the Settings control shows while one is — is a decision, and 013 makes
   it. Task 012 took settings keys because it needs the numbers now and a `schedules` row
   nothing selects from cannot supply them; 013 layers named schedules on top rather than
   replacing this.

2. **`resolve` returns `global = 1` in sequential mode regardless of the stored limit.**
   That is what keeps sequential mode on literally the same code path as parallel instead
   of on a preserved special case, and it is what makes "turning parallelism on did not
   change sequential mode" a test rather than an assertion. The stored number is left
   alone, so flipping back restores the value the user chose — which is also why the
   Settings control shows the *stored* limit and not the resolved one.

   Reads are tolerant and writes are strict, the asymmetry `mcp::settings` already states:
   an absent, unparseable or out-of-range stored value warns and falls back, because
   ADR-0003 counts the user as a supported writer of this file and a queue that refuses to
   run all night over a typo is the worse outcome. A value from a form or a tool is
   refused with a sentence.

3. **`selection::next_batch` answers capacity; `skip_reason` learns nothing about it.**
   Eligibility ("may this task ever start") and capacity ("may it start right now") are
   different questions with different lifetimes, and only the first belongs in a set the
   card renders as a *problem*. The second is already answered, better, by
   `QueueEntry::queue_position`: the third entry of a repository capped at one reads
   `queue_position: 3, skip: None`, which is exactly "third in line" and needs no badge. A
   `SkipReason::RepositoryAtCapacity` would sit next to `UnattendedRunsNotAllowed` — true
   until the user acts — while being true for ninety seconds, and the morning review would
   then have to tell them apart. `next_to_start` is reimplemented as
   `next_batch(..).into_iter().next()`, so there is one rule and not two.

4. **A per-repository `worktree::prepare` lock, on `InFlight`.** `prepare` runs
   `git fetch --prune`, `git worktree prune` and `git worktree add` against the **shared**
   repository, and two of those take `.git`-level locks. ADR-0005's isolation is about the
   working *trees*; it says nothing about the administrative directory they are all
   registered in. `InFlight::preparation_lock(repository_id)` hands out one
   `tokio::sync::Mutex` per repository and `run_task` holds it across `prepare` and nothing
   else. It lives on `InFlight` because that is already the thing every spawner holds
   (D19's argument for there being one), and it reaches `run_task` through an
   `Option<InFlight>` on `RunRequest` where `None` skips it.

   **This is invisible until a repository's cap is lifted**, which is the whole reason to
   write it down: with a cap of 1 it can never fire, and the first time it would have is a
   raw `index.lock` error at 2am on one of two tasks that were both fine.

5. **The queue's `select!` has four arms, and `InFlight::releases` is not optional.**
   `finish_run` publishes its `ChangeEvent`s from *inside* `run_task`, while the lease is
   still held. A loop woken only by that channel counts the run that is finishing, finds no
   capacity, and sleeps — with nothing left to wake it when the lease actually drops. That
   is a queue asleep at 2am with a free slot and a full board. The `JoinSet` arm does not
   cover it either for the case that matters most: a *manual* run freeing a slot is not a
   task this queue spawned, so nothing joins. The `join_next` arm is guarded by
   `if !runs.is_empty()`, because `join_next` on an empty set returns immediately and an
   unguarded arm is a spin loop for the whole idle night.

   **And `run()` ends by draining the `JoinSet`, never by dropping it.** Dropping a
   `JoinSet` aborts its tasks; an aborted supervisor never reaches `finish_run`, so the
   attempt keeps an open `runs` row and comes back `interrupted` on the next launch for no
   reason — the exact failure `queue`'s header already argues against for one run, times N.
   The drain is the last statement of the function and sits behind no `?`.

**Why.** Every one of these is a place where the obvious choice is wrong in a way that only
shows up at 2am: a `schedules` column nothing selects from, a sequential branch that drifts
from the parallel one, a transient fact rendered as a problem, a git lock that cannot fire
until it does, a wake source that looks redundant until it is the only one, and a `Drop`
that looks like cleanup and is an abort.

One thing fixed on the way past, recorded because it is a behaviour change outside this
entry's subject: `runner::outcome::move_to_in_review` looked the bottom card up *outside*
`move_task`'s transaction, accepted in a comment as "an ordering nit in a single-user
desktop app". Two runs finishing in the same millisecond each read the same bottom card and
computed the same midpoint against it. The lookup moved into `tasks::move_task_to_bottom`,
inside the transaction that writes and under `BEGIN IMMEDIATE` — which is what the old
comment's own objection asked for ("a second implementation of the neighbour search inside
a module that has no business owning board order"), since `tasks` does own board order.

**Binds.** 012, 013, 014.

## D22 — Where the doctor's refusal lives, and what a status means

> D19, D20 and D21 were claimed by tasks 012 and 016 while this task was in flight, so this
> entry moved twice before settling here. Numbers are never reused.

**Question.** Task 018 adds eight environment checks and says "fails block queue start".
*Which* code refuses, what exactly does each status promise, and what happens to a queue
that is already running when the environment breaks?

**Decision.** Three parts.

1. **The refusal lives on `QueueHandle::start` and `QueueHandle::resume`, and nowhere else.**
   Both run the doctor first and return `Error::invalid(blocking_summary())` **without
   writing `queue_state`** — a queue that was refused is not a queue that is paused, and
   leaving state behind would make the next `resume` look like a resumption of something.
   Task 013's scheduled start goes through the same two functions, which is the whole point:
   a broken environment is reported in the evening rather than discovered in the morning.
   The MCP server inherits it for free, per ADR-0006.

2. **`try_step` is deliberately *not* gated.** Checking per-step would spawn `claude`, `git`
   and `gh` subprocesses before every task in the queue — eight probes per step, on a path
   that runs unattended for hours. Worse, it would let a transient blip (a volume briefly
   below the disk threshold, a `gh` token refreshing) halt a queue mid-flight, which is a new
   failure mode invented to prevent an old one. **The doctor is a gate at the door, not a
   guard in the corridor.** A run whose environment breaks after it started fails on its own
   terms and is classified by ADR-0011's rules, which is what those rules are for.

3. **Only `fail` blocks; `warn` never does.** The line between them is *whether the queue can
   still do its job*. No `claude` binary is a fail — every run dies immediately. An
   unauthenticated `gh` is a warn: the runs still work, only the pull-request step at the end
   is skipped, and blocking a night's work over it would cost more than it saves. A `claude`
   older than the pinned minimum warns rather than fails, because locking a user out of their
   own queue over a version comparison is worse than letting them try. Every non-passing row
   carries a `remediation` string naming the specific fix; a status without one is a bug.

**Why.** The rule that "fails block queue start" has to be enforced in exactly one place or
it is not a rule (ADR-0006) — a doctor the UI consults before enabling a button is a
suggestion, and the MCP server would not inherit it. Putting it on `start`/`resume` also
makes it testable without a UI, which is how `a_blocking_report_refuses_to_start_the_queue_and_writes_no_queue_state`
can assert the "writes no state" half at all.

The `pass`/`warn`/`fail` split is a three-way distinction on purpose. Two statuses would
force every check to choose between blocking the night and being ignorable, and the four
checks that warn are precisely the ones where neither is right.

**Consequence for tests, stated because it surprised this task twice.** `QueueHandle::start`
now spawns real subprocesses and measures the real volume, so **every test that starts a queue
transitively exercises the host environment.** Three things follow for
`crates/core/tests/scheduler.rs`, for `queue.rs`'s own unit tests, and for anything like them:

- **Any test that reaches `start()` or `resume()` must supply a stand-in `claude`, because CI
  has none.** `claude` is a prerequisite the project deliberately never bundles (ADR-0004), so
  it is on every developer's machine and on no runner. A queue built from a bare
  `RunnerConfig::default()` — whose `program` is `claude` resolved on `PATH` — therefore passes
  locally and fails in CI with a preflight refusal. This is the one place where "the same
  command" is not enough: `cargo test -p rimaia-core` is verbatim what CI runs, and it still
  disagreed, because the *environment* differed. `testing::doctor::passing_queue_environment`
  exists for exactly this and hands back a temp app directory and a runner pointing at a
  stand-in; `tests/scheduler.rs` has a richer one that also replays run fixtures.
  **The stand-in satisfies the gate rather than disabling it** — a test that switched the
  doctor off would prove less than the one it replaced.
  To check before pushing, run the suite with the CLI off `PATH`:
  `PATH=/usr/bin:/bin:$HOME/.cargo/bin cargo test -p rimaia-core`.
- The stand-in `claude` must answer `auth`, not only `--version`. A stand-in that answers
  only the latter lets the doctor's auth probe fall through to the run dispatch, which
  derives a task id from the working directory and records a phantom run in the spawn log
  every ordering assertion reads.
- **The suite requires about 1 GB free on the volume holding `TMPDIR`.** Below that,
  `disk_space` fails, `start()` refuses, and roughly twenty queue tests fail at once with
  the doctor's message rather than their own. That message names the shortage plainly, so
  it is discoverable rather than mysterious — but it is a real prerequisite for running the
  tests, not a flake.

**Task 013 in particular.** A scheduled start goes through `QueueHandle::start`, so every test
of "the queue woke at 23:00 and began" inherits all three points above — a stand-in `claude`
included. Read this before writing the first one, rather than after CI disagrees.

If that coupling ever costs more than it is worth, the fix is to inject `doctor::Environment`
into `scheduler::build` the way `InFlight` already is and the way `RimaiaServer::new` already
takes one, and let the harness supply a deterministic one. That was not done here because it
changes a signature task 012 had only just landed, and the coupling is honest: a queue whose
preflight is real is the entire point of this entry.

**Binds.** 012, 013, 018.

---

## D23 — Task 014's cross-cutting choices

**Question.** ADR-0011 fixes the retry table, the classes and the resume mechanism, and stops
there. Making a queue actually survive the five-hour wall needs a dozen smaller answers, and
several of them widen types that other tasks share.

**Decision.** Nine, taken together by task 014.

1. **`Clock` grows `sleep_until`, and `tokio::time::sleep` was refused.** The queue loop had no
   timer, and nothing publishes a `ChangeEvent` when a wall-clock deadline passes — so a
   `waiting_retry` task became due and nobody noticed until the next unrelated mutation. A bare
   `tokio::time::sleep` in the loop would have been a *second clock*: the deadline is computed
   against `Clock::now`, and a wait measured any other way is not the same quantity. Concretely
   it would have made CLAUDE.md's "a fifteen-minute backoff test finishes in milliseconds" true
   for the policy function and quietly false for the loop, which is the half that matters.

   The method is boxed rather than an `async fn` so the trait stays object-safe (the scheduler
   holds an `Arc<dyn Clock>`) without an `async-trait` dependency, which [D6](#d6) would forbid.
   `TestClock`'s instant moved from an `Arc<Mutex<..>>` to a `watch::Sender`, because a mutex can
   be read but not awaited: `advance` and `set` now resolve pending waiters as a *consequence* of
   writing, rather than through a second notification anyone could forget to send.

2. **The deadline is capped at 60 seconds before it is slept on, and the cap is not a poll
   interval.** A `tokio` timer measures elapsed *monotonic* time. A laptop suspended at 23:10 and
   reopened at 06:30 has elapsed almost none of it, so a single seven-hour timer would fire hours
   after the window it was waiting for reopened. The cap forces the loop to re-derive the answer
   from `ctx.clock.now()` shortly after each wake, which is the only reading that survives a
   system sleep. It costs at most one board read a minute, and only while something is actually
   waiting: with no deadline the loop parks on its channels and arms no timer at all.

3. **The loop's wake sources become five**, and `Step` grows `IdleUntil(DateTime<Utc>)` to carry
   the deadline out of `try_step`. A separate variant rather than `Idle` carrying an `Option`,
   because "wait for the world to change" and "wait for the clock" are different conclusions and
   conflating them either arms a timer nothing needs or sleeps through one something does.

4. **`SkipReason` grows a fifth variant, `WaitingForRetry`.** Its own doc calls the set closed
   and serialized for the Runs view, so widening it is a decision rather than a detail. It is
   justified because it is genuinely a different answer from `AlreadyInFlight`: nothing is
   running, nothing is wrong, and the card can say *when*. Collapsing the two — which is what the
   MVP did while nothing resumed a waiting task — leaves a morning reviewer unable to tell a task
   coming back at 06:00 from one that is stuck. A `waiting_retry` task with **no** `resume_after`
   still reads `AlreadyInFlight`: that wait was scheduled by something other than this policy,
   and ending it is not this module's call.

   [D21](#d21) point 3's argument against `RepositoryAtCapacity` does **not** apply here and is
   worth distinguishing, since they look alike. Capacity is true for ninety seconds and is
   already answered by `queue_position`. A retry deadline is a fact about the task that persists
   across restarts, has no other rendering, and is the difference between two states the user
   must act on differently.

5. **`usage_limit_pause_until` is a `settings` key owned by `scheduler::pause`**, in [D3](#d3)'s
   shape, exactly as `scheduler::state` and `scheduler::capacity` already use it. ADR-0011 says a
   usage-limit hit "pauses new starts globally for the duration of the wait, in both modes" and
   does not say where that lives.

   **Stored, not in memory**, because the case that matters is a relaunch at 03:00: a queue that
   forgot the hold would burn a start proving the window is still closed. `note_usage_limit`
   keeps the **later** of two instants, so a second limit reporting an earlier reset cannot
   shorten a pending wait. `try_step` reads it *before* the plan, so both modes honour it by
   construction rather than by each having a branch — the same property `capacity::resolve` buys
   by making sequential mode `global = 1`. In-flight runs are deliberately not killed: a run
   mid-edit when another task hits a wall has done nothing wrong, and this is a rule about
   starting. It is surfaced on `QueueStatus` for the reason `last_step_error` is — a hold the
   operator cannot see is one they will debug as a bug.

6. **`--max-turns` gains a default and a `settings` key, and this changes every implementation
   run's argv.** ADR-0011 asks for it per attempt; the flag existed on `Invocation` and was never
   set. The default is **300**, and the number is chosen against two constraints pulling opposite
   ways: a turn limit classifies as `fatal` (no retry), so a budget set too low does not cost a
   retry, it *abandons the task* half-done under a verdict the operator did not choose — while a
   budget set too high does not bound the runaway. The exact-vector assertions in
   `tests/runner_process.rs` and `tests/runner_strategy.rs` change with it, which is expected and
   not a regression.

7. **The attempt count is derived from `session_id` and must never become a column.** There is no
   attempt-count column, [D4](#d4) forbids a migration anyway, and the deeper reason is that a
   counter is a second source of truth for something the rows answer exactly. `scheduler::attempts`
   reads `runs` newest-first and counts backwards **only while `session_id` matches**, which is
   what ADR-0011's "each attempt is a row sharing the task's session id" means operationally: a
   task the user re-queued in the morning starts a new session and gets a fresh budget, while
   last night's attempts stay on the board as history.

   `history` takes the ending attempt as a parameter rather than reading it back, because it is
   called at the one moment the newest row cannot answer for itself — after `execute` returns and
   *before* `finish_run` closes the row, since what `finish_run` writes is the thing being
   decided. Two inputs are only in the outcome at that point: `exit_class`, still NULL on the row,
   and the reported reset time, which has no column at all.

8. **`RunOutcome` keeps `usage_limit_resets_at` *and* gains `resume_after`.** One is what the CLI
   said, the other what the policy decided — ADR-0011's "reset plus jitter", which for a
   `transient` ending is not derived from the first at all. A single field would leave a morning
   reviewer unable to tell "the window reopened at 06:00 and we waited until 06:41" from "we
   invented 06:41". Only the second is persisted. `apply_to_task` consequently routes on
   class-**plus-decision**: a retryable class with no deadline is a spent budget and lands
   `failed`, which is what keeps an exhausted task out of a state nothing will ever leave.

   Jitter is a deterministic FNV-1a of the run id, not a random number. [D6](#d6) forbids the
   dependency (`rand` included), a spread that is stable per run is easier to reason about at 2am,
   and a test that had to tolerate randomness would assert less.

9. **The synthesized-fixture discipline.** `spike/FINDINGS.md` §4 and ADR-0011's 2026-08-20
   amendment both record that the `rate_limit_event` payload when `status` is not `"allowed"` has
   never been observed. The two fixtures task 014 adds are edited copies of
   `interrupted-sigterm.jsonl` — not `success.jsonl`, because a limited run does not complete —
   with exactly two changes inside the existing event: the status value, and a pinned `resetsAt`.

   They get their **own README section and their own `SYNTHESIZED_UNOBSERVED` list**, separate
   from both the recordings and the three parser-edge synthetics, because they make a weaker
   claim than either: those synthesize a *shape* against a real payload, these synthesize a
   *value nobody has seen*. `the_usage_limit_fixtures_are_labelled_unobserved_rather_than_recorded`
   is what stops a later agent promoting them by accident.

   The invented word is not load-bearing, and proving that is what makes shipping the guess
   acceptable: the classifier matches on "not `allowed`" and never on a value, asserted by
   `a_status_the_corpus_never_saw_still_reads_as_a_usage_limit` over five words. Replace both
   files byte-for-byte the first time a real queue hits the wall, and delete the section.

**Also decided, and smaller.** `claim::claim_retry` is a **sibling** of `claim`, not a branch
inside it: a single function that read the row and then routed would do the read *outside* the
transaction, reintroducing the race the module exists to close. `claim::release`'s refusal to
overwrite `waiting_retry` becomes load-bearing rather than defensive. `QueueEntry` gains
`resume_after`, populated **only** for a task in `waiting_retry`, so a task started again by hand
does not look like a continuation because of an old deadline on its last run. And
`crates/core/src/testing/cli.rs` is `tests/scheduler.rs`'s stand-in promoted behind the `testing`
feature, with a second dispatch axis (task **and attempt**) plus per-attempt argv and stdin
capture; the old header's argument against sharing was about `mod common` between test binaries,
which a feature-gated module is not.

**What is deliberately *not* here.** `retry_task_now` gets a Tauri command and **no MCP tool**,
against ADR-0021's parity rule, because ADR-0021's own 2026-09-02 amendment names task 014 and
says so: "tasks 012 and 014 deliberately do not ship the tool... shipping a process-spawning tool
is a separate decision with its own scope argument". `give_up_on_task` spawns nothing and ships as
both. And `retry::decide` takes **no run window**, though ADR-0011 says a usage-limit wait is
"capped by the run window": windows are task 013's, and 013 adds a parameter to that function
rather than a second policy beside it.

**Why.** Every one of these is a place where the obvious choice is wrong in a way that only shows
up at 2am — a sleep that is not the injected clock, a timer trusted across a system suspend, a
budget stored as a counter that drifts from the rows, a reported time and a decided time collapsed
into one field, a turn limit set low enough to abandon a task under a verdict nobody chose, and a
guessed payload value that the classifier must never depend on.

**Binds.** 013, 014, 015, 019.

---

## D24 — Task 013's cross-cutting choices

**Question.** ADR-0010 fixes the three triggers, the run window and the modes, and stops
there. The `schedules` table has carried `mode`, `max_concurrency`, `cron`, `start_at` and
`enabled` since the initial schema and the 2026-09-02 migration added four more columns —
none of them read by anything. Turning that into a queue that starts itself at 22:00 needs a
dozen smaller answers, and [D21](#d21) explicitly handed one of them to this task by name.

**Decision.** Eight, taken together by task 013.

1. **The four columns, and what each one means.**

   `timezone` is an **IANA name**, never an offset and never an abbreviation. Nullable in
   the schema and **required by the service for every row it writes** — a
   `NOT NULL DEFAULT 'UTC'` would let a nightly schedule be created silently in the wrong
   zone, which is exactly the failure the DST acceptance criterion exists to catch. It is
   the one read in this codebase that is **strict where every other `settings`-shaped read
   is tolerant**: the tolerant rule is right for a key whose fallback is *safe*, and there
   is no safe fallback for a zone. Reading an unknown name as UTC is how a nightly queue
   runs at 23:00 in January and 22:00 in June with nothing to say so.

   `stop_at` is a **local wall-clock time of day, `HH:MM`** — not an instant, and not a
   duration. "Stop at 06:00" is the sentence the user says; a recurring window needs a
   repeating stop, which an absolute instant cannot express, and a duration column would
   move the stop whenever the start moved *and* end a spring-forward window an hour early.
   Resolved through the schedule's own `timezone`, so a window crossing the gap is seven
   real hours and still ends at 06:00 local.

   `last_fired_at` is when the schedule **actually fired**, never when it was due. That
   distinction is the whole of what makes ADR-0010's "fires late rather than skipping" work
   without becoming a re-fire loop: the occurrence is in the past, the fire is now, and
   comparing against *now* is what stops the same missed night firing again a millisecond
   later. It is written even when the doctor refuses the start, because the schedule did
   fire — what it found was a broken machine — and not writing it turns a missing `claude`
   into eight subprocess spawns a minute until morning.

   `armed_at` is the instant from which missed occurrences count: set on create, re-set on
   every enable, by both doors. Without it a nightly 22:00 schedule created at 23:00 fires
   immediately for an occurrence that predates its own existence, and one disabled for a
   month fires the second it is re-enabled. **The recurring baseline is
   `max(last_fired_at, armed_at)`.**

2. **Run now is not a `schedules` row, contradicting the initial schema's own comment.**
   That comment anticipated one — "a cron expression with a timezone, or a wall-clock time,
   **or neither for run now**" — and task 013 declines it. `QueueHandle::start` already *is*
   Run now: it is the button, it runs the doctor, and it flips the switch. A row nothing
   ever fires would be a second spelling of that button, with its own enable toggle to leave
   in the wrong position and its own next-fire time to render as "never". `schedule::fire`
   refuses such a row with a message that names the button, so the absence reads as a
   decision rather than an omission. Recorded here because the schema expected otherwise.

3. **The timer is a third arm of the queue's existing `select!`, not a second task.** Three
   reasons, in order of weight. ADR-0010 makes the scheduler **the only component allowed to
   move a task into `running`**, so a separate timer calling `QueueHandle::start` would be a
   second decider racing `try_step`'s own switch re-checks — the exact window `queue`'s
   mid-claim section was written to close, reopened from the other side. ADR-0018's "another
   `subscribe()` and no coordination with anyone" is about *subscribers*, and a timer is not
   one; this is the same loop learning to wake on time as well as on events, which it
   already learned to do for ADR-0011's deadlines. And it costs one future in a `select!`
   whose arms are already cancel-safe.

   The order is shutdown → `drain` → **`tick_schedules`** → `step`, and `tick_schedules`
   running first is load-bearing: it **closes a window before anything selects**, so a task
   cannot be claimed one millisecond after the night was meant to end.

   **The deadline cap is [D23](#d23) point 2's, and it is still not a poll.** The schedule's
   next wake is folded into the same deadline the retry arm computes — the earlier of the
   two — and capped at 60 seconds before it is slept on, because a `tokio` timer measures
   elapsed *monotonic* time and a laptop suspended at 23:10 and reopened at 06:30 has
   elapsed almost none of it. A single seven-hour timer to a 22:00 occurrence would fire
   hours late. The cap forces the loop to re-derive the answer from `ctx.clock.now()`, which
   is the only reading that survives a system sleep, and it arms nothing at all while
   nothing is waiting. The timer feeds it `next_wake_at` and never `next_fire_at`: the
   latter reports an *overdue* occurrence, which is in the past, and a deadline in the past
   resolves immediately and would spin the loop until morning.

4. **The active window lives in `settings` under `active_run_window`, owned by
   `schedule::window`.** [D3](#d3)'s shape, exactly as `scheduler::state` uses it for
   `queue_state` and `scheduler::pause` for the usage-limit hold. [D4](#d4) forbids a
   column, and a column would be wrong anyway: at most one window is open, so this is a
   singleton fact about the installation, which is what that table is. **Stored rather than
   held in memory**, for `pause`'s reason: a window opened at 22:00 must still know it
   closes at 06:00 after a relaunch at 03:00.

   It carries the schedule's **name** as well as its id, denormalised on purpose. The Runs
   view says "Running until 06:00 — Nightly", and a caption that re-read the row would fail
   the moment the schedule was renamed or deleted mid-window. The window is a record of what
   was decided at 22:00; it does not become untrue afterwards.

5. **The [D21](#d21) reconciliation, settled: the open window wins, the default wins
   whenever none is open.** `capacity::resolve` reads `window::active` first and takes its
   `mode` and `max_concurrency` over the `schedule_mode` / `max_concurrency` settings keys.
   Three reasons: the schedule is the more specific instruction and the more recent
   deliberate act; it is what makes ADR-0010's own `schedules.mode` and
   `schedules.max_concurrency` columns mean anything at all, which D21 point 1 deferred only
   because "a `schedules` row nothing selects from cannot supply them"; and a manual Start
   opens **no** window, so the button still resolves against the settings keys and nothing
   about task 012's behaviour changes on a night nobody has scheduled.

   **And what the Settings control shows while a window is open: the stored default,
   unchanged.** That is D21 point 2's own argument one layer out — the control already shows
   the stored `max_concurrency` rather than the `1` sequential resolves to, because a number
   that changed every time a mode was flipped would look forgotten. One that rewrote itself
   at 22:00 would be worse: it would read as the user's own setting having been silently
   changed. "What is happening right now" belongs on `QueueStatus`, which carries the window
   itself. The window's number is still clamped to `CONCURRENCY_CEILING` on read, by the
   same helper a hand-edited repository cap goes through.

6. **Late firing coalesces, and the window's own stop time bounds it.** `due` asks for the
   **most recent** occurrence at or before now, not for a walk forward from the baseline, so
   three nights asleep produce one instant and one fire. Honouring the *oldest* missed
   occurrence instead is the reading that never runs: its stop time was three mornings ago.
   And the newest one is bounded too — a machine woken at 11:00 on a 22:00-to-06:00 schedule
   has genuinely missed the night, and `Due::Expired` says so rather than starting a full
   night's work in the middle of a working morning. **An expired occurrence writes no
   `last_fired_at`**, because that column means "it fired" and lying to it to save a
   recomputation would cost more than the one cron search it saves.

7. **`ChangeEvent::Schedules(Arc<[ScheduleId]>)` is a new variant, not a reuse of
   `Settings`.** `settings` is a key/value table whose every consumer re-reads all of it,
   which is why that variant carries no ids; `schedules` is a table of **entities** the user
   creates, names, edits and deletes, the same kind of thing `tasks` and `repositories` are,
   so it carries ids for the same reason they do. A panel listing thirty schedules is not
   obliged to re-read them because a base-instructions textarea was saved.

   The *window* is a settings key and does announce itself as `Settings`. That is not an
   inconsistency: it is one singleton fact, and the Runs view reading it re-reads the whole
   queue status anyway.

8. **A doctor refusal is recorded stickily on `last_step_error`.** Task 018's preflight runs
   before a scheduled start — D22 point 1 promised exactly this — and a blocking report does
   not flip the switch. But the ordinary `last_step_error` is cleared by the next pass that
   gets all the way through, and the queue a refusal left `paused` returns from `try_step`
   at its switch check on *every* pass, having proved nothing. Left non-sticky, the message
   the user is meant to find in the morning would be cleared microseconds after it was
   written. It is cleared instead by the two things that genuinely supersede it: a fire that
   got through, and a human pressing Start.

**Why.** Every one of these is a place where the obvious choice is wrong in a way that only
shows up at 2am, or at 09:00 the next day: a zone defaulted to UTC, a stop time stored as an
instant that cannot repeat, a fire time recorded as the occurrence so the same night fires
forever, a month of disabled nights arriving at once, a timer task racing the one component
allowed to start a run, a deadline in the past spinning a loop until morning, five missed
occurrences opening five windows, and a refusal cleared before anybody read it.

**What is deliberately *not* here.** `retry::decide` gains the run-window parameter
[D23](#d23) reserved for this task, and ADR-0011's "capped only by the run window" now binds
the **usage-limit row only** — a transient backoff is at most fifteen minutes, and a window
with less than fifteen minutes left is about to close anyway, so extending the cap there
would spend a task's retry budget on the clock rather than on the failure. And there is no
per-task schedule and no schedule-level task filter: ADR-0010 rejected the first, and the
second would be a second answer to "what runs next" beside board order (ADR-0007).

**See also** [D15](#d15)'s 2026-09-03 amendment, which settles what a schedule means for
quitting and for a crashed run's resume, and ADR-0010's, which takes the three refinements
large enough to be argued at product scale.

**Binds.** 013, and 023 as the next task to read `PreflightSummary`.
## D20 — Task 016's cleanup: what it refuses, what it may not be forced past, and what it never deletes

**Question.** Task 016 removes worktrees. ADR-0005 fixes where they live, that cleanup is
"explicit and never automatic on failure", and that the branch is left alone unless asked
for; ADR-0022 part 2 fixes that nothing here deletes a `runs` row. Everything else is a
judgement about *deletion*, which is the one irreversible thing this app does — and a
reviewer meeting a guard in a diff has nothing to check it against unless it is written
down. Which guards exist, which of them a user may override, where automatic removal lives
and with what authority, how files with no database row get an age, and which of these
capabilities deliberately never reach MCP.

**Decision.** Six, taken together by task 016:

1. **Four guards, and exactly one of them has no override.** In the order
   `cleanup::remove_worktree` applies them:

   | Guard | Overridable by | Why |
   | --- | --- | --- |
   | Task is `running` or `waiting_retry` | **nothing** | A process is writing in that directory |
   | Uncommitted changes | `uncommitted_changes: confirmed_by_user` | Work committed nowhere at all |
   | Unpushed commits | `unpushed_commits: confirmed_by_user` | Work that exists on exactly one disk |
   | Branch is not merged | `branch: delete_even_if_unmerged` | The only copy of the run's commits |

   The first is the one to argue for, because it is the one that looks like an omission.
   Every other refusal here is about the user's appetite for risk, and a confirmation is
   the right shape for that. This one is not about their judgement: removing a directory
   a Claude Code process is writing in produces a half-deleted checkout, a run that fails
   on an unreadable error, and a `git worktree` record pointing at rubble. There is no
   answer to "are you sure?" that improves that outcome, so the question is not asked and
   there is no flag to pass. `waiting_retry` is included on `worktree::correct_run_state`'s
   reasoning — it means "a process is about to be", and the gap before the next attempt is
   not a window in which the directory is spare.

   The three overridable answers are **three separate fields, not one `force`**, because
   they protect three different things and one flag would let a user who meant "yes, drop
   that scratch file" also authorise losing a branch. `RemovalAuthorization::default()` is
   the refusing value in all three axes, and a test pins that, so a field added later has
   to be given a safe default deliberately rather than by whatever `Default` derives.

   **The uncommitted-changes refusal states the count** ("3 uncommitted changes"), because
   the count is what makes it a decision rather than a shrug: one stray log file and
   forty-seven edited source files are not the same question.

   "Merged" is `git merge-base --is-ancestor <branch> <base>`. It says **no** for a branch
   that was squash-merged or rebased, since those produce different commits and git cannot
   tell them from work that was never merged. That false negative costs a click; the false
   positive would cost a commit.

2. **Bulk actions report; the single action errors.** "Remove all `done`" and "remove all
   merged" return a `CleanupReport` carrying both what went and what was refused, with each
   refusal's own sentence. A bulk action that aborted on its first guard would leave a user
   unable to reclaim nine safe worktrees because the tenth is dirty, and would not say
   which. The single-worktree call returns `Result` instead, because there the user asked
   about exactly one thing and a refusal *is* the answer. Both bulk actions run with
   `RemovalAuthorization::default()` and nothing else: one click standing in for N
   decisions may not carry more authority than the user would have granted one at a time.

3. **Auto-removal on `done` lives in `tasks::move_task`, and creates the first `tasks` →
   `worktree` edge.** In the service, not in a command, so the board and the MCP server get
   it identically (ADR-0006) — "the worktree disappears when I move the card" would be a
   conspicuous rule to enforce on only one door. The edge runs opposite to every existing
   one (`worktree` reads tasks and calls `set_run_state`); Rust permits the cycle within a
   crate, the direction is the honest one because the policy belongs to the *transition*
   rather than to the directory, and it is named here rather than met as a surprise.

   Its posture is fixed: **every force off, the branch always kept**, and it is *best
   effort* — the call returns nothing and a refusal is logged, never propagated. The move
   has already committed and published by then, and a cleanup a guard declined must not be
   able to report the move as having failed. An automatic action gets strictly less
   authority than a human clicking a button, because there is nobody present to read the
   refusal it would otherwise be overriding.

   The setting is `settings["worktree_auto_cleanup"]`, owned by `worktree::cleanup` in the
   shape [D3](#d3--who-owns-settings-storage-vs-the-typed-accessor) fixed and
   [D16](#d16--task-010s-cross-cutting-choices).2 repeated. **Off by default with no seeded
   row** — an absent key *is* off, which makes "off by default" true of an unconfigured
   database rather than of a migration ([D4](#d4--migration-file-numbering) forbids one).
   Its "on" value is spelled `on_done_acknowledged`, not `true`: task 016 requires that
   enabling it means acknowledging what it deletes, and the spelling is how that
   acknowledgement survives past the dialog that collected it into the row itself.

4. **`prune_logs` gains a filesystem sweep for `strategy-*.jsonl`, dated by mtime.**
   [D17](#d17--task-020s-cross-cutting-choices).5 already warned that a strategy run has no
   `runs` row, so "anything that enumerates transcripts through the database misses them".
   `runs::prune_logs` was exactly that, while `runs::total_log_size` walks the filesystem
   and had been counting them all along — so Settings reported disk the prune button could
   not reclaim, and the number never fell as far as it promised. The sweep matches on
   `runner::STRATEGY_TRANSCRIPT_PREFIX`, never a literal, and `prune_logs` therefore takes
   an `AppPaths`.

   The age rule is genuinely different, not merely differently implemented, because there
   is no `started_at` and no `ended_at` to read:
   - `older_than_days(n)` → mtime at least `n` days old, across every task's directory.
   - `task(id)` → that task's directory, with no age of its own; the user named the task.
   - **Both are floored at one hour.** That floor stands in for the `ended_at IS NOT NULL`
     guard the row-based half gets for free: a file written in the last hour may be one a
     planner is writing right now, and there is no row to ask.

   `PruneResult` counts these separately from `runs_pruned`. Adding them together would
   report more runs pruned than the database holds.

5. **Nothing in task 016 deletes a `runs` row.** ADR-0022 part 2, restated here because it
   binds a module that ADR does not otherwise touch: worktree cleanup reclaims disk, and
   the record of what a run cost is not disk worth reclaiming. `runs::prune_logs` deletes
   files and leaves every row; `worktree::cleanup` deletes directories and branches and
   touches the `runs` table not at all. **The one exception is not this module's**:
   deleting a *task* still cascades to its runs through `ON DELETE CASCADE`, because that
   is a person saying "this never happened", which is a different act from the disk being
   full.

6. **The three destructive commands have no MCP tool, deliberately.** ADR-0021 point 1
   makes a Tauri command without a tool a defect, and point 5 names the standing exception:
   `delete_task` "stays absent from both … a decision about destructiveness, not about
   which client is privileged". `remove_task_worktree`, `cleanup_done_worktrees` and
   `cleanup_merged_worktrees` join it on the same ground, and with a sharper edge —
   `remove_task_worktree` with both forces set destroys work that exists in no commit and
   on no remote, which is strictly more than `delete_task` can do, and a run-scoped agent
   could reach its own directory. They live only where a human confirms them.

   What *does* ship is the read and the policy: `list_worktrees`,
   `get_worktree_auto_cleanup` and `set_worktree_auto_cleanup`, all
   `RunAccess::Refused`. The setter is ADR-0021 point 4's "reconfigures the installation"
   clause verbatim. The two reads are refused on a narrower ground of their own — an
   inventory is by construction an enumeration of every *other* task's directory, which is
   [D16](#d16--task-010s-cross-cutting-choices).6's objection to `list_tasks`, and a run
   has no business knowing what else is on the disk, still less that its own directory is
   the one due to be reclaimed. Refusing the read as well as the write would have left an
   operator's agent unable even to explain a full disk, which is the half of the problem it
   can help with without being able to make anything irreversible.

**Why.** Every one of these is a place two agents would have answered differently and a
reviewer could not have said which was right. (1) is the whole substance of the task —
task 016's Notes make "if in doubt, refuse and explain" the design rule, and a guard set
that lives only in a match arm is one a later task widens without noticing that the
override it adds is the one that had no override on purpose. (2) and (3) are about
*authority*: who is deciding, and how much less a machine gets than a person. (4) is a rule
about files that no query can find, which is the definition of something that has to be
written down rather than discovered. (5) is ADR-0022 reaching a module it does not name.
(6) is a deliberate hole in a parity rule, and ADR-0021 is explicit that the way to have
one is to argue it, not to leave it.

See also [ADR-0005](adr/0005-git-worktree-per-task.md) (where worktrees live, and that the
branch is left alone), [ADR-0021](adr/0021-mcp-first-tool-surface.md) points 4 and 5 (the
scope decision and the destructiveness exception), and
[ADR-0022](adr/0022-what-a-run-is-remembered-by.md) part 2 (rows survive pruning).

**Binds.** 016, 024.

---

## How to use this

An implementation task reads the entries its number appears in, before writing code:

| Task | Entries |
| --- | --- |
| [002](../tasks/002-sqlite-store-and-migrations.md) | D1 · D3 · D4 · D5 · D9 · D10 · D11 |
| [003](../tasks/003-repository-registration.md) | D5 · D6 · D8 · D10 |
| [004](../tasks/004-task-crud-and-service-layer.md) | D1 · D2 · D5 · D8 · D9 · D10 · D12 · D13 |
| [005](../tasks/005-kanban-board-ui.md) | D1 · D2 · D6 · D7 · D9 · D12 · D13 |
| [006](../tasks/006-base-instructions-and-prompt-composition.md) | D3 · D4 · D5 · D8 |
| [007](../tasks/007-git-worktree-service.md) | D5 · D8 · D10 · D13 |
| [008](../tasks/008-claude-code-runner.md) | D2 · D3 · D5 · D7 · D8 · D9 · D10 · D14 · D15 · D18 |
| [009](../tasks/009-sequential-run-queue.md) | D2 · D5 · D7 · D8 · D9 · D10 · D14 · D15 · D19 |
| [010](../tasks/010-local-mcp-server.md) | D2 · D3 · D4 · D5 · D6 · D8 · D10 · D12 · D13 · D16 |
| [011](../tasks/011-task-dependencies-and-blocking.md) | D4 · D12 · D16 |
| [012](../tasks/012-parallel-execution.md) | D2 · D4 · D5 · D8 · D9 · D12 · D14 · D15 · D19 · D21 · D22 |
| [013](../tasks/013-run-scheduling.md) | D4 · D6 · D15 · D21 · D22 · D23 · D24 |
| [014](../tasks/014-usage-limit-resilience.md) | D3 · D4 · D5 · D8 · D9 · D12 · D14 · D15 · D19 · D21 · D22 · D23 |
| [015](../tasks/015-run-history-and-log-viewer.md) | D14 · D18 · D23 |
| [016](../tasks/016-worktree-lifecycle-and-cleanup.md) | D17 · D18 · D20 |
| [018](../tasks/018-preflight-doctor-and-packaging.md) | D11 · D16 · D22 |
| [020](../tasks/020-per-task-execution-strategy.md) | D2 · D3 · D4 · D5 · D8 · D10 · D12 · D16 · D17 · D19 |
| [021](../tasks/021-review-and-fix-loop.md) | D17 |
| [024](../tasks/024-analytics.md) | D4 · D5 · D12 · D18 · D20 |
| every task | D4 and D6 as prohibitions |

A reviewer treats any decision visible in a diff that is neither in an ADR nor here as a
finding, **even when the code looks right**. The objection is not that the choice was bad; it
is that the next agent has no way to inherit it.

Adding an entry: append it with the next free `D` number, in the same four-part shape. Never
renumber — same rule as ADRs and tasks. If an entry grows into an architectural decision, write
the ADR and replace the entry's body with a one-line pointer, as D2 shows.
