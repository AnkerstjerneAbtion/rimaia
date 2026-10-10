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

### Amendment, 2026-09-04 — a sixth, and it is the ask this entry demands

D4's body says *"a task that believes it needs one stops and asks."* Task 022 believed it, and
this is the answer:

```
src-tauri/migrations/20260904120000_repository_credentials.sql   (task 022)
```

Three nullable columns on `repositories` — `credential_login`, `credential_label`,
`credential_added_at` — and **no secret**. The token is in the OS keychain
(`crates/core/src/credentials/`); these are the metadata a settings pane needs to say *that* a
credential exists, whose it is and when it was added, none of which the keychain has a good way
to hold. `credential_added_at` doubles as the flag the spawn path reads, which is deliberate:
asking the *keychain* whether anything is there would read a locked one as "no credential
configured" and fall straight through to the operator's ambient login, which is the exact
failure ADR-0020's fail-closed rule exists to prevent.

The timestamp sorts after every migration already on disk, which is this entry's own 2026-09-02
lesson applied rather than restated.

**The count is now six, and six is the whole list.** Nothing else on task 022's branch adds
one — task 024's subscription figure and task 027's dismissal set are `settings` keys, on D3's
own argument, and both say so at their accessor. The prohibition is otherwise unchanged: a task
that believes it needs a seventh stops and asks.

**Binds.** 022, in addition to everything this entry already bound.

### Amendment, 2026-09-15 — a seventh, and it is the ask this entry demands again

Task 030 believed it needed one. It does, and this is the answer:

```
src-tauri/migrations/20260915120000_archive_and_on_archive.sql   (task 030)
```

Three columns, no table. `tasks.archived_at` is ADR-0025's third axis — nullable, because
`NULL` *is* "on the board" and the alternative is a boolean plus a timestamp that can
disagree. `repositories.on_archive` and `repositories.on_archive_script` are the
per-repository cleanup slot; `on_archive` is `NOT NULL DEFAULT 'none'` with a `CHECK`
spelling the whole closed domain, for the reason `20260826120000_task_source.sql`'s header
gives — SQLite cannot widen a `CHECK` afterwards, so the domain is stated once rather than
grown.

This one could **not** have ridden along with an earlier file the way task 024's capture
columns rode with task 013's. Every existing migration is merged and applied on the user's
own database, and ADR-0003 makes those append-only outright.

The timestamp sorts after every migration already on disk, which is this entry's 2026-09-02
lesson applied rather than restated.

**The count is now seven, and seven is the whole list.** Nothing else on task 030's branch
adds one: there is no new `settings` key at all, because ADR-0025 point 4 puts the policy on
the repository row where the repository's own infrastructure is described, not in the
key/value table. The prohibition is otherwise unchanged: a task that believes it needs an
eighth stops and asks.

**Binds.** 030, in addition to everything this entry already bound.

### Amendment, 2026-09-30 — team mode's thirteen files, see D28

Team mode's nine board and four runner migration names are reserved in
[D28](#d28--team-modes-schema-one-rebuild-additive-files-around-it-and-the-ddl-of-each)'s D4
amendment, beside the DDL they carry. The count is now sixteen board files and four runner
files.

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

### Amendment, 2026-09-30 — two caches from task 040, see D33

From task 040 there are two offline query caches, `crates/core/.sqlx/` and
`crates/runner/.sqlx/`, with the regeneration recipe in
[D33](#d33--two-offline-query-caches-one-per-schema-amends-d5). The body above holds for
every task before 040.

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

### Amendment, 2026-09-04 — the rule was never npm-only, and two Cargo entries (task 022)

**This entry's prohibition extends to Cargo dependencies of `crates/core`, explicitly.** It has
always been read that way — task 018 argued its way *out* of a `semver` dependency in
`doctor::checks`, and task 026 declined a charting and a terminal-detection crate — but the body
above only lists npm packages, and the next agent reading it cold could reasonably conclude the
Cargo side is ungoverned. It is not: the collision argument (two branches editing one lockfile,
resolved by regenerating it, silently reverting the other) is identical, and the review
argument is stronger, because a Cargo dependency links into the binary an unattended agent runs.

Two are approved, both for task 022 and both in `crates/core`:

- **`keyring` v3**, features `apple-native`, `windows-native`, `sync-secret-service`,
  `crypto-rust`. The requirement rather than a convenience: ADR-0020 puts the token in the OS
  keychain and there is no hand-rollable version of three platform secret stores. All backends
  are on because "cross-platform is a requirement of this task, not a follow-up" — a build that
  compiled only the host's would let the other two rot between releases. On Linux it links
  libdbus, which is the one system package this crate needs anywhere and which `ci.yml` installs
  on that platform only; ADR-0015's "no display server, WebKit or GTK" is untouched.
- **`base64` 0.22**, for the `http.https://github.com/.extraheader` value. Already in
  `Cargo.lock` transitively through `reqwest`, so promoting it to a direct workspace dependency
  costs **no new tree**. A twenty-line encoder was considered and declined: base64 is a place
  where a subtle error produces a header the forge rejects with "bad credentials", which is the
  least diagnosable failure this feature has, and the crate is already being compiled either way.

**No new npm dependency on task 022's branch, or anywhere else on it.** Task 024's charts are
layout and a span with a height (its own Out of scope: "a bar chart is not worth a bundle"),
task 026 reuses `tauri-plugin-opener` and task 025 reuses `tauri-plugin-dialog` — both already
in `package.json`, `Cargo.toml` and `capabilities/default.json`.

**Binds.** 022, 024, 025, 026, and every later task as a prohibition — on both sides of the
crate boundary.

### Amendment, 2026-09-30 — team mode's list, see D34

Three npm packages and eight Cargo entries are approved for team mode, each for one named
task, in [D34](#d34--team-modes-dependencies-approved-up-front-a-d6-amendment). The list is
still closed.

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

### Amendment, 2026-10-10 — `Conflict`, the fencing code (task 043)

ADR-0031 point 3 asks for this amendment: a report under a lease that is not the current one is
refused with a code of its own. `ErrorCode` gains **`Conflict`**, serialised `"conflict"`, built
with `Error::conflict(..)`, and `src/types.ts`'s union gains `"conflict"` with no rendering
change, because no solo path can produce it.

It means **only** "your lease is not the current one": the task exists in the lease's team, and
its lease row is gone, or has another generation, or another runner (`board::lease::current`). A
task that does not exist, or is outside the lease's team, stays `NotFound` in the sentence a
never-issued task gets (D31 point 3). A refusal a person reads stays `Invalid`, the pinned-task
refusal included. Nothing may treat `Conflict` as a lost race: a fenced report is not one, and
D31 point 11 gives it its one reaction (task 053).

It is the first of D32 point 3's three variants; task 046 adds `unauthenticated` and
`upgrade_required`, on the same terms.

**Binds.** 043, 046, 052, 053, 056.

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

### Amendment, 2026-09-04 — the dialog, and what was actually measured (task 025)

The case above is closed. **`src-tauri/src/lib.rs`'s setup hook now shows a native error
dialog on every fatal path, before the process exits** — the failing step, the reason, and
the directory holding the log files. Everything else about this entry stands unchanged: the
window still never opens, the exit is still non-zero, and every step still writes its stderr
line and its log line *before* the dialog, so a bundle that cannot draw one loses nothing it
had.

**`blocking_show()` on the setup hook's own thread is safe, and here is why.** Three facts,
each read out of a dependency rather than assumed:

1. The setup hook runs on the main thread **inside the already-running event loop** —
   `RuntimeRunEvent::Ready`, `tauri-2.11.5/src/app.rs:1424`.
2. `blocking_show()` posts through `AppHandle::run_on_main_thread` and then blocks the caller
   on a channel. That is a deadlock only if the post has to wait for this thread to return,
   and it does not: `tauri-runtime-wry`'s `send_user_message` (2.11.4, `src/lib.rs:239`)
   executes the closure **inline** when the caller is already the main thread.
3. The dialog never touches the run loop at all. `rfd`'s macOS `AsyncMessageDialogImpl`
   (0.16.0, `src/backend/macos/message_dialog.rs:172`) branches on whether a **parent window**
   was set; `tauri-plugin-dialog` sets one only if the caller asks, and this one does not.
   With no parent it takes `utils::async_pop_dialog` — a `CFUserNotificationDisplayAlert` on
   a thread of its own.

Point 3 is the one worth carrying forward, because it is not the mechanism this entry
predicted. The obvious hazard — a *sheet* attached to the window `tauri.conf.json` declares
`"visible": false`, which nobody could see and nothing would answer — lives in rfd's
`ModalFuture`, and `ModalFuture` is reached **only** on the explicit-parent branch. An
implementation that passes `.parent(&window)` would meet exactly the deadlock this entry
feared. Do not add one.

**Verified against a real bundle, 2026-09-04, macOS 15 (aarch64).** `npm run tauri build`
with a deliberately failing migration added to `src-tauri/migrations/`, launched from the
built `Rimaia.app`:

- `sample` on the live process shows the main thread parked in `report_startup_failure` →
  `blocking_show` → `mpmc::Receiver::recv`, and a second thread in
  `rfd::backend::macos::utils::user_alert::UserAlert::run` → `CFUserNotificationDisplayAlert`
  → `CFUserNotificationReceiveResponse`. A real alert, on screen, waiting for a person.
- The process resumed the instant the alert was answered and ended **non-zero** — 134, the
  `SIGABRT` of this entry's own panic-at-`Ready`, which is pre-existing and unchanged (the
  panic crosses an `extern "C"` frame, so it aborts rather than unwinds).
- The log file held the `ERROR startup failed; the window will not open` line naming the step
  and the sqlx error, exactly as before.
- Rebuilt without the broken migration: startup succeeds, the window opens, and `sample`
  finds no `CFUserNotification` frame anywhere in the process. **A successful launch is
  byte-identical to before this task.**

Not claimed: Windows and Linux. The code is one call and is platform-neutral, but the reading
above is macOS's, and this entry does not assert what nobody ran.

**Binds.** 002, 018, 025.

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

### Amendment, 2026-10-04 — the last run has a kind, see D29

The bulk read's last-run summary now says which kind of run it was. D29 states the rule and the
query change. This entry's cost argument is unchanged.

### Amendment, 2026-10-10 — the summary carries the review loop (task 037)

`TaskSummary` gains `review_loop: Option<ReviewLoopSummary>`, the same field `TaskDetail`
gained in task 021, built by the same pure function (`review_loop::history::summary`), so the
card and the panel cannot disagree about a loop. `ReviewLoopSummary` also gains
`open_advisory` beside `open_blocking`, and the history's findings carry a `blocking` flag, so
no view compares severities or counts findings.

The verdict is a Rust function over runs, findings and configuration, so it cannot be one
correlated subquery in `TASK_SUMMARY_SELECT`. A SQL copy of 021's rule was refused for the
reason the 2026-08-28 amendment gives for the strategy chain: a second implementation drifts
from the first, and the drift would show as a card saying one thing and its panel another.
`list_tasks` therefore runs `TASK_SUMMARY_SELECT` as before and then reads, in a fixed number of
statements whatever the number of cards and repositories (`review_loop::board::summaries`):
the global configuration; the listed tasks' own; their repositories'; their runs rows; their
findings. The last four are keyed by id through one JSON array bound once (`json_each`), so
the statement text never changes and never meets SQLite's cap on bound parameters. The runs and
findings cover **every** listed task, not only those with loop rows, because 021's
`not_reviewed` case is a succeeded implementation with no loop rows after it, read against
today's configuration. The per-task resolution and verdict then run in memory.

This is deliberately **not** `apply_effective_strategy`'s shape: that function calls
`defaults_for_repository` once per distinct repository, which is N+1 over repositories, and
the amendment above only called it cheap because settings reads are cheap. The entry's
argument was always against fifty reads per board read, not against a second statement.

The entry now binds 037 as well.

### Amendment, 2026-10-10 — no worktree path on the summary or the detail (task 066)

`TaskSummary = every column of tasks` above now means every column a board DTO may carry:
`worktree_path` is not one of them. `Task`, `TaskSummary` and `TaskDetail` lose
`worktreePath`, and the MCP `TaskView` loses `worktree_path`. `TASK_SUMMARY_SELECT` names
its columns instead of `t.*`, so no board read carries the retired column even unread.

Where a task's worktree is belongs to the machine that made it (ADR-0028 point 2), and the
window reads it from the local `list_local_worktrees` command, one read shared by every
card and joined by task id. The cost argument above is unchanged: a board read is still one
query, and the worktree read is one more invoke per window, not one per card.

The entry now binds 066 as well.

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

### Amendment, 2026-10-10 — the guard reads the branch, not the worktree (task 066)

A task's repository is reassignable **only while it has no recorded `branch` and no runs**.
The refusal is, exactly:

`cannot move "<title>" to another repository: it already has a branch, <branch>, in <repository name>`

**Why.** A board rule cannot see a path: since task 066 the worktree is the runner's
record, not a board column. The branch is the board-side fact ADR-0005 ties to one
repository, and the same act that recorded it created the worktree, so it is what the
guard reads. This is a real narrowing: a task whose worktree was removed with its branch
kept, and which has no runs, used to be reassignable, and is not any more. That is
intended: its branch is still in the old repository.

`RepositorySelector.tsx` computes its disabled reason from `task.branch` in the same
words. The run-count refusal is unchanged.

The entry now binds 066 as well.

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

   *Pointer, 2026-10-05 (task 035).* The document's key is `"rimaia-run"`, not `"rimaia"`:
   the run-scoped handle is served under its own server name ([D30](#d30--the-run-scoped-handle-is-served-as-rimaia-run) point 1).
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

### Amendment, 2026-10-10 — the slot is `LocalSlot`, and the loop takes it after the claim (task 042)

Two changes, both about which half owns what.

- **The rename.** `Lease` is `LocalSlot`, `LeaseOwner` is `SlotOwner` and `LeaseRefused`
  is `SlotRefused`, everywhere: `scheduler::inflight`, the manual starter, the planner, the
  MCP server's Plan now, the runner loop, the shell and every test. The method names
  (`acquire`, `acquire_unbounded`, `cancel_owned_by`, `releases`) and `PlannerClaim`'s shape
  are unchanged. From here on "lease" means only the board's (`LeaseRef`, `LeasePurpose`,
  task 043's `runner_leases`), because from 043 the loop holds a slot and a lease at once
  and a diff that confused the two would compile. Point 2's "A `Lease` is RAII" now reads
  "a `LocalSlot` is RAII"; point 4's `LeaseOwner::Queue` is `SlotOwner::Queue`.
- **The runner loop takes its slot after the claim** (`rimaia_runner::queue`). The queue's
  old header said the slot came *before* the claim "so a Pause pressed mid-claim has
  something to act on". Under `ClaimTarget::Next` the board picks the task, so the loop
  cannot take a slot for it until the claim returns. What closed the mid-claim window was
  never the slot: it is the re-check of the switch and the shutdown signal after every
  await. A Stop that lands during the probe writes `paused` and cancels nothing, because
  nothing is held yet, and the re-check after the probe ends the pass before any claim. One
  that lands during the claim is found by the re-check after it, and the claim is released.
  The three `…_mid_claim_…` tests now hold the version probe instead of a slot and assert
  what they always asserted. A slot refused after a won claim falls back to
  `acquire_unbounded` for a capacity refusal (a Run now took the slot after the view was
  built; the board decided on the capacity the runner reported) and releases for
  `AlreadyInFlight` or the ceiling.

  **Manual starts keep task 036's order** (`preview`, slot, `claim(Run)`), because a person
  named the task.

**Binds.** 042, 043, 052, 053.

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

### Amendment, 2026-09-04 — a warning can be put down, and the refusal never reads it (task 027)

Point 3 above draws the `warn`/`fail` line and stops at "only `fail` blocks". Task 027 adds the
thing that line implies and this entry did not say: **a `warn` can be dismissed, and a `fail`
cannot.**

- **Dismissal is per row, keyed on `check` + `repository` + `detail`** — not on the check. The
  same check about a different repository is a different warning, and a changed `detail` is a
  sentence the user has not read. Stored as JSON in the `doctor_dismissals` settings key
  (D3, D4 — no migration), read through a typed accessor beside `onboarding_dismissed` and
  tolerant of a hand-edited row the way `run_environment` is.
- **`DoctorReport` marks; it never drops.** `CheckResult::dismissed` is set by
  `DoctorReport::new`, and `CheckResult::answered_by` only ever marks a `CheckStatus::Warn` — so
  the "a `fail` is not dismissible" rule is enforced on every *read* rather than at the two
  write paths, and a row that was a warning yesterday and is a failure today is not silently
  silenced by yesterday's answer. `DoctorReport` also carries the whole stored set, including
  dismissals that match no current row, so nothing stored is invisible.
- **`is_blocking`, `blocking` and `blocking_summary` do not read `dismissed`, now or ever.**
  Point 1's refusal on `QueueHandle::start`/`resume` is unchanged, byte for byte, and
  `crates/core/tests/doctor.rs::dismissing_every_row_still_refuses_to_start_the_queue_and_writes_no_queue_state`
  dismisses every row of a blocking report and asserts the same error and the same absent
  `queue_state`. That test is the point of this amendment: it fails loudly if a later change
  ever wires the dismissal set into the gate.
- **The banner collapses a `fail` instead of dismissing it.** The rows fold away, the headline
  and the blocking count stay. Collapsing is component state and does not outlive the window.
- **Both tools are `RunAccess::Refused`** (ADR-0021 point 4), with the sharpest edge on that
  clause: a run that could dismiss a doctor warning could silence the report on the environment
  it is itself running in.

**One correction this task forced.** `Check`'s `rename_all = "snake_case"` produced
`git_hub_cli` for `GitHubCli`, where `Check::as_str()`, `CheckResultView` and `src/types.ts` all
say `github_cli`. It cost nothing while a check was only ever *written* to the wire; it became a
defect the moment a check became the key half of a stored value that has to compare equal across
both spellings. The variant now carries an explicit `#[serde(rename = "github_cli")]`, and
`every_check_serializes_with_the_spelling_its_accessor_returns` pins the agreement for all eight.

**Binds.** 018, 027.

### Amendment, 2026-10-10 — the loop's probe is memoised, and the gate's tests moved (task 042)

Point 2 keeps the doctor out of the loop and left one per-step check, `probe_cli`. Before
task 042 the loop knew from its own plan read whether anything was startable and probed
only before a non-empty batch. Under `ClaimTarget::Next` the board decides that, and the
loop cannot know before it claims, so a probe per pass would be the per-change spawn point
2 argues against. So:

- **The probe runs at most once per `DEADLINE_CAP` of clock time on a pass with a free
  slot.** The loop keeps the last answer, success or failure, with the instant on the
  injected clock it was taken, and reuses it until the cap has passed. `QueueHandle::start`,
  `resume` and a schedule's fire forget it, because the doctor they run has just asked the
  same question. With free capacity, a board change costs at most one `--version` spawn a
  minute whether or not anything is ready. A pass that is switched off, held by a usage
  limit or full spawns nothing and asks the board for nothing.
- **What that costs is bounded and stated.** A `claude` removed while the queue runs is
  noticed within the same minute, and one claimed task can fail at spawn before it is:
  `run_task` probes again before it spawns, and the failure is that run's.
- Point 1's refusal moved crates with the loop, unchanged. Its three tests,
  `a_blocking_report_refuses_to_start_the_queue_and_writes_no_queue_state`,
  `dismissing_every_row_still_refuses_to_start_the_queue_and_writes_no_queue_state` (027's
  amendment above names it) and `a_healthy_installation_starts_the_queue_even_with_warnings_outstanding`,
  are now in `crates/runner/tests/queue_preflight.rs`, with the same names and
  assertions.

Tests: `a_queue_that_cannot_start_anything_spawns_no_probe`,
`an_idle_board_costs_at_most_one_probe_per_deadline_cap` and
`a_missing_binary_is_found_before_the_queue_claims_anything`, in
`crates/runner/tests/queue.rs`.

**Binds.** 042, 053, 058.

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

### Amendment, 2026-10-04 — the budget boundary reads run kinds, see D29

Point 7's retry-budget boundary is now read over the run kinds D29 names. D29 states the rule.

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
---

## D25 — Task 022's cross-cutting choices

**Question.** ADR-0020 decides that a repository may carry its own forge token and that the
token lives in the OS keychain, and stops there. Making an unattended run *use* one needs a
dozen smaller answers, several of which widen types other tasks share or put a rule in a place
no ADR reaches.

**Decision.** Seven, taken together by task 022.

1. **The row says whether a credential exists; the keychain says whether it is still there, and
   the two disagreeing is a refusal.** `repositories.credential_added_at` is what
   `repo::has_credential` reads, and `runner::process::repository_credentials` refuses the run —
   naming the repository, and saying it did not fall back — when the row claims a token the
   keychain does not hold. **Never ask the keychain "is anything there" instead.** A locked or
   unreachable keychain answers "no", and a spawn path that read that as "this repository has no
   credential" would run with the operator's whole GitHub account: the exact failure ADR-0020
   point 5 exists to prevent, and one that is invisible in every artefact the run leaves behind.

2. **Everything is an environment variable at spawn, and `Invocation::args` does not move.**
   `ps` is world-readable on Unix and the Windows equivalents are no better, so no token is ever
   an argument; nothing is written to disk, so nothing is left in a worktree the run could stage.
   Keeping the credential out of `args` also preserves ADR-0012's byte-for-byte argv contract:
   the flags this product is most dangerous to get wrong stay assertable without a process, and
   the credential is asserted separately as an environment diff.

3. **HTTPS auth is `GIT_CONFIG_COUNT`/`KEY_0`/`VALUE_0`, never a `credential.helper`.** A helper
   snippet is `sh -c` by another name and does not exist on Windows. The operator's own
   `GIT_CONFIG_*` are **removed before** ours is added — appending to their count is an
   off-by-one that silently drops one side or the other, and which side depends on numbering
   nobody can see. `GIT_TERMINAL_PROMPT=0` and `GCM_INTERACTIVE=Never` ride along so a bad
   credential fails immediately rather than blocking on a prompt nobody will answer at 2am.

4. **A repository without a credential is byte-identical to before this feature existed.**
   `ChildEnvironment::ambient` adds nothing and removes nothing, and a test asserts it. That is
   what makes adopting this safe one repository at a time, and it is why the ambient-forge strip
   is conditional where the `CLAUDE_*` strip is not — they are two different rules, and this
   entry is the third one `runner::process`'s header now names.

5. **Redaction happens before write, over exactly the values that were injected.** The
   transcript is a file that outlives the process and that task 015 offers to open; redacting on
   read would leave the secret in the only copy that matters. It is deliberately **not** a secret
   scanner — no regex over `ghp_[A-Za-z]+`, which would miss a fine-grained token, miss an
   enterprise one, and eventually redact a legitimate string. The set worth hiding is known
   exactly: it is what Rimaia itself put in the child's environment, in both the raw and the
   base64 form, because a run that echoes its environment prints one and a `git config --list`
   prints the other.

6. **Two commands get no MCP tool, and it is a third kind of exception.** ADR-0021 point 1 makes
   a Tauri command without a tool a defect; point 5 names `delete_task`'s destructiveness
   exception, and D20.6's 2026-09-04 amendment names task 026's desktop-referent one.
   `set_repository_credential` and `remove_repository_credential` are neither: **the argument is
   a live forge token**, and putting one on a loopback protocol into a process's argv is a
   widening nobody asked for. `get_repository_credential_status` *does* get a tool,
   `RunAccess::Refused` — it carries the login, the label and the date and never the secret, and
   an operator's agent that can see a repository is configured while its keychain item is gone
   can explain a failed push, which is the half of the problem it can help with.

7. **Verification has three outcomes, and the third is not a failure.** `gh api user` and
   `gh api repos/{owner}/{repo}`, with only that token in the child's environment. Verified
   stores the resolved login. Rejected **refuses the save** — ADR-0020's "refused at paste
   time", because a token that cannot open a pull request is a run that fails at 2am having
   already done the work. `gh` absent stores it *unverified*: a missing local tool says nothing
   about the token, and refusing would make the feature unusable on a machine with git but not
   `gh`, which is a machine that can still clone and push.

**Why.** Every one of these is a place two agents would have answered differently and a reviewer
could not have said which was right, which is this file's own test for what belongs in it. (1)
and (4) are the two halves of "fail closed without changing anything for anyone who has not
opted in", and both are silent failures if got wrong. (2), (3) and (5) are each a place where
the obvious implementation is the one that works on macOS and needs a rethink on Windows —
which task 022's own file calls not meeting its contract. (6) is a deliberate hole in a parity
rule, and ADR-0021 is explicit that the way to have one is to argue it, not to leave it. (7) is
the difference between a feature people use and one they route around.

**What this does not claim.** ADR-0020 says it and it is worth repeating wherever this is read:
a `bypassPermissions` run can read its own environment, so all of the above bounds what a stolen
token is **worth**, not whether it can be stolen. The UI's guidance toward a fine-grained,
single-repository token is doing as much work as the keychain is.

See also [ADR-0020](adr/0020-per-repository-git-credentials.md), ADR-0012 (the posture this bounds),
ADR-0002 (why every choice above is the cross-platform one), and
[D4](#d4--migration-file-numbering)'s and [D6](#d6--pre-approved-npm-dependencies)'s 2026-09-04
amendments (the migration, and the two Cargo dependencies).

**Binds.** 022, and every later task that touches the spawn environment.

---

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
branch is left alone), [ADR-0021](adr/0021-mcp-first-capability-parity.md) points 4 and 5 (the
scope decision and the destructiveness exception), and
[ADR-0022](adr/0022-what-a-run-is-remembered-by.md) part 2 (rows survive pruning).

**Binds.** 016, 024.

### Amendment, 2026-09-04 — two more commands with no tool, on different ground (task 026)

Point 6 above records a hole in ADR-0021's parity rule and argues it from *destructiveness*.
Task 026 adds two commands to the no-tool list — `list_open_in_targets` and
`open_task_worktree_in` — and neither is destructive at all. They belong here because point 6
is where this repository keeps the list, but the argument is a different one, and it is the
one `reveal_task_worktree` has stated in `src-tauri/src/commands/worktree.rs` since task 007:

**An MCP client is a protocol, not a desktop.** "Open this directory in VS Code" has no
referent for a caller that has no screen, no window server and no user sitting in front of
one. This is not a capability being withheld from agents on grounds of trust — it is a
capability an agent has nothing to do with. `reveal_task_worktree` was never given a tool for
exactly this reason and was never recorded as an exception; it is recorded now, with these
two, so ADR-0021 point 1 ("a Tauri command without an MCP tool is a defect") stays literally
true rather than true-with-an-unwritten-asterisk.

Nothing else about the parity rule moves. The *detection* these commands sit on top of is
`rimaia_core::openers`, a function over injected inputs like `doctor::checks`, so the rule is
in core even though only one door reaches it.

**Binds.** 016, 024, 026.

### Amendment, 2026-09-15 — archiving reaches this entry twice (task 030)

ADR-0025 adds a second trigger for worktree removal and a first trigger for running
something that is not `git`, `claude` or `kill`. Two of this entry's points are touched,
and neither changes — they are restated because a reader meeting task 030's code would
otherwise read it as a deviation.

**Point 5 still holds, and now holds of two things.** Archiving deletes no `runs` row, no
transcript and no link. It is the opposite of `delete_task` in exactly the dimension point 5
carves out: the cascade is the thing being avoided, not the thing being reached for.
ADR-0025 point 2 is where that is argued at product scale.

**Point 1's four guards are reached intact by one of the two cleanup modes and bypassed
entirely by the other**, and that asymmetry is the decision, not an oversight. The
`remove_worktree` preset calls `remove_worktree` with `RemovalAuthorization::default()` —
the same posture `auto_remove_on_done` uses, for the same reason given in point 3. A
configured **script** is not routed through this module at all: it is handed the task's
paths and may do as it likes with them, including deleting a worktree with uncommitted
changes that `ensure_committed` would have refused. There is no way to have it both ways.
A script that Rimaia guarded would be a script that could not do the teardown it was
written for, and a script Rimaia re-ran cleanup after would be Rimaia removing a directory
its owner had already removed. The obligation this creates is on the *copy*, not the code:
the Settings pane must say that a script gives up the guards, in those words.

Point 6's no-tool list does **not** grow. ADR-0025 point 8 argues archiving onto the tool
surface — it is reversible, which is the property point 6's list is drawn along — while
keeping it off the run-scoped half. `set_repository_on_archive` is refused for a run on
ADR-0021 point 4's "reconfigures the installation" clause, which is the same ground
`set_worktree_auto_cleanup` already stands on.

**Binds.** 030, in addition to 016, 024 and 026.

## D26 — Task 030's cross-cutting choices

**Question.** ADR-0025 fixes what archiving means, what it preserves, what it refuses and
what a repository may configure. Six things it deliberately leaves at implementation scale
are still places two agents would answer differently, and four of them are only visible
from outside the module that owns them.

**Decision.** Six, taken together by task 030:

1. **`TaskFilter` grows a third axis, and its default is `Active`.** A new
   `ArchiveFilter` enum — `Active` (default) · `Archived` · `All` — appended to
   `TaskFilter` in `crates/core/src/tasks/types.rs`, and one more predicate on the
   `WHERE 1 = 1` chain [D12](#d12--what-the-boards-bulk-read-returns)'s projection already
   ends with.

   The default is the whole point. `tasks::list_tasks` is not only the board's read: it is
   the **scheduler's** read (`scheduler::selection::plan`), the **plan pass's** read
   (`runner::strategy::selected_tasks`) and the **MCP tool's**. A `#[derive(Default)]`
   that means "not archived" gives all four the right answer with no call site edited; a
   default of `All` would have put archived tasks back in the run queue, and the first
   place anyone would have learned that is a night run.

   It is a SQL literal appended to the string, never a bind — the value is one of three
   variants of a Rust enum and there is no user input anywhere near it, which is also
   [D5](#d5--compile-time-checked-queries-and-the-sqlx-cache)'s reason this query is
   hand-built rather than a macro in the first place.

2. **`archived_at` rides the summary as a plain column.** `TaskSummary` and `TaskDetail`
   both carry `archived_at: Option<String>`; no fifth correlated subquery, no aggregate.
   D12's cost argument — one board read is one query — is untouched, and the archive view
   needs the timestamp to sort by, which is the same read the board already does with a
   different filter rather than a second endpoint.

3. **Bulk reports, single errors — and the report carries the action's outcome.**
   `archive_tasks` returns an `ArchiveReport { archived, refused }` and never aborts on the
   first refusal; `archive_task` and `unarchive_task` return `Result`. That split is
   [D20](#d20--task-016s-cleanup-what-it-refuses-what-it-may-not-be-forced-past-and-what-it-never-deletes)
   point 2 verbatim, and the bulk half is a loop over the single half exactly as
   `worktree::cleanup::sweep` is.

   What is new is the third field. Each archived entry carries an `OnArchiveOutcome` —
   `Nothing` · `WorktreeRemoved { bytes_freed }` · `ScriptRan { exit_code, output }` ·
   `Failed { reason }` — because ADR-0025 point 6 makes the action *reported* rather than
   silent, and a caller that had to ask a second time what the cleanup did would be asking
   after the process had exited and the bytes were gone.

4. **The script's spawn contract, in one place.** `crates/core/src/archive/` owns it, not
   `runner::` and not `worktree::`:

   | | |
   | --- | --- |
   | argv | `[script_path]`, no arguments at all (ADR-0025 point 5) |
   | cwd | the **repository** root, never the worktree — the script may be deleting the worktree |
   | env added | `RIMAIA_TASK_ID`, `RIMAIA_TASK_TITLE`, `RIMAIA_REPOSITORY_PATH`, `RIMAIA_BRANCH`, `RIMAIA_WORKTREE_PATH` (empty string when the task never ran) |
   | env removed | every inherited `CLAUDE_*`, through the runner's own `strip_process_identity` |
   | process group | its own, via `set_process_group(0)`, so one signal reaches the tree |
   | stop | `TERM` to the group through `runner::process::signal_group`, then the runner's grace period, then `KILL` |
   | output | stdout and stderr captured, tail capped and carried on the outcome — no new log tree, and **not** redacted |

   `signal_group` is reused rather than reimplemented because it is already an argument
   vector (`kill -s TERM -- -<pgid>`) rather than a shell string, which is the rule this
   whole feature is most at risk of breaking.

   **The output is deliberately not redacted**, which is the one place this diverges from
   the runner and is worth stating because the runner's habit looks like the safe default.
   `credentials::redact` exists because the runner puts a token into its child's
   environment and then writes that child's output to a transcript file. Neither half holds
   here: an on-archive script is handed no credential, and its output goes to the person
   who wrote it, on their own machine, without touching disk. Redacting anyway would mean
   reading the keychain on an archive — a new failure mode, and on macOS a possible prompt
   — to scrub a value Rimaia never supplied. `ServiceContext` has no credential store to
   reach for either, and ADR-0018 fixes that struct's shape.

5. **The timeout is the first wall-clock one in this codebase, and it is scoped to this.**
   `ARCHIVE_SCRIPT_TIMEOUT`, two minutes. Nothing else here has one: `worktree::git::run`
   is bounded by git, and a run is bounded by ADR-0010's window and `MAX_TURNS` *on
   purpose* — `runner::process`'s grace period says in its own doc that it "is not a
   timeout on the run itself". An archive hook has neither bound and sits in front of a
   user waiting for a board to update, so it gets the ordinary answer. **It is injected,
   not read from a constant at the call site**, because CLAUDE.md forbids `sleep` in tests
   and "a script that never exits is killed" is one of the behaviours that has to be
   tested.

6. **The picked set becomes general, and starts being pruned.** Task 023's
   `pickedTaskIds` in `src/components/board/Board.tsx` now drives two actions, so the
   card checkbox's `aria-label` loses its `for planning` suffix.

   More importantly it gains the prune effect `selectedTaskId` has had since task 005 and
   it has never had: an id that disappears from `state` leaves the set. It was harmless
   while the set only fed a planner that resolves ids through
   `runner::strategy::selected_tasks` and refuses unknown ones with a sentence. It is not
   harmless now — a stale id is a stale *archive* target, and "archive the 3 I picked"
   reporting a refusal for a card the user deleted ten minutes ago is a bug report nobody
   can reproduce.

**Why.** (1) is the one an agent would get wrong in the direction that costs a night's
runs, and it is invisible from the module that makes the change — the scheduler never
mentions archiving anywhere. (2) and (3) are D12 and D20 reaching a module they do not
name. (4) and (5) are a subprocess contract, which is the kind of thing that ends up
half-stated across three files unless it is tabulated once; (5) is additionally a
*precedent* being set, and a precedent set silently is one the next task widens without
knowing it was scoped. (6) is a latent bug this task promotes to a real one, which is
exactly the sort of thing a reviewer finds in a diff and cannot tell from a drive-by.

See also [ADR-0025](adr/0025-archiving-a-task-and-what-it-may-clean-up.md) (all of it),
[D4](#d4--migration-file-numbering)'s 2026-09-15 amendment (the migration),
[D12](#d12--what-the-boards-bulk-read-returns) (the projection this extends) and
[D20](#d20--task-016s-cleanup-what-it-refuses-what-it-may-not-be-forced-past-and-what-it-never-deletes)'s
2026-09-15 amendment (the guards, and which mode reaches them).

**Binds.** 030.

## D27 — Task 031's cross-cutting choices

**Question.** Task 031 draws a provider seam through `runner/`. Six decisions sit under it
that no ADR covers and that task 032 would otherwise have to re-derive: where the provider
module lives, how a provider is held, what happens to `Invocation`, who shapes the MCP
document, whose identity variables are stripped, and where a second provider's fixtures go.

**Decision.**

1. **The provider lives at `crates/core/src/runner/provider/`, not at crate root.** Every
   type the trait names — `RunIntent`, `RunEvent`, `Termination`, `ExitClass` — already lives
   under `runner`; a top-level module would need `runner → provider` *and* `provider →
   runner`.
2. **Providers are zero-sized unit structs held as `Arc<dyn AgentProvider>` on
   `RunnerConfig`.** `RunnerConfig` is `Clone + Debug` and is held in `AppState`,
   `scheduler::queue::Shared`, `runner::strategy`'s call chain and the doctor's environment;
   a type parameter would propagate into all of them. `Arc<T: ?Sized>` keeps both derives
   where `Box<dyn>` would not keep `Clone`. Zero-sized means `Debug` can never leak a token.
3. **`Invocation` is renamed `RunIntent` and loses its two Claude-shaped fields.**
   `disallowed_tools: Vec<String>` becomes `forbidden: Vec<ForbiddenOperation>`, and
   `mcp_config: Option<String>` becomes `rimaia_handle: Option<RimaiaHandle>`. The other
   nine fields are neutral concepts and keep their names.
4. **`RunHandles` mints the scoped URL; the provider shapes the document.**
   `mcp_config_json` is removed from `crates/core/src/mcp/scope.rs` and replaced by
   `endpoint_for(&RunGrant) -> Option<String>`. The `{"mcpServers":…}` shape becomes the
   Claude provider's private business.
5. **Identity-variable stripping takes the union over every registered provider, not the
   active one's.** Rimaia is developed from inside a Claude Code session *and* may be
   spawning something else; the converse arrives the moment anyone drives Rimaia from
   another agent.
6. **The second provider's fixtures live in their own directory with their own harness.**
   `crates/core/tests/fixtures/cli/` stays Claude-only and `crates/core/tests/harness.rs`'s
   existing assertions are not loosened.

**Why.** (1) and (2) are about what a refactor is allowed to cost: both alternatives compile,
and both spread the provider into modules that have no business knowing one exists — which is
the thing ADR-0026 is trying to prevent, arriving as a type parameter instead of a `match`.
(3) is where the mismatch actually is: those two fields are the whole of what a second
provider cannot say, so leaving either as a `String` would leave the seam decorative. (4)
splits a function that does two jobs — minting a scoped URL is Rimaia's, and spelling it as a
config document is the provider's — and it is the one change that makes a flagless provider
expressible at all. (5) is a rule about a failure nobody sees: a child that believes it is a
nested session of its parent writes the wrong session id into a transcript and nothing else
goes wrong until someone reads it. (6) protects a claim rather than a behaviour — seven tests
iterate the recorded corpus asserting Claude properties, and a foreign file dropped in beside
them turns every one of those into an exclusion list.

See also [ADR-0026](adr/0026-a-provider-seam-for-the-agent-cli.md) (the seam itself),
[ADR-0012](adr/0012-permission-posture-for-unattended-runs.md) point 3 (the blocklist a
provider may not silently drop), and D17.4 (where the scoped handle comes from).

**Binds.** 031, 032.

---

## D28 — Team mode's schema: one rebuild, additive files around it, and the DDL of each

**Question.** ADRs 0028–0036 add teams, users, runners, leases, consent, sign-in,
invitations, remote-keyed repositories and uploaded transcripts to a schema whose three
most-referenced tables have no owner and carry `NOT NULL` local paths: `repositories.path`,
`repositories.worktree_root` and `runs.log_path`. SQLite's `ALTER TABLE` adds nullable
columns and little else. Relaxing a `NOT NULL`, adding a `NOT NULL` foreign key and adding a
table-level constraint each need the rename-copy-drop rebuild that `db::models` warns about,
and rebuilding a table that other tables reference is only safe with `foreign_keys` off. The
approved plan does that with sqlx's `-- no-transaction` marker and a `PRAGMA foreign_keys =
OFF` inside the file. Eleven tasks then write thirteen files across two stores, and several
of those files add a column that a later milestone reads. This entry answers four things:
which file carries which column, what each file says, how the one rebuild is applied, and
how it keeps every row.

**Decision.** Seven parts. Parts 1 and 2 replace the plan's mechanism for the rebuild and
keep what it was for.

1. **On SQLite, sqlx 0.8.6 ignores `-- no-transaction`, so `db::migrate` turns foreign keys
   off around the migrator. The file never does.** Three facts, read from the sources this
   workspace compiles and measured with the sqlite3 CLI (3.51; the crate bundles 3.46.0
   through `libsqlite3-sys` 0.30.1, and every statement below exists in both):

   - sqlx parses the marker into `Migration::no_tx` (sqlx-core 0.8.6,
     `src/migrate/source.rs:127`), and only the Postgres driver reads it (sqlx-postgres
     0.8.6, `src/migrate.rs:214`). sqlx-sqlite 0.8.6's `apply` (`src/migrate.rs:131–162`)
     always opens a transaction. It runs the whole file inside it, then the
     `_sqlx_migrations` insert.
   - `PRAGMA foreign_keys` does nothing inside a transaction. After `BEGIN; PRAGMA
     foreign_keys = OFF;` it still reads 1.
   - With enforcement on, `DROP TABLE tasks` runs SQLite's implicit `DELETE FROM tasks`,
     and that fires every `ON DELETE CASCADE`. Measured: after rebuilding `tasks` this way,
     `runs` is empty, and so is `task_links`. Only a `task_dependencies` edge can stop it,
     through its `RESTRICT`, and only if SQLite happens to delete the depended-on task before
     its dependent.

   So the plan's mechanism would have deleted every run on every existing install and then
   reported success. Putting `BEGIN … COMMIT` in the file does not help either. Its `COMMIT`
   ends sqlx's transaction early, so sqlx's bookkeeping insert runs outside any transaction
   and sqlx's own commit then fails. The file's schema changes are already committed, but
   sqlx records the migration as not applied. Every later launch re-runs it, and it fails at
   its first `CREATE TABLE`. What replaces it:

   - **No file in either set begins with `-- no-transaction`.** sqlx checks only the start
     of the file (`starts_with`), so even a first line that merely mentions the marker opts
     the file out. Each file's first line is its title.
   - **`db::migrate` keeps its signature but gets a new body** (task 038). The body goes
     through one helper, `db::apply_migrations(&Migrator, &SqlitePool)`, which the runner
     store also calls (task 040). The helper:
     1. acquires one connection from the pool;
     2. runs `PRAGMA foreign_keys = OFF` on it, outside any transaction, and confirms it
        reads 0;
     3. runs `migrator.run(&mut *conn)`. Each file still runs inside sqlx's own
        transaction, and that transaction is what makes the rebuild atomic;
     4. if the run applied anything, checks that `PRAGMA foreign_key_check` returns no row.
        Any row is an error naming its table, rowid and parent, and startup fails loudly
        (D11);
     5. runs `PRAGMA foreign_keys = ON` and confirms it reads 1;
     6. on every error path, calls `PoolConnection::close_on_drop()` first, so a connection
        with enforcement off never goes back to the pool.

     This is SQLite's documented twelve-step table-rebuild procedure, with sqlx supplying
     the transaction. It also works for the one-connection in-memory pool that
     `testing::db::test_pool` builds, where a second connection would be a separate
     database. That pool's `foreign_keys_are_enforced_as_they_are_in_production` test
     already asserts step 5. Step 4 runs only when something was applied, so a board that a
     CLI writer left inconsistent is not suddenly refused on an ordinary launch.
   - **Nothing cascades inside any migration.** Enforcement is off for every file, so a
     later file that deletes rows must delete their children itself. Step 4 rejects a result
     that relied on a cascade.
   - **038 guards itself against being applied with enforcement on.** That happens if it is
     applied through `cargo sqlx migrate run`, whose `SqliteConnectOptions` default turns
     foreign keys on (sqlx-sqlite 0.8.6, `src/options/mod.rs:185`), or through the sqlite3
     CLI. The file's first statements fail, rolling the whole file back, unless
     `foreign_keys` reads 0 or `tasks` is empty. An empty `tasks` has nothing to cascade,
     which is why CLAUDE.md's prepare loop can apply the file to `target/sqlx-prepare.db`
     unchanged. A plain SQL file can only fail on a condition through a `CHECK`, and a named
     one lets the constraint name carry the error message SQLite reports.

2. **`20261003120000_team_mode_board.sql` (task 038) is the only rebuild, and it keeps every
   row.** It rebuilds `repositories`, `tasks` and `runs`, once, in that order. How the rows
   survive:

   - **A reference that already dangles stops it before it starts.** ADR-0003 counts the
     sqlite3 CLI as a writer, and the CLI runs with enforcement off, so an existing install
     can hold a task whose repository is gone. The rebuild does not guess which team such a
     row belongs to. A second guard counts the rows of `pragma_foreign_key_check`; if there
     are any, the file rolls back, and the message names the pragma to run.
   - **New table, copy, drop the old, rename the new.** Never rename the old table first.
     With `legacy_alter_table` off, which is SQLite's default, renaming `tasks` to
     `tasks_old` rewrites the `REFERENCES tasks` clause in `runs`, `task_links`,
     `task_dependencies`, `review_findings` and `review_bundles` to name `tasks_old`.
     Dropping `tasks_old` then leaves all of them pointing at nothing. Renaming `tasks_new`
     to `tasks` rewrites nothing, and the children's `REFERENCES tasks` resolve to the new
     table. Measured, including `ON DELETE CASCADE` firing against the new table.
   - **Explicit column lists on both sides of every copy, never `*`.** `ADD COLUMN`
     appended columns in migration order, and the new tables declare them in a different
     order.
   - **Every column the three tables have when 038 lands is redeclared** with its type,
     default and `CHECK`. The only exceptions are the three relaxed `NOT NULL`s. Every value
     that was legal before is legal after, so no row can fail the copy on a constraint. This
     includes the columns from 033 and 035. If either file differs from part 6, 038 follows
     the file.
   - **Retired columns stay.** ADR-0028 point 5 splits the change across two releases: this
     one copies and relaxes, task 065 drops, and the release in between is the rollback
     window.
   - **Every index on the three tables is recreated with its exact definition**, because
     `DROP TABLE` drops them. Today these are `idx_tasks_board`, `idx_tasks_run_state`,
     `idx_runs_task_attempt` and 035's `idx_runs_task_kind`.
   - **A third guard at the end** repeats the foreign-key count inside the same
     transaction. If the copy left anything dangling, the whole file rolls back, and the
     next launch retries against the untouched board.
   - **A test checks row-for-row equality** (part 7).

   038 makes three columns optional for every `query!` that reads them:
   `repositories.path`, `repositories.worktree_root` and `runs.log_path` now infer
   `Option`. 038 updates every reader and regenerates `.sqlx`. A reader either handles
   `None`, or, if it is solo-only until task 066, uses a `"path!"` override with a comment
   naming 066.

3. **038 adopts the solo identity when there is a board to adopt. Otherwise the app creates
   it at first launch.** ADR-0029 point 2 says the solo team and user are "created by
   migration with ordinary generated UUIDv4 ids". The hosted server applies the same
   embedded board migrations (ADR-0028 point 1). A file that always created them would start
   every hosted instance with a team nobody belongs to and a user nobody can sign in as. The
   app cannot create them *before* 038 either: `Migrator::run` applies every pending file in
   one call, and 038's `NOT NULL team_id` copy needs the team row in the same transaction.
   So:

   - **038 adopts** exactly when the board holds something a team must own: `repositories`
     has a row, or `settings` holds anything other than the one row that
     `20260820120100_seed_settings.sql` wrote, compared byte for byte. A fresh board, whether
     a server's, the test harness's or a new install's, holds exactly that one row. **No
     migration before 038 may insert a `settings` row**, or this test can no longer tell a
     fresh board from an existing one. A default belongs in its accessor, as
     `run_environment`'s already does. Adopting writes one user, that user's personal team,
     an owner membership, one runner and the `solo_identity` row. The ids are generated in
     SQL, in the shape `Uuid::new_v4().to_string()` produces (D10: lowercase, hyphenated,
     version nibble 4, variant 8–b):

     ```sql
     lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4'
       || substr(lower(hex(randomblob(2))), 2) || '-'
       || substr('89ab', 1 + (random() & 3), 1) || substr(lower(hex(randomblob(2))), 2)
       || '-' || lower(hex(randomblob(6)))
     ```

     It uses `random() & 3` rather than `abs(random()) % 4`, because `abs()` of SQLite's
     smallest integer raises an overflow error. Part 6 abbreviates this expression as
     `<uuid_v4>`. The file writes it out in full at each of its three uses, because plain
     SQL cannot define functions.
   - **Otherwise the app creates it**, through `identity::ensure_solo` in `rimaia-core`. The
     shell's setup calls it in solo mode, after `db::migrate` and before any
     `ServiceContext` exists (038 makes the scope a required field). If a `solo_identity`
     row exists, it loads it. If there is none and `teams` is empty, it creates the same
     five rows with `Uuid::new_v4()` in one transaction. It does this through the
     team-creation service that 038 writes and a server sign-up reuses (task 047), which
     seeds `base_instructions` from `DEFAULT_BASE_INSTRUCTIONS`. If there is no row but
     `teams` is not empty, the file belongs to a server, and solo refuses to start (D11).
     `testing::context` also goes through `ensure_solo`, so service tests exercise the app's
     path.
   - **Both paths write the same placeholder values**, and nothing in solo displays them:
     user `login` `solo`, team `name` `Personal`, runner `label` `This computer`, runner
     `provider` `claude-code` (`ProviderId::ClaudeCode.as_str()`).

   This refines ADR-0029 point 2 rather than departing from it. The ids are generated and
   recorded as the installation's solo identity, and a migration creates them on every
   install that has a board to adopt. ADR-0029 gains a one-line pointer to this entry, in
   the same commit.

4. **`settings` is split three ways, following ADR-0028 point 2's table, and nothing is
   deleted.** Under the adopted identity:

   | Placement | Keys | Goes to |
   | --- | --- | --- |
   | User | `subscription_monthly_usd`, `review_digest_seen_through` | `user_settings`, by 038 |
   | Runner | `run_environment`, `mcp_port`, `max_concurrency`, `schedule_mode`, `queue_state`, `active_run_window`, `usage_limit_pause_until`, `worktree_auto_cleanup`, `doctor_dismissals`, `onboarding_dismissed` | `runner.db`'s `runner_settings`, by task 040 |
   | Team | every other key. Today: `base_instructions`, `strategy_catalogue`, `strategy_default`, `strategy_default.<repository_id>` (D17.2), `strategy_approval`, `max_turns`, `disallowed_tools`, plus task 021's review-loop keys | `team_settings`, by 038 |

   Team placement is defined *by exclusion*, so a key nobody listed ends up in
   `team_settings` instead of being lost when task 065 drops `settings`. A task that adds a
   key before 038 lands states at its accessor which placement the key has, and 038's two
   lists are updated to match. `max_turns` and `disallowed_tools` are copied to the team
   only. The runner's stricter override starts out absent, so the effective value, which is
   the stricter of the two, stays what it is today. The copy happens once, and task 039
   moves the readers. 038 and 039 ship in the same release, so no user ever runs a build
   that has one without the other.

5. **Every other file in both sets is additive.** They contain only `CREATE TABLE`, `CREATE
   INDEX`, `ALTER TABLE … ADD COLUMN` and backfilling `UPDATE`s. Three rules follow for
   where a column may go:

   - Some columns cannot be added without a rebuild: a `NOT NULL` column without a constant
     default, a `NOT NULL` foreign key, or any table-level constraint on `repositories`,
     `tasks` or `runs`. Those go in 038, or wait for a release that plans another rebuild.
     None is planned.
   - An added foreign-key column defaults to `NULL`. SQLite refuses a non-`NULL` default on
     such a column while enforcement is on, and the prepare loop runs with it on.
   - A `CHECK` on a table that nothing references is cheap to widen later: rebuilding a
     leaf table cascades into nothing and needs none of part 1. A `CHECK` on
     `repositories`, `tasks`, `runs`, `runners` or `api_tokens` is permanent. Every such
     `CHECK` below encodes a set of values an ADR has already fixed.

   Task 065 needs no rebuild either. Every retired column can be dropped with `ALTER TABLE …
   DROP COLUMN` (SQLite 3.35 and later), because none is indexed, part of a key, or named by
   a table-level constraint. Measured for `on_archive`: its column-level `CHECK` does not
   block the drop. `schedules` and `settings` are dropped whole.

6. **The DDL.** Each file's header comment is written by its task, in the voice of the
   existing migrations. Timestamps are RFC 3339 text in the `+00:00` spelling, and ids are
   D10 strings, as the initial schema's header requires. Board set, in version order:

   **`20261001120000_run_head_and_review_bundles.sql` — task 033**

   ```sql
   -- The commit the worktree's HEAD was on when the run ended, and the commit it started
   -- from (ADR-0033 points 4, 5 and 7). NULL is "not recorded" (D18). base_ref keeps the
   -- branch name it has always held; base_sha is what that name resolved to.
   ALTER TABLE runs ADD COLUMN head_sha TEXT;
   ALTER TABLE runs ADD COLUMN base_sha TEXT;

   -- ADR-0033 point 7. A table of its own so the patch never rides a board read; the PR URL
   -- stays on runs.pr_url and both commits on runs. files and commits are JSON arrays of
   -- task 033's serde types, read and written only through them.
   CREATE TABLE review_bundles (
       run_id          TEXT NOT NULL PRIMARY KEY REFERENCES runs (id) ON DELETE CASCADE,
       files_changed   INTEGER NOT NULL,
       insertions      INTEGER NOT NULL,
       deletions       INTEGER NOT NULL,
       files           TEXT NOT NULL,
       commits         TEXT NOT NULL,
       patch           TEXT,                        -- up to the cap; NULL once pruned
       patch_bytes     INTEGER NOT NULL,            -- the whole patch, before the cap
       patch_truncated BOOLEAN NOT NULL DEFAULT 0,
       patch_pruned_at TEXT,                        -- ADR-0036 point 6
       created_at      TEXT NOT NULL
   );
   ```

   **`20261001120100_run_kinds_and_review_findings.sql` — task 035, carrying task 021's
   columns**

   ```sql
   ALTER TABLE runs ADD COLUMN kind TEXT NOT NULL DEFAULT 'implementation'
       CHECK (kind IN ('implementation', 'review', 'fix'));

   -- One attempt sequence per task across kinds: idx_runs_task_attempt stays UNIQUE on
   -- (task_id, attempt), and start_run keeps computing max(attempt) + 1. What each reader
   -- does with kind is D29's.
   CREATE INDEX idx_runs_task_kind ON runs (task_id, kind, attempt);

   -- D30 point 7's witness: a clean review is an explicit call. Set once, by
   -- record_review_findings, in the same transaction as the rows it writes, including
   -- when it writes none. NULL on every row that is not a review, and on a review that
   -- never called.
   ALTER TABLE runs ADD COLUMN findings_recorded_at TEXT;

   -- Task 021's, riding here because 021 has no file of its own. review_instructions is
   -- content (ADR-0032 point 3). review_config is a JSON ReviewConfig whose every field is
   -- optional and inherits when absent; NULL inherits all of it.
   ALTER TABLE tasks ADD COLUMN review_instructions TEXT;
   ALTER TABLE tasks ADD COLUMN review_config TEXT;
   ALTER TABLE repositories ADD COLUMN review_config TEXT;

   CREATE TABLE review_findings (
       id                 TEXT NOT NULL PRIMARY KEY,
       task_id            TEXT NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
       review_run_id      TEXT NOT NULL REFERENCES runs (id) ON DELETE CASCADE,
       ordinal            INTEGER NOT NULL,  -- the reviewer's order within one call, from 0
       severity           TEXT NOT NULL CHECK (severity IN ('critical', 'high', 'medium', 'low')),
       title              TEXT NOT NULL,
       body               TEXT NOT NULL,
       file               TEXT,     -- repository-relative; NULL for the change as a whole
       line               INTEGER,
       fingerprint        TEXT,     -- 021's key for "the same finding again"
       status             TEXT NOT NULL DEFAULT 'open'
                          CHECK (status IN ('open', 'fixed', 'rejected')),
       resolution         TEXT,     -- what the fix run did, or why it declined
       resolved_by_run_id TEXT REFERENCES runs (id) ON DELETE SET NULL,
       created_at         TEXT NOT NULL,
       resolved_at        TEXT,
       CHECK (status <> 'rejected' OR resolution IS NOT NULL)
   );
   CREATE INDEX idx_review_findings_task ON review_findings (task_id, status);
   CREATE UNIQUE INDEX idx_review_findings_review_run ON review_findings (review_run_id, ordinal);
   CREATE INDEX idx_review_findings_resolved_by ON review_findings (resolved_by_run_id);
   ```

   **`20261003120000_team_mode_board.sql` — task 038**

   ```sql
   -- 1. Guards (parts 1 and 2).
   CREATE TEMP TABLE rebuild_guard (
       ok INTEGER NOT NULL,
       CONSTRAINT "team_mode_board needs foreign_keys OFF: apply it through rimaia_core::db::migrate"
           CHECK (ok = 1)
   );
   INSERT INTO rebuild_guard (ok)
   SELECT foreign_keys = 0 OR NOT EXISTS (SELECT 1 FROM tasks) FROM pragma_foreign_keys;
   DROP TABLE rebuild_guard;

   CREATE TEMP TABLE dangling_guard (
       violations INTEGER NOT NULL,
       CONSTRAINT "team_mode_board found a dangling reference before it began: run PRAGMA foreign_key_check"
           CHECK (violations = 0)
   );
   INSERT INTO dangling_guard (violations) SELECT count(*) FROM pragma_foreign_key_check;
   DROP TABLE dangling_guard;

   -- 2. Who (ADR-0029, ADR-0030).
   CREATE TABLE users (
       id                TEXT NOT NULL PRIMARY KEY,
       identity_provider TEXT,   -- 'github' today; no CHECK, ADR-0030 point 1 expects more
       provider_subject  TEXT,   -- the provider's stable id, never the login
       login             TEXT NOT NULL,
       avatar_url        TEXT,
       created_at        TEXT NOT NULL,
       CHECK ((identity_provider IS NULL) = (provider_subject IS NULL))
   );
   CREATE UNIQUE INDEX idx_users_identity ON users (identity_provider, provider_subject)
       WHERE identity_provider IS NOT NULL;

   CREATE TABLE teams (
       id               TEXT NOT NULL PRIMARY KEY,
       name             TEXT NOT NULL,
       personal_user_id TEXT UNIQUE REFERENCES users (id) ON DELETE RESTRICT,
       created_at       TEXT NOT NULL
   );

   CREATE TABLE team_memberships (
       team_id    TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
       user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
       role       TEXT NOT NULL CHECK (role IN ('owner', 'member')),
       created_at TEXT NOT NULL,
       PRIMARY KEY (team_id, user_id)
   );
   CREATE INDEX idx_team_memberships_user ON team_memberships (user_id);

   CREATE TABLE runners (
       id           TEXT NOT NULL PRIMARY KEY,
       user_id      TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
       label        TEXT NOT NULL,
       provider     TEXT NOT NULL,  -- ProviderId::as_str(); no CHECK (ADR-0026)
       app_version  TEXT,           -- reported by heartbeat (053), read by the updater (063)
       paired_at    TEXT NOT NULL,
       last_seen_at TEXT,
       unpaired_at  TEXT            -- the row outlives unpairing; runs keep naming it
   );
   CREATE INDEX idx_runners_user ON runners (user_id);

   CREATE TABLE solo_identity (
       singleton  INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
       team_id    TEXT NOT NULL REFERENCES teams (id) ON DELETE RESTRICT,
       user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
       runner_id  TEXT NOT NULL REFERENCES runners (id) ON DELETE RESTRICT,
       created_at TEXT NOT NULL
   );

   CREATE TABLE team_settings (
       team_id TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
       key     TEXT NOT NULL,
       value   TEXT NOT NULL,
       PRIMARY KEY (team_id, key)
   );

   CREATE TABLE user_settings (
       user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
       key     TEXT NOT NULL,
       value   TEXT NOT NULL,
       PRIMARY KEY (user_id, key)
   );

   -- 3. Adoption (part 3): zero rows on a fresh board, one on an existing one.
   CREATE TEMP TABLE solo_adoption AS
   SELECT <uuid_v4> AS team_id,
          <uuid_v4> AS user_id,
          <uuid_v4> AS runner_id,
          strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00' AS adopted_at
    WHERE EXISTS (SELECT 1 FROM repositories)
       OR EXISTS (SELECT 1 FROM settings WHERE key <> 'base_instructions')
       OR NOT EXISTS (
              SELECT 1 FROM settings
               WHERE key = 'base_instructions'
                 AND value = 'Commit as you work, with focused commits and clear messages.
   Run the project''s tests and linters before you finish.
   When the work is complete, push the branch and open a pull request describing what changed and why.
   If you cannot complete the task, stop, commit what you have, and explain what is blocking you.'
          );

   INSERT INTO users (id, login, created_at)
   SELECT user_id, 'solo', adopted_at FROM solo_adoption;
   INSERT INTO teams (id, name, personal_user_id, created_at)
   SELECT team_id, 'Personal', user_id, adopted_at FROM solo_adoption;
   INSERT INTO team_memberships (team_id, user_id, role, created_at)
   SELECT team_id, user_id, 'owner', adopted_at FROM solo_adoption;
   INSERT INTO runners (id, user_id, label, provider, paired_at)
   SELECT runner_id, user_id, 'This computer', 'claude-code', adopted_at FROM solo_adoption;
   INSERT INTO solo_identity (singleton, team_id, user_id, runner_id, created_at)
   SELECT 1, team_id, user_id, runner_id, adopted_at FROM solo_adoption;

   INSERT INTO user_settings (user_id, key, value)
   SELECT a.user_id, s.key, s.value FROM settings AS s CROSS JOIN solo_adoption AS a
    WHERE s.key IN ('subscription_monthly_usd', 'review_digest_seen_through');
   INSERT INTO team_settings (team_id, key, value)
   SELECT a.team_id, s.key, s.value FROM settings AS s CROSS JOIN solo_adoption AS a
    WHERE s.key NOT IN ('subscription_monthly_usd', 'review_digest_seen_through',
                        'run_environment', 'mcp_port', 'max_concurrency', 'schedule_mode',
                        'queue_state', 'active_run_window', 'usage_limit_pause_until',
                        'worktree_auto_cleanup', 'doctor_dismissals', 'onboarding_dismissed');

   -- 4. repositories. allow_unattended_runs keeps its name and now means the team ceiling
   -- (ADR-0032 point 4); task 041 copies it into runner.db as the runner's consent. path,
   -- worktree_root, max_concurrency, credential_*, on_archive and on_archive_script are
   -- retired: read until 066, dropped by 065.
   CREATE TABLE repositories_new (
       id                    TEXT NOT NULL PRIMARY KEY,
       team_id               TEXT NOT NULL REFERENCES teams (id) ON DELETE RESTRICT,
       name                  TEXT NOT NULL,
       path                  TEXT,
       default_branch        TEXT NOT NULL,
       worktree_root         TEXT,
       allow_unattended_runs BOOLEAN NOT NULL DEFAULT 0,
       created_at            TEXT NOT NULL,
       max_concurrency       INTEGER NOT NULL DEFAULT 1,
       credential_login      TEXT,
       credential_label      TEXT,
       credential_added_at   TEXT,
       on_archive            TEXT NOT NULL DEFAULT 'none'
                             CHECK (on_archive IN ('none', 'remove_worktree', 'script')),
       on_archive_script     TEXT,
       review_config         TEXT
   );
   INSERT INTO repositories_new (
       id, team_id, name, path, default_branch, worktree_root, allow_unattended_runs,
       created_at, max_concurrency, credential_login, credential_label, credential_added_at,
       on_archive, on_archive_script, review_config)
   SELECT
       id, (SELECT team_id FROM solo_adoption), name, path, default_branch, worktree_root,
       allow_unattended_runs, created_at, max_concurrency, credential_login,
       credential_label, credential_added_at, on_archive, on_archive_script, review_config
     FROM repositories;
   DROP TABLE repositories;
   ALTER TABLE repositories_new RENAME TO repositories;
   CREATE INDEX idx_repositories_team ON repositories (team_id);
   CREATE UNIQUE INDEX idx_repositories_id_team ON repositories (id, team_id);

   -- 5. tasks. The repository reference becomes (repository_id, team_id), so the store
   -- itself refuses a task in another team than its repository (ADR-0029 point 5), and a
   -- repository changing team under its tasks. worktree_path is retired (066, 065).
   CREATE TABLE tasks_new (
       id                  TEXT NOT NULL PRIMARY KEY,
       team_id             TEXT NOT NULL REFERENCES teams (id) ON DELETE RESTRICT,
       repository_id       TEXT NOT NULL,
       title               TEXT NOT NULL,
       plan                TEXT,
       extra_instructions  TEXT,
       board_column        TEXT NOT NULL
                           CHECK (board_column IN ('not_ready', 'ready', 'in_review', 'done')),
       position            REAL NOT NULL,
       run_state           TEXT NOT NULL
                           CHECK (run_state IN ('idle', 'queued', 'running', 'blocked',
                                                'waiting_retry', 'failed', 'cancelled')),
       branch              TEXT,
       worktree_path       TEXT,
       strategy_mode       TEXT NOT NULL DEFAULT 'default'
                           CHECK (strategy_mode IN ('default', 'manual', 'planned')),
       model               TEXT,
       effort              TEXT,
       strategy_plan       TEXT,
       strategy_source     TEXT
                           CHECK (strategy_source IS NULL
                                  OR strategy_source IN ('user', 'planner')),
       strategy_updated_at TEXT,
       created_at          TEXT NOT NULL,
       updated_at          TEXT NOT NULL,
       source              TEXT NOT NULL DEFAULT 'ui' CHECK (source IN ('ui', 'mcp', 'system')),
       archived_at         TEXT,
       review_instructions TEXT,
       review_config       TEXT,
       FOREIGN KEY (repository_id, team_id) REFERENCES repositories (id, team_id)
           ON DELETE RESTRICT
   );
   INSERT INTO tasks_new (
       id, team_id, repository_id, title, plan, extra_instructions, board_column, position,
       run_state, branch, worktree_path, strategy_mode, model, effort, strategy_plan,
       strategy_source, strategy_updated_at, created_at, updated_at, source, archived_at,
       review_instructions, review_config)
   SELECT
       id, (SELECT team_id FROM solo_adoption), repository_id, title, plan,
       extra_instructions, board_column, position, run_state, branch, worktree_path,
       strategy_mode, model, effort, strategy_plan, strategy_source, strategy_updated_at,
       created_at, updated_at, source, archived_at, review_instructions, review_config
     FROM tasks;
   DROP TABLE tasks;
   ALTER TABLE tasks_new RENAME TO tasks;
   CREATE INDEX idx_tasks_board ON tasks (repository_id, board_column, position);
   CREATE INDEX idx_tasks_run_state ON tasks (run_state);
   CREATE INDEX idx_tasks_team ON tasks (team_id, board_column, position);

   -- 6. runs. log_path is relaxed and retired (ADR-0028 point 2; 056 replaces it, 065
   -- drops it). Every existing run ran on this machine, so it names the solo runner.
   CREATE TABLE runs_new (
       id                    TEXT NOT NULL PRIMARY KEY,
       task_id               TEXT NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
       attempt               INTEGER NOT NULL,
       status                TEXT NOT NULL
                             CHECK (status IN ('running', 'succeeded', 'failed',
                                               'cancelled', 'interrupted')),
       session_id            TEXT NOT NULL,
       prompt                TEXT NOT NULL,
       started_at            TEXT NOT NULL,
       ended_at              TEXT,
       exit_class            TEXT
                             CHECK (exit_class IS NULL
                                    OR exit_class IN ('success', 'usage_limit', 'transient',
                                                      'interrupted', 'fatal', 'cancelled')),
       error_message         TEXT,
       num_turns             INTEGER,
       cost_usd              REAL,
       log_path              TEXT,
       pr_url                TEXT,
       resume_after          TEXT,
       base_ref              TEXT,
       model                 TEXT,
       effort                TEXT,
       run_environment       TEXT,
       input_tokens          INTEGER,
       output_tokens         INTEGER,
       cache_read_tokens     INTEGER,
       cache_creation_tokens INTEGER,
       head_sha              TEXT,
       base_sha              TEXT,
       kind                  TEXT NOT NULL DEFAULT 'implementation'
                             CHECK (kind IN ('implementation', 'review', 'fix')),
       findings_recorded_at  TEXT,
       runner_id             TEXT REFERENCES runners (id) ON DELETE SET NULL
   );
   INSERT INTO runs_new (
       id, task_id, attempt, status, session_id, prompt, started_at, ended_at, exit_class,
       error_message, num_turns, cost_usd, log_path, pr_url, resume_after, base_ref, model,
       effort, run_environment, input_tokens, output_tokens, cache_read_tokens,
       cache_creation_tokens, head_sha, base_sha, kind, findings_recorded_at, runner_id)
   SELECT
       id, task_id, attempt, status, session_id, prompt, started_at, ended_at, exit_class,
       error_message, num_turns, cost_usd, log_path, pr_url, resume_after, base_ref, model,
       effort, run_environment, input_tokens, output_tokens, cache_read_tokens,
       cache_creation_tokens, head_sha, base_sha, kind, findings_recorded_at,
       (SELECT runner_id FROM solo_adoption)
     FROM runs;
   DROP TABLE runs;
   ALTER TABLE runs_new RENAME TO runs;
   CREATE UNIQUE INDEX idx_runs_task_attempt ON runs (task_id, attempt);
   CREATE INDEX idx_runs_task_kind ON runs (task_id, kind, attempt);
   CREATE INDEX idx_runs_runner ON runs (runner_id);

   -- 7. The copy left nothing dangling, or the whole file rolls back.
   CREATE TEMP TABLE copy_guard (
       violations INTEGER NOT NULL,
       CONSTRAINT "team_mode_board left a dangling reference: run PRAGMA foreign_key_check"
           CHECK (violations = 0)
   );
   INSERT INTO copy_guard (violations) SELECT count(*) FROM pragma_foreign_key_check;
   DROP TABLE copy_guard;
   DROP TABLE solo_adoption;
   ```

   **`20261003120100_runner_leases.sql` — task 043**

   ```sql
   -- ADR-0031 point 1. The primary key is the store's own "two machines never run one task
   -- at once". run_id is NULL for 'strategy' (the planner writes no runs row, D17.5) and,
   -- for the other purposes, until start_run writes the row. expires_at NULL is a solo
   -- lease, which never expires (ADR-0031 point 5).
   CREATE TABLE runner_leases (
       task_id     TEXT NOT NULL PRIMARY KEY REFERENCES tasks (id) ON DELETE CASCADE,
       purpose     TEXT NOT NULL
                   CHECK (purpose IN ('implementation', 'strategy', 'review', 'fix')),
       run_id      TEXT REFERENCES runs (id) ON DELETE CASCADE,
       runner_id   TEXT NOT NULL REFERENCES runners (id) ON DELETE RESTRICT,
       generation  INTEGER NOT NULL,
       acquired_at TEXT NOT NULL,
       expires_at  TEXT,
       CHECK (purpose <> 'strategy' OR run_id IS NULL)
   );
   CREATE INDEX idx_runner_leases_runner ON runner_leases (runner_id);
   CREATE INDEX idx_runner_leases_run ON runner_leases (run_id);
   CREATE INDEX idx_runner_leases_expiry ON runner_leases (expires_at)
       WHERE expires_at IS NOT NULL;

   -- Monotonic per task across leases: a claim increments it and copies it onto the new
   -- lease in the same transaction (ADR-0031 point 3's fencing).
   ALTER TABLE tasks ADD COLUMN lease_generation INTEGER NOT NULL DEFAULT 0;
   -- ADR-0031 point 4: after an expiry or a retry, only this runner may claim the task.
   ALTER TABLE tasks ADD COLUMN pinned_runner_id TEXT REFERENCES runners (id) ON DELETE SET NULL;
   CREATE INDEX idx_tasks_pinned_runner ON tasks (pinned_runner_id);
   -- ADR-0035 point 6: a hosted plan_task_strategy records a request that a runner claims
   -- with purpose 'strategy'. Written by task 060.
   ALTER TABLE tasks ADD COLUMN strategy_requested_at TEXT;
   ALTER TABLE tasks ADD COLUMN strategy_requested_by TEXT REFERENCES users (id) ON DELETE SET NULL;
   ```

   **`20261003120200_consent.sql` — task 045**

   ```sql
   -- Attribution (ADR-0030 point 8) and assignment (ADR-0032 point 1). SET NULL: a deleted
   -- account leaves "a former member", never a deleted task.
   ALTER TABLE tasks ADD COLUMN created_by TEXT REFERENCES users (id) ON DELETE SET NULL;
   ALTER TABLE tasks ADD COLUMN assignee_id TEXT REFERENCES users (id) ON DELETE SET NULL;
   ALTER TABLE tasks ADD COLUMN assigned_by TEXT REFERENCES users (id) ON DELETE SET NULL;
   CREATE INDEX idx_tasks_assignee ON tasks (assignee_id);

   -- ADR-0032 point 3's revisions, each with its author and point 6's mark. plan_revision
   -- covers plan and extra_instructions together.
   ALTER TABLE tasks ADD COLUMN plan_revision INTEGER NOT NULL DEFAULT 1;
   ALTER TABLE tasks ADD COLUMN plan_updated_by TEXT REFERENCES users (id) ON DELETE SET NULL;
   ALTER TABLE tasks ADD COLUMN plan_written_during_run BOOLEAN NOT NULL DEFAULT 0;
   ALTER TABLE tasks ADD COLUMN review_instructions_revision INTEGER NOT NULL DEFAULT 1;
   ALTER TABLE tasks ADD COLUMN review_instructions_updated_by TEXT
       REFERENCES users (id) ON DELETE SET NULL;
   ALTER TABLE tasks ADD COLUMN review_instructions_written_during_run BOOLEAN NOT NULL DEFAULT 0;

   -- The same facts per team setting; consent reads them for base_instructions and
   -- review_instructions.
   ALTER TABLE team_settings ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
   ALTER TABLE team_settings ADD COLUMN updated_by TEXT REFERENCES users (id) ON DELETE SET NULL;
   ALTER TABLE team_settings ADD COLUMN updated_at TEXT;
   ALTER TABLE team_settings ADD COLUMN written_during_run BOOLEAN NOT NULL DEFAULT 0;

   -- Everything that exists was written by the solo user, when there is one. assignee_id
   -- is deliberately not backfilled: ADR-0032 point 2's "in a personal team every task is
   -- the owner's own" is the eligibility rule's to state, not an assignment nobody made.
   UPDATE tasks
      SET created_by      = (SELECT user_id FROM solo_identity),
          plan_updated_by = (SELECT user_id FROM solo_identity),
          review_instructions_updated_by =
              CASE WHEN review_instructions IS NULL THEN NULL
                   ELSE (SELECT user_id FROM solo_identity) END;
   UPDATE team_settings
      SET updated_by = (SELECT user_id FROM solo_identity)
    WHERE team_id = (SELECT team_id FROM solo_identity);

   -- (user, content, revision). task_id is NULL exactly for the two team-wide pieces.
   -- revision is the integer revision in decimal for plan and instructions, the review
   -- run's id for review_findings, and the commit for base_commit, whose task_id is the
   -- dependency that produced it.
   CREATE TABLE acceptances (
       id          TEXT NOT NULL PRIMARY KEY,
       user_id     TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
       team_id     TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
       task_id     TEXT REFERENCES tasks (id) ON DELETE CASCADE,
       content     TEXT NOT NULL
                   CHECK (content IN ('plan', 'task_review_instructions', 'base_instructions',
                                      'review_instructions', 'review_findings', 'base_commit')),
       revision    TEXT NOT NULL,
       accepted_at TEXT NOT NULL,
       CHECK ((task_id IS NULL) = (content IN ('base_instructions', 'review_instructions')))
   );
   CREATE UNIQUE INDEX idx_acceptances_task_content
       ON acceptances (user_id, task_id, content, revision) WHERE task_id IS NOT NULL;
   CREATE UNIQUE INDEX idx_acceptances_team_content
       ON acceptances (user_id, team_id, content, revision) WHERE task_id IS NULL;
   CREATE INDEX idx_acceptances_task ON acceptances (task_id);
   CREATE INDEX idx_acceptances_team ON acceptances (team_id);

   -- The trust list: personal, per team, off unless a row exists.
   CREATE TABLE trusted_authors (
       user_id         TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
       team_id         TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
       trusted_user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
       created_at      TEXT NOT NULL,
       PRIMARY KEY (user_id, team_id, trusted_user_id),
       CHECK (user_id <> trusted_user_id)
   );
   CREATE INDEX idx_trusted_authors_team ON trusted_authors (team_id);
   CREATE INDEX idx_trusted_authors_trusted ON trusted_authors (trusted_user_id);

   -- ADR-0032 point 2, on the board so the claim can apply it.
   ALTER TABLE runners ADD COLUMN eligibility TEXT NOT NULL DEFAULT 'assigned'
       CHECK (eligibility IN ('assigned', 'assigned_then_pool'));
   CREATE TABLE runner_pool_teams (
       runner_id TEXT NOT NULL REFERENCES runners (id) ON DELETE CASCADE,
       team_id   TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
       PRIMARY KEY (runner_id, team_id)
   );
   CREATE INDEX idx_runner_pool_teams_team ON runner_pool_teams (team_id);
   ```

   **`20261003120300_identity.sql` — task 047.** The OAuth `state`, the PKCE verifier and
   the desktop's one-time code exchange (ADR-0030 point 4) are held in server memory for at
   most ten minutes and have no table. There is no CSRF column: 047 derives the token from
   the session secret.

   ```sql
   -- ADR-0030 point 2. The cookie carries a random secret; only its hex SHA-256 is stored.
   -- id is the handle the account page revokes by.
   CREATE TABLE sessions (
       id           TEXT NOT NULL PRIMARY KEY,
       user_id      TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
       secret_hash  TEXT NOT NULL UNIQUE,
       user_agent   TEXT,
       created_at   TEXT NOT NULL,
       last_used_at TEXT NOT NULL
   );
   CREATE INDEX idx_sessions_user ON sessions (user_id);

   -- ADR-0030 point 3: one table, three kinds. secret_hash is the hex SHA-256 of the whole
   -- token, prefix included. Revoking deletes the row.
   CREATE TABLE api_tokens (
       id             TEXT NOT NULL PRIMARY KEY,
       user_id        TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
       kind           TEXT NOT NULL CHECK (kind IN ('desktop', 'runner', 'personal')),
       secret_hash    TEXT NOT NULL UNIQUE,
       label          TEXT NOT NULL,
       runner_id      TEXT REFERENCES runners (id) ON DELETE CASCADE,
       created_at     TEXT NOT NULL,
       last_used_at   TEXT,
       last_used_from TEXT,
       expires_at     TEXT,
       CHECK ((kind = 'runner') = (runner_id IS NOT NULL))
   );
   CREATE INDEX idx_api_tokens_user ON api_tokens (user_id);
   CREATE INDEX idx_api_tokens_runner ON api_tokens (runner_id);

   -- ADR-0030 point 6. No rows: every team the token's user belongs to.
   CREATE TABLE api_token_teams (
       token_id TEXT NOT NULL REFERENCES api_tokens (id) ON DELETE CASCADE,
       team_id  TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
       PRIMARY KEY (token_id, team_id)
   );
   CREATE INDEX idx_api_token_teams_team ON api_token_teams (team_id);

   -- ADR-0030 point 5: single use, ten minutes, deleted when redeemed.
   CREATE TABLE pairing_codes (
       code_hash  TEXT NOT NULL PRIMARY KEY,
       user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
       created_at TEXT NOT NULL,
       expires_at TEXT NOT NULL
   );
   CREATE INDEX idx_pairing_codes_user ON pairing_codes (user_id);
   ```

   **`20261003120400_invitations.sql` — task 051**

   ```sql
   -- ADR-0029 point 4: single use, seven days, tied to no address, only the hash stored.
   -- Accepted and revoked rows are kept as the team's record of who let whom in.
   CREATE TABLE invitations (
       id          TEXT NOT NULL PRIMARY KEY,
       team_id     TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
       token_hash  TEXT NOT NULL UNIQUE,
       role        TEXT NOT NULL CHECK (role IN ('owner', 'member')),
       created_by  TEXT REFERENCES users (id) ON DELETE SET NULL,
       created_at  TEXT NOT NULL,
       expires_at  TEXT NOT NULL,
       accepted_by TEXT REFERENCES users (id) ON DELETE SET NULL,
       accepted_at TEXT,
       revoked_at  TEXT
   );
   CREATE INDEX idx_invitations_team ON invitations (team_id);
   ```

   **`20261003120500_repositories_by_remote.sql` — task 054**

   ```sql
   -- ADR-0033 point 1: 'github.com/owner/repo', normalised by one function in rimaia-core.
   -- NULL for a solo repository with no remote. Not backfilled in SQL: the solo runner
   -- reports each checkout's origin at its first launch after this file.
   ALTER TABLE repositories ADD COLUMN normalized_remote TEXT;
   CREATE UNIQUE INDEX idx_repositories_team_remote ON repositories (team_id, normalized_remote)
       WHERE normalized_remote IS NOT NULL;

   -- ADR-0033 point 2: which runners map which team repositories, as last reported, with
   -- each mapping's consent (ADR-0032 point 4) and the doctor's push check.
   CREATE TABLE runner_repositories (
       runner_id          TEXT NOT NULL REFERENCES runners (id) ON DELETE CASCADE,
       repository_id      TEXT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
       reported_at        TEXT NOT NULL,
       unattended_consent BOOLEAN NOT NULL DEFAULT 0,
       push_checked_at    TEXT,
       push_error         TEXT,
       PRIMARY KEY (runner_id, repository_id)
   );
   CREATE INDEX idx_runner_repositories_repository ON runner_repositories (repository_id);

   -- ADR-0034 point 5: the browser shows each runner's last doctor result.
   ALTER TABLE runners ADD COLUMN doctor_report TEXT;
   ALTER TABLE runners ADD COLUMN doctor_reported_at TEXT;

   -- ADR-0033 point 8: the cleanup a holding runner still owes ('archived' or 'done'), and
   -- the on-archive result it reported. No CHECK: one on tasks is permanent (part 5).
   ALTER TABLE tasks ADD COLUMN cleanup_pending TEXT;
   ALTER TABLE tasks ADD COLUMN archive_outcome TEXT;
   ```

   **`20261003120600_transcripts_and_retention.sql` — task 056.** Retention is a
   `team_settings` key (ADR-0036 point 6) and needs no DDL. A server keys each transcript it
   stores as `<team_id>/<run_id>.jsonl`, which is ADR-0036 point 7's "keyed by team and
   run". A solo board stores no copy of its own (D31 point 4), so its key names the
   runner's file in ADR-0013's layout, which is also what the backfill below writes.

   ```sql
   -- ADR-0036 point 7, and ADR-0022's marker on the server's side. transcript_key is an
   -- opaque key under the transcript store's root; runs.log_path stops being read.
   ALTER TABLE runs ADD COLUMN transcript_key TEXT;
   ALTER TABLE runs ADD COLUMN transcript_bytes INTEGER;     -- bytes the server holds
   ALTER TABLE runs ADD COLUMN transcript_complete_at TEXT;
   ALTER TABLE runs ADD COLUMN transcript_pruned_at TEXT;
   ALTER TABLE runs ADD COLUMN transcript_kept_on_runner BOOLEAN NOT NULL DEFAULT 0;

   -- Existing transcripts are ADR-0013's <data>/runs/<task-id>/<run-id>.jsonl. Keying them
   -- by that layout lets a solo store find them where they are, moving nothing.
   UPDATE runs SET transcript_key = task_id || '/' || id || '.jsonl';
   UPDATE runs SET transcript_complete_at = ended_at WHERE ended_at IS NOT NULL;
   ```

   **The runner set.** `runner.db` opens with the same pragmas as `db::connect` and migrates
   through `db::apply_migrations`. It has no foreign key into the board, because the board
   is another file or another machine. Its internal foreign keys are enforced.

   **The copies out of `rimaia.db` are Rust, not SQL.** A runner migration cannot know where
   the board file is, and a headless runner has none. Each copy is one step that runs at
   first launch in solo mode. It reads the board through `rimaia-core` functions that take
   the board's `ServiceContext` and ignore its scope, because the runner crate holds no
   query that names a board table (D33 point 2), and because after 039 no core function
   takes a bare pool. It writes `runner.db` in one transaction, which also inserts the
   step's `adoptions` row. The step runs only while that row is absent, and it never writes
   the board. The steps:

   - `settings` (040): the runner keys from part 4, plus `runner_identity` taken from
     `solo_identity`.
   - `machine_state` (041): for each repository, `path`, `worktree_root`,
     `max_concurrency`, `on_archive`, `on_archive_script`, `credential_*` and `created_at`
     go into `checkouts`, with `allow_unattended_runs` becoming `unattended_consent`. Every
     `tasks.worktree_path` goes into `worktrees`, and `schedules` is copied whole.
   - `credential_keys` (054): keychain items are re-keyed from the repository id to the
     pair (repository, runner), per ADR-0033 point 6.

   **A runner store from another board is refused.** On every solo launch, before any step
   runs, adoption compares `runner_identity.runner_id`, if the row exists, with the
   board's `solo_identity.runner_id`. If they differ, it returns `Error::invalid` and
   writes neither file. The message names both runner ids and the `runner.db` path, and
   gives the remedy: move `runner.db` aside to start this machine over, or restore the
   `rimaia.db` it was adopted against. This happens when a user deletes `rimaia.db` to
   start over and keeps `runner.db`, or copies one file between machines. Re-adopting
   silently would give one store two runners' histories once 041 puts worktrees in it.
   It is D11's answer to a file that belongs to someone else, applied to the second file,
   as part 3 applies it to a board whose `teams` is not empty. The rule binds solo
   adoption, and no launch path may work around it by rewriting the row. The only writers
   of `runner_identity` besides adoption are the deliberate, user-initiated ones: 058's
   pairing creates it, and 059's connecting and disconnecting replace and delete it.

   **`crates/runner/migrations/20261003130000_runner_store.sql` — task 040**

   ```sql
   -- Which runner this store is, reporting to which board (ADR-0030 point 5). server_url
   -- is NULL in solo. The runner token is in the keychain, never here.
   CREATE TABLE runner_identity (
       singleton  INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
       runner_id  TEXT NOT NULL,
       server_url TEXT,
       created_at TEXT NOT NULL
   );

   CREATE TABLE runner_settings (
       key   TEXT NOT NULL PRIMARY KEY,
       value TEXT NOT NULL
   );

   -- One row per completed one-time copy out of rimaia.db.
   CREATE TABLE adoptions (
       step       TEXT NOT NULL PRIMARY KEY,
       adopted_at TEXT NOT NULL
   );
   ```

   **`crates/runner/migrations/20261003130100_machine_state.sql` — task 041**

   ```sql
   -- The per-repository runner half of ADR-0028 point 2, keyed by the board's repository
   -- id (ADR-0033 point 2). unattended_consent is ADR-0032 point 4's runner consent.
   CREATE TABLE checkouts (
       repository_id       TEXT NOT NULL PRIMARY KEY,
       path                TEXT NOT NULL,
       worktree_root       TEXT NOT NULL,
       max_concurrency     INTEGER NOT NULL DEFAULT 1,
       unattended_consent  BOOLEAN NOT NULL DEFAULT 0,
       on_archive          TEXT NOT NULL DEFAULT 'none'
                           CHECK (on_archive IN ('none', 'remove_worktree', 'script')),
       on_archive_script   TEXT,
       credential_login    TEXT,
       credential_label    TEXT,
       credential_added_at TEXT,
       created_at          TEXT NOT NULL
   );

   -- ADR-0033 point 3. fenced_at is ADR-0031 point 4's fence after "run elsewhere" (task
   -- 057): the worktree is kept and never pushed from.
   CREATE TABLE worktrees (
       task_id       TEXT NOT NULL PRIMARY KEY,
       repository_id TEXT NOT NULL REFERENCES checkouts (repository_id) ON DELETE RESTRICT,
       path          TEXT NOT NULL,
       fenced_at     TEXT
   );
   CREATE INDEX idx_worktrees_repository ON worktrees (repository_id);

   -- The leases this runner holds, so startup reconciles only its own (ADR-0031 point 5)
   -- and a heartbeat can name each one as D31's LeaseRef, team included. Written from
   -- task 043.
   CREATE TABLE held_leases (
       task_id     TEXT NOT NULL PRIMARY KEY,
       team_id     TEXT NOT NULL,
       purpose     TEXT NOT NULL
                   CHECK (purpose IN ('implementation', 'strategy', 'review', 'fix')),
       run_id      TEXT,
       generation  INTEGER NOT NULL,
       acquired_at TEXT NOT NULL
   );

   -- Task 013's table, moved whole (ADR-0031 point 6).
   CREATE TABLE schedules (
       id              TEXT NOT NULL PRIMARY KEY,
       name            TEXT NOT NULL,
       mode            TEXT NOT NULL CHECK (mode IN ('sequential', 'parallel')),
       cron            TEXT,
       start_at        TEXT,
       max_concurrency INTEGER NOT NULL DEFAULT 2,
       enabled         BOOLEAN NOT NULL DEFAULT 1,
       timezone        TEXT,
       stop_at         TEXT,
       last_fired_at   TEXT,
       armed_at        TEXT
   );
   ```

   **`crates/runner/migrations/20261003130200_checkout_mapping.sql` — task 054**

   ```sql
   -- ADR-0033 point 2: what origin normalised to when the mapping was last verified. NULL
   -- for a solo repository with no remote, which stays legitimate (ADR-0033 point 1).
   ALTER TABLE checkouts ADD COLUMN normalized_remote TEXT;
   ALTER TABLE checkouts ADD COLUMN remote_verified_at TEXT;
   ```

   **`crates/runner/migrations/20261003130300_outbox.sql` — task 056**

   ```sql
   -- Lease-bound reports the server has not acknowledged, in the order they were made
   -- (ADR-0036 point 1). body is the request exactly as it will be sent. Transcript bytes
   -- are not copied in: they are read from the local file when sent.
   CREATE TABLE outbox (
       id         INTEGER NOT NULL PRIMARY KEY,
       command    TEXT NOT NULL,
       task_id    TEXT NOT NULL,
       run_id     TEXT,
       generation INTEGER NOT NULL,
       body       TEXT NOT NULL,
       created_at TEXT NOT NULL,
       attempts   INTEGER NOT NULL DEFAULT 0,
       last_error TEXT
   );
   CREATE INDEX idx_outbox_run ON outbox (run_id);

   -- The local copy of each transcript and how much of it the server holds. upload is
   -- false when the runner kept transcripts at home for that run (ADR-0036 point 5). The
   -- local file may be pruned once completed_at is set.
   CREATE TABLE transcript_uploads (
       run_id       TEXT NOT NULL PRIMARY KEY,
       task_id      TEXT NOT NULL,
       path         TEXT NOT NULL,
       upload       BOOLEAN NOT NULL,
       acked_offset INTEGER NOT NULL DEFAULT 0,
       final_offset INTEGER,
       completed_at TEXT
   );
   ```

7. **Tests that land with the files.** Each is named for a behaviour and runs against real
   files in a `TempDir`, never a mocked store.

   - `the_team_mode_rebuild_keeps_every_row` (038). It builds a file board at the pre-038
     schema by running `Migrator::new` over a `TempDir` that holds copies of every board
     file older than `20261003120000`. That is public API; the test does not touch sqlx's
     `#[doc(hidden)]` fields. It fills every table: a repository with every nullable column
     set, tasks in all four columns, an archived task, a dependency edge, links, runs with
     every capture column set, a bundle, findings, a schedule, and every settings key from
     part 4, including a `strategy_default.<repository_id>`. It then runs `db::migrate` on
     the same file and asserts:
     - every pre-existing value is unchanged, column by column, and row counts are equal;
     - `team_id` is the adopted team and `runner_id` the adopted runner everywhere;
     - each settings key is where part 4 places it;
     - `pragma_foreign_key_check` is empty;
     - `PRAGMA foreign_keys` reads 1 on the pool's connections.
   - `a_fresh_board_adopts_nothing` (038): after `db::migrate` alone, `solo_identity`,
     `teams`, `users` and `runners` are empty. This is the server's shape.
   - `the_rebuild_refuses_to_run_over_a_cascade` (038): 038 is applied through a plain
     `Migrator::run` on a connection with enforcement on, over a board holding a task and a
     run. It fails with the guard's constraint name, and both rows are still there.
   - `a_dangling_reference_stops_the_rebuild_and_keeps_the_board` (038).
   - `ensure_solo_and_adoption_build_the_same_identity` (038): one board is built each way.
     They are equal on everything except ids and timestamps, and every id parses as a
     version-4 UUID.
   - `no_migration_opts_out_of_its_transaction` (038, extended by 040 to the runner set):
     `no_tx` is false for every embedded migration. The runner set's copy carries the same
     name as a unit test beside the private migrator in `crates/runner/src/store.rs`,
     because core cannot reach that migrator.
   - `the_runner_adopts_the_board_once` (040, extended by 041 and 054): a second launch
     copies nothing, and the board's rows are unchanged.
   - `a_runner_store_from_another_board_is_refused` (040): a `runner.db` adopted against
     one board and then opened against another fails with the refusal above, and neither
     file changes.

**Why.** There is one rebuild because it is the one operation in this schema that can
silently delete data. Part 1 measured it doing exactly that. So it happens once, in one
file, behind three guards and one row-for-row test, and every other file is shaped so it
cannot lose a row. It sits in 038 and not earlier because 038 is where `team_id` arrives. A
rebuild in 033 or 035 would have meant a second one.

The pragma moves into `db::migrate`, not into a pre-step outside sqlx, to keep one mechanism
and one bookkeeping table. Every file stays versioned, checksummed and atomic. The only
addition is the one setting SQLite refuses to change inside a transaction. The same answer
also holds for the runner store and for any future rebuild, with no special case for this
file.

Adoption and creation are split along the one line that matters. Adoption has to be SQL,
because the rows must exist inside the transaction that needs them. Creation must not be
SQL, because the same file builds every server's board. The discriminator is the seed row,
not a guess from `_sqlx_migrations`, because to a migration a fresh board and an existing
one otherwise look the same. An existing install whose `settings` still match the seed and
which has no repository loses nothing by being treated as fresh.

The composite foreign key on `tasks` goes in now because 038 is the last chance to add a
table-level constraint to `tasks`. The initial schema already argues for store-level
backstops "for a writer that is not that service", and ADR-0029's consequences put tenant
isolation on the same level as run-state transitions. The `team_id` references are
`RESTRICT`, like `repository_id`'s. Inside a cascade, SQLite checks a `RESTRICT` row by row,
in no guaranteed order. A raw `DELETE FROM teams` that cascaded into both `repositories` and
`tasks` would fail at whichever row came first. So deleting a team is an ordered service
(task 051): dependency edges, then tasks, then repositories, then the team. The store
refuses any other order.

The smaller choices each settle a question a later task would otherwise answer on its own:

- **`tasks.lease_generation`.** A generation read off the lease row would restart whenever
  the row is replaced, and a stale runner still holding generation 1 would then match again.
- **One attempt sequence across run kinds.** `idx_runs_task_attempt` stays the backstop it
  already is against two writers numbering the same attempt, and `start_run`'s `max + 1`
  does not change.
- **`review_config` is JSON.** Task 021's settings are still open, nothing filters on them
  in SQL, and after 038 a `CHECK` on `tasks` or `repositories` is permanent.
- **`acquired_at` on a lease.** This is one column beyond the list the plan fixed. "Held by
  Alice's laptop since 01:12" (ADR-0031's consequences) needs the lease's own start time,
  and a planner run has no `runs.started_at`.
- **`team_id` on a held lease.** The runner store cannot join the board, and D31's
  `LeaseRef` carries the team on every report, a heartbeat after a restart included.
- **Sign-in state stays in memory.** The OAuth `state`, the PKCE verifier and the desktop's
  code exchange live for seconds. Litestream replicates every write to object storage
  (ADR-0037 point 2), so writing them to the database would put them in backups. A server
  restart in the middle of a sign-in costs the user one click.
- **No index on the author columns.** These are the columns that become `NULL` when an
  account is deleted. A user is deleted only a handful of times in an instance's life, and a
  table scan then is cheaper than an index updated on every task write.

See also ADR-0028 (all of it), ADR-0029 point 2, ADR-0031, ADR-0032 point 3, ADR-0033,
ADR-0036, D3, D4 and the amendment below, D10, D11, D17.5, D18, D29 (what each `runs` reader
does with `kind`), D31 (the `LeaseRef` a held lease rebuilds) and D33 (the runner store's
offline cache).

**Binds.** 021 (its columns ride in 035), 033, 035, 038, 039, 040, 041, 042, 043, 044, 045,
047, 051, 052, 053, 054, 055, 056, 057, 058, 059, 060, 062, 065, 066.

### D4 amendment, 2026-09-30 — team mode's thirteen files, named before any is written

*D4 carries a one-line pointer to this amendment. The amendment lives here, beside the DDL it
names.*

ADR-0028 point 6 asks for exactly this: team mode's migration names written down before
the tasks that write them start. There are now two sets. The board's stay in
`src-tauri/migrations/`. The runner store's live in `crates/runner/migrations/` (ADR-0028
point 3). Task 040 creates that directory, with its own `build.rs` `rerun-if-changed` line,
as `crates/core/build.rs` has for the board. Each set has its own `_sqlx_migrations` table in
its own file, so the two sets are ordered independently, and each only needs to be ordered
within itself.

Board, `src-tauri/migrations/`, in the order they apply:

```
20261001120000_run_head_and_review_bundles.sql     (task 033)
20261001120100_run_kinds_and_review_findings.sql   (task 035, carrying task 021's columns)
20261003120000_team_mode_board.sql                 (task 038, the one rebuild; D28)
20261003120100_runner_leases.sql                   (task 043)
20261003120200_consent.sql                         (task 045)
20261003120300_identity.sql                        (task 047)
20261003120400_invitations.sql                     (task 051)
20261003120500_repositories_by_remote.sql          (task 054)
20261003120600_transcripts_and_retention.sql       (task 056)
```

Runner store, `crates/runner/migrations/`:

```
20261003130000_runner_store.sql                    (task 040)
20261003130100_machine_state.sql                   (task 041)
20261003130200_checkout_mapping.sql                (task 054)
20261003130300_outbox.sql                          (task 056)
```

**These names are reserved in advance, which the 2026-09-02 amendment warned against. It is
safe here for three reasons:**

- Every name sorts after every migration already on disk; the newest is `20260915120000`.
- All thirteen land on one long-lived branch, in one PR, in task order, and task order is
  version order within each set.
- If anything merges into `src-tauri/migrations/` on `main` before that PR does, it must
  sort before `20261001120000`, or these names are re-dated on the branch before merge.
  Re-dating is allowed only because none of them has shipped. After merge they are
  append-only like every other migration.

**A file is frozen once its task lands on the branch**, not once the PR merges. Every later
task's scratch database has applied it by then, and sqlx rejects a changed checksum with
`VersionMismatch`. A later task that finds a column missing from an earlier file does not
edit that file. It stops and asks, and gets an amendment naming a new file that sorts after
the last one on disk.

**Task 065's drop is deliberately left unnamed.** It ships a release later, and naming it
now would be exactly the bet on merge order that the 2026-09-02 lesson describes.

Every task that writes one of these files regenerates the offline query cache in the same
task: with D5's recipe before task 040, and from 040 on with D33's, which regenerates both
caches. Task 040 adds the runner set's prepare step to CLAUDE.md, and the runner crate's
clippy and test steps to CLAUDE.md and CI, identically in both. CI still runs no prepare
step (D5, D33 point 5).

**The count is now sixteen board files and four runner files, and that is the whole list.**
Otherwise the prohibition is unchanged: a task that believes it needs another file stops and
asks.

**Binds.** 021, 033, 035, 038, 040, 041, 043, 045, 047, 051, 054, 056 and 065, in addition
to everything this entry already bound.

### Amendment, 2026-09-30 — two columns in task 035's file, before it is written

Part 6's DDL for `20261001120100_run_kinds_and_review_findings.sql` gains two columns. The
file is not frozen, because task 035 has not landed, so this edits the DDL rather than
naming a new file.

- **`runs.findings_recorded_at TEXT`.** D30 point 7 reads "did that `run_id` record
  anything" to tell a clean review from one whose write-back never arrived. A clean review
  calls `record_review_findings` with `findings: []`, which writes no `review_findings` row,
  so without this column the two cases are the same rows. Task 035's writer sets it once, in
  the same transaction as the rows it writes, and task 021 reads it. It is additive,
  nullable and has no `CHECK`, so part 5 allows it outside the rebuild, and 038's rebuild of
  `runs` redeclares and copies it (part 2, already applied to part 6's 038 block).
- **`review_findings.ordinal INTEGER NOT NULL`**, with `idx_review_findings_review_run`
  becoming `UNIQUE (review_run_id, ordinal)`. `review::findings::list` orders by the review
  run's `attempt`, then by the order the reviewer gave. D10's ids say nothing about order,
  `TestClock` gives one timestamp to a whole call, and the implicit `rowid` of a table with
  a `TEXT` primary key is renumbered by `VACUUM`, which the sqlite3 CLI may run (ADR-0003),
  and by any copy that does not carry it, such as a later rebuild of the table. The writer
  sets `ordinal` to the finding's index in the call, from 0. The unique index serves the
  same lookup by `review_run_id` the plain one did.

Nothing else in part 6 changes. **Binds.** 035 (writes both), 021 (reads the witness), 038
(redeclares the first). No task copies findings: task 051's copy to a team carries the
title, plan and extra instructions only (ADR-0029 point 5), and names findings among what
it leaves behind.

### Amendment, 2026-10-04 — three columns in task 054's board file, before it is written

Part 6's DDL for `20261003120500_repositories_by_remote.sql` gains three columns, already
applied above. The file is not frozen, because task 054 has not landed.

- **`runner_repositories.unattended_consent`.** D31 point 14 has the runner's report carry
  its consent for each mapping, and the table had nowhere to keep it. Without it the board
  would list a runner for a repository its queue never picks from. That is the invisible
  state ADR-0033's Consequences want made visible, and 061 shows it per runner.
- **`tasks.cleanup_pending`.** D31's 2026-10-04 amendment makes the heartbeat tell the
  runner holding a task's worktree to clean up after an archive or a move to `done`. The
  request is a column, not server memory, so a runner that was asleep at the archive still
  hears it when it wakes. The Rust enum `CleanupTrigger` is its only writer.
- **`tasks.archive_outcome`.** The JSON of D31's `ArchiveOutcomeSummary`. ADR-0025 point 6
  makes the archive's result something that is reported, and on a server that result arrives
  after `archive_task` has already returned. NULL until a runner reports one.

All three are additive and nullable or defaulted, and none has a `CHECK`, so part 5 allows
them outside the rebuild. **Binds.** 054 (writes all three), 061 (reads the first and the
third).

### Amendment, 2026-10-04 — the review digest's marker is a user setting (task 034)

`review_digest_seen_through` is an RFC 3339 instant, owned by `review::digest` in D3's shape.
Its placement is **User**: it records what this person has seen of the queue's work, so
point 4's User row gains it, and both key lists in part 6's adoption SQL (the `user_settings`
copy and the `team_settings` exclusion) name it, already applied above. No migration writes
the key, and none may before 038. **Binds.** 034 (writes it), 038 (moves it), 017 (reads the
digest it bounds).

### Amendment, 2026-10-10 — what task 039 found scoping the services

Task 039's acceptance criteria name the functions that share a transaction across modules,
and the one writer of `team_settings`. Building it needed four decisions the criteria do
not state. They are recorded here; the task file is unchanged. Each one binds the tasks
that follow. A fifth, point 5, was added when review found the fourth's trade applied to a
second caller.

**1. `context::ScopedTx` is the transaction a helper takes when it shares a caller's
transaction across modules.** `ServiceContext::begin` and `begin_immediate` return it. It
wraps a `sqlx::Transaction` together with the context's `TeamScope`, exposes that scope as
`scope()`, and derefs to `SqliteConnection`, so a private helper that takes a bare
connection is handed `&mut tx` as before. There are two reasons for it.

- A helper reached from another module needs the scope to filter by, and the transaction
  is the only argument it is handed. Passing the scope separately would be a second
  argument that can disagree with the first.
- `&mut SqliteConnection` is also satisfied by a pooled connection in autocommit. A
  helper that renumbers a column (`tasks::position::rebalance_column`) or reads a row
  before writing it (`tasks::service::fetch_task_row`) must not run outside a
  transaction. `ScopedTx` can only be built from a context, so it is always a real
  transaction.

It buys no way around the structural test. `no_service_takes_a_pool_without_a_scope` treats
`ScopedTx` exactly like `Transaction`. A function visible outside its module that takes or
returns one needs its own `STORE_HANDLE_EXCEPTIONS` entry, with a reason. A private helper
needs none. `ScopedTx` is exported from `lib.rs`. Tasks 040–046 use it, not a bare
`Transaction`, when a transaction crosses a module boundary inside `rimaia-core`.

**2. The exception list gains nine entries beyond the eight 039's criterion names.** The
criterion's closing line, "a later task that needs an exception appends an entry with its
reason in the same commit", applied to 039 itself. Each entry and its reason, as written in
`crates/core/tests/tenant_isolation.rs`:

| Entry | Reason |
| --- | --- |
| `context::ServiceContext::begin` | It opens the transaction a service shares with its helpers, and gives it the context's scope. |
| `context::ServiceContext::begin_immediate` | The same, as `BEGIN IMMEDIATE`, for a read that the write after it depends on. |
| `repo::team_of_repository` | A task's create and its repository move resolve the repository's team inside their own transaction. It filters by that transaction's scope. |
| `review::digest::advance_marker` | A verdict advances the actor's marker inside its own transaction, so the column move and the marker commit together (034). It calls `set_user_in`. |
| `tasks::dependencies::dependents_in` | Deleting a task, and a review action, read the task's dependents inside the transaction that read the task. It filters by that transaction's scope. |
| `tasks::position::rebalance_column` | A move renumbers its column inside its own transaction; a failure partway through would otherwise leave the column reordered. The caller has already scoped the repository. |
| `tasks::service::move_within` | A review action writes its note and moves the card in one transaction (034). |
| `tasks::service::fetch_task_row` | Every task write reads the row it changes inside its own transaction. It filters by that transaction's scope and answers a foreign id the way it answers a missing one. |
| `tasks::service::team_of_task` | Every write that names a task resolves the task's team, for its event, inside its own transaction. It filters by that transaction's scope. |

None takes a pool. The helpers that read board rows filter by `tx.scope()`, or act on a
row their caller has already resolved under it. `advance_marker` writes the actor's own row,
which no team scope governs (D28 part 4).

**3. `set_team` takes `value: Option<&str>`. `None` deletes the row.** The criterion calls
`set_team` the one writer of `team_settings`, so that 045's revision and authorship columns
and 051's owner check reach every write. A delete is a write. A separate delete function
would be a second writer that both checks miss. A removal is checked against the scope and
the key's placement exactly as a write is. Through `set_team` it publishes
`ChangeEvent::settings(team_id)`; through `set_team_in` the caller announces, as for a write.
`each_split_settings_table_has_one_writer` scans for every statement that writes either
split table, and allows two writers of `team_settings`: `set_team_in` (point 5), and the
`base_instructions` seed row that `identity::create_personal_team` writes as the team comes
into being. That seed is part 3's, and no `set_team` can run before its team exists. Read
039's "`set_team` is the only function that writes `team_settings`" as saying this.

**4. A removed repository's strategy default is deleted inside the removal's transaction.**
D17.1 has `repo::remove` delete `strategy_default.<repository_id>` so that no row is left
behind. Before 039 the delete ran inside the removal's transaction, and it still does: it is
`set_team_in(ctx, &mut tx, team_id, &repository_default_key(id), None)`, on the transaction
that deletes the repository, before that transaction commits. Consequences:

- A removal refused partway through (tasks still reference the repository) rolls back and
  keeps its default, as before.
- The repository and its default leave together or not at all.
- A removal publishes one event, `Repositories`, naming the repository's team, as before 039.
  `set_team_in` publishes nothing, so there is no second `Settings` event.

An earlier draft of this point ran the delete through `set_team(.., None)` after the commit,
because `set_team` ran on its own statement. That predated point 5; once `set_team_in` took a
caller's transaction the reason was gone, and the two events it published were a solo
behaviour change 039's Goal forbids.

D17.1's test still holds (`removing_a_repository_removes_its_strategy_default_row`), and so
does `a_refused_repository_removal_keeps_its_strategy_default`, which also pins that a refusal
publishes nothing. `removing_a_repository_announces_only_the_repository_change` pins the one
event. All three are in `strategy/settings.rs`.

**5. The statement lives in `set_team_in`, and `set_team` is it run over the pool.**
`set_team_in(ctx, conn, team_id, key, value)` holds the placement check, the scope check and
both statements, on a connection the caller's transaction holds, and publishes nothing.
`set_team` acquires a connection, calls it, and publishes. This is the shape
`set_user`/`set_user_in` already had. It exists for `review_loop::config::set_review_settings`,
which before 039 wrote `review_instructions` and `review_config` in one transaction and
published one `Settings` event. Two `set_team` calls would have let a failure between them
commit new instructions over the old configuration, and would have published twice: a solo
behaviour change 039's Goal forbids. The save now runs both `set_team_in` calls in one
`ctx.begin()` transaction, commits, and publishes once. `repo::remove` is its second caller
(point 4).
`saving_the_global_settings_commits_both_keys_and_announces_once` in
`tests/review_loop.rs` pins this. `set_team_in` has its own `STORE_HANDLE_EXCEPTIONS` entry.

**Binds.** 040–046 (`ScopedTx` across modules; an exception appended with its reason in the
same commit), 045 (decides what a removal records, since a deleted row has no revision to
carry; its columns go in `set_team_in`), 051 (its owner check goes in `set_team_in` and covers
both of its branches), 065 (drops the legacy `settings` table that 039 leaves unread).

### Amendment, 2026-10-10 — the runner's two limit keys (task 042)

Part 4's runner row gains `max_turns` and `disallowed_tools`, as the runner's stricter
override of the team keys of the same names (ADR-0028 point 2). They are **never adopted**:
they stay out of `db::settings::RUNNER_KEYS`, `runner_placed` and task 040's adoption step,
because the board's legacy `settings` holds the *team's* values under those names, and
copying them would make the team's value the runner's override, against this part's "the
runner's stricter override starts out absent". Both are absent by default, and absent
means no override. They are read by one typed accessor beside the rule that combines them,
`runner::limits::runner_limits(&MachineContext)` (D3), straight off the machine store,
because the placement table is keyed by name and places these names with the team. A
`max_turns` that is unparseable or `0` warns and reads as absent, never as `0` and never as
the team's value; the blocklist is one pattern per line. There is no command, MCP tool or
UI: they are set in the `sqlite3` CLI (ADR-0003), and a control is task 061's.

The effective value is the stricter of the two, and only `runner::limits::{effective,
planner_max_turns}` combine them: the lower turn budget, and the team's blocklist (ADR-0012
point 3's defaults when it is unset, an explicitly empty list included as empty, D27's
`ProviderRule` tagging) followed by the runner's rules not already present, then the
caller's own operations and the operator surface. The team half is read only where
`board::service` builds `TeamLimits`.

Test: `a_fresh_adoption_leaves_both_runner_limit_keys_absent`
(`crates/runner/tests/adoption.rs`).

**Binds.** 042, 061.

---

## D29 — Runs have a kind, and every reader of `runs` says which kinds it means

**Question.** Task 035 adds `runs.kind`, and from task 021 on a task's `runs` rows are a mix
of implementation, review and fix runs (ADR-0017). Every reader of `runs` on `main` was
written when a row could only be an implementation attempt, and most of them order by
`attempt` or take "the newest row" without saying why. Put a review row among them and each
reader either stays correct or silently starts answering a different question. Examples:
`scheduler::attempts::fold` counts a review against the implementation's retry budget.
`resumable_session` resumes a review's session as an implementation. D12's card shows the
wrong outcome. Analytics reports review spend as implementation spend. And ADR-0033's
"latest successful `head_sha`" has no rule for which kinds count. Which readers take which
kinds, and what happens to `idx_runs_task_attempt`?

**Decision.** Nine points, followed by the inventory of every current reader.

1. **The column, and what may write it.** In
   `src-tauri/migrations/20261001120100_run_kinds_and_review_findings.sql` (task 035, D4):

   ```sql
   ALTER TABLE runs ADD COLUMN kind TEXT NOT NULL DEFAULT 'implementation'
       CHECK (kind IN ('implementation', 'review', 'fix'));
   ```

   The `DEFAULT` is there only for rows that existed before this migration, which were all
   implementation attempts. Production code never relies on it. `NewRun` gains
   `kind: RunKind` as a required field with no `Default`, and `start_run`
   (`runner/outcome.rs`) binds it explicitly in its `INSERT`. Otherwise a review started by
   a caller that forgot the field would be recorded as an implementation, and nothing would
   notice. Test fixtures that `INSERT INTO runs` without the column (`tests/store.rs`,
   `tests/tasks.rs`) do rely on the default, and that is correct for what they model.

   **A row's kind is written once, by `start_run`, and never updated**, in the same way D18
   point 3 treats the capture columns. A run does not change kind. `finish_run` reads the
   kind off the row it is closing and never takes it from its caller. `RunKind` lives in
   `crates/core/src/db/models.rs` beside `RunStatus`, with the same derives and
   `rename_all = "snake_case"`. It is mirrored in `src/types.ts` as
   `export type RunKind = "implementation" | "review" | "fix";`.

   **The strategy planner stays row-less** (D17.5), and `'strategy'` is deliberately
   missing from the `CHECK`. Its lease purpose is `'strategy'` with `run_id` NULL
   (ADR-0031). This makes the lease purpose a separate enum, `LeasePurpose`, with
   `From<RunKind>`, not a fourth `RunKind` variant, because the `runs` `CHECK` refuses that
   value. The invariant ties the two enums together: when `runner_leases.run_id` is set,
   the lease's `purpose` equals that run's `kind`. D28 owns the lease DDL. Task 043
   enforces the invariant in the claim service.

   `runner/strategy.rs`'s header gives three reasons the planner has no row, and its third
   ("needs a `runs.kind` column, which is a fourth migration") stops being true. Task 035
   rewrites it to point here. The reason that stands is analytics: a planner row would
   count planner spend twice, once from `tasks.strategy_plan`'s envelope (`planner_spend`)
   and once from `runs`.

2. **`attempt` stays one sequence per task, across all kinds, and `idx_runs_task_attempt`
   does not change.** `UNIQUE (task_id, attempt)` stands as written. `start_run` still
   computes `max(attempt) + 1` over *all* of the task's rows inside its transaction. That
   makes `attempt` the row's position in the task's history: implementation 1, review 2,
   fix 3, review 4. The highest `attempt` is still the newest row, and every
   `ORDER BY attempt DESC LIMIT 1` below stays a total order without further changes.

   Everything else here depends on this, and it relies on D19. `InFlight` excludes a task
   from a second process whatever the purpose of the first, so a task never has two open
   rows. Its newest row is therefore always the one that is running, or the one that ran
   last.

   On screen, the word "Attempt" retires from the run history. A review is not an attempt
   at the task. `RunHistorySection.tsx` ("Attempt {run.attempt}") and
   `RunDetailOverlay.tsx` ("— attempt N") render the kind and the number instead, for
   example "Review · #4". Per-kind counts ("second review") are derived by the view from
   the list it already has. There is no column for them.

3. **The retry budget ends at the first row whose kind *or* session differs.** This amends
   D23 point 7. `attempt_rows` selects `kind` as well as `session_id` and `exit_class`.
   `fold` keeps its single newest-first pass and gets one extra condition: counting stops
   at the first older row where `(kind, session_id)` differs from the newest row's. Without
   the kind, a fix that resumes the implementation session (ADR-0017 lets the fix phase
   "resume" it) would continue to spend the budget of the implementation that preceded
   the review. With the kind, the review row in between ends that budget. Each fix phase
   therefore gets its own budget, and so does each review, because every review starts a
   fresh session anyway. ADR-0011's "each attempt is a row sharing the task's session id"
   now reads, in practice, as "shares the newest row's kind and session id, with no other
   row in between". This is still derived from the rows and is never stored as a column.
   D23 point 7's reasoning applies unchanged.

   `resumable_session` becomes
   `resume_point(ctx, task_id) -> Result<Option<ResumePoint>>` with
   `ResumePoint { kind: RunKind, session_id: String }`. The retry path resumes **the kind
   that was waiting**. Its two callers, `scheduler/queue.rs` (the `resuming` branch of
   `try_step`) and `src-tauri/src/commands/runs.rs` ("Retry now"), dispatch on
   `ResumePoint::kind`. Task 035 wires the `Implementation` arm to `run_task` as today.
   Until task 021 wires the other two arms, they refuse with `Error::invalid` (D8: no new
   code), and before 021 no row can reach them.

   The fix phase's *resume* option is a different question with a different answer. It
   continues **the newest implementation row's** session, not the task's newest row's. The
   newest row is the review, and resuming it would give the fixer the reviewer's context,
   which is the opposite of what ADR-0017 set out to separate. Task 021 reads that session
   with a `kind = 'implementation'` filter. It never calls `resume_point`.

4. **Readers that mean "the task's newest row" take every kind.** These are:

   - D12's summary (`TASK_SUMMARY_SELECT`'s `LEFT JOIN … r.attempt = (SELECT max(attempt)
     …)`);
   - `fetch_last_run` (the `TaskDetail`);
   - `reconcile::has_scheduled_resume`;
   - `worktree::recorded_base_ref`;
   - through the summary, `selection::skip_reason`.

   The SQL does not change. All five need the waiting or running row, whatever its kind:
   `skip_reason` has to see a review's `resume_after`, or a review that hit the usage
   limit sits in `AlreadyInFlight` indefinitely (D23 point 4).

   **D12 gets an amendment:** `LastRunSummary` gains `kind`, read from the same correlated
   join and mirrored in `src/types.ts`. Then the card can tell "reviewing" from "running",
   and "review failed" from "failed". One board read still costs one query.

   `recorded_base_ref` stays correct only because of a rule this entry adds. **A review or
   fix row records the same `base_ref` and `base_sha` as the implementation row whose
   branch it runs on.** A loop never moves the branch's base. With the base copied, every
   row can describe itself, and 033's bundle for any row can be measured against the right
   base.

5. **"Latest successful `head_sha`" also takes every kind, and it is a different query
   from "newest".** Task 044 adds it as one function, `runs::latest_successful_head`:

   ```sql
   SELECT head_sha FROM runs
    WHERE task_id = ?1 AND status = 'succeeded' AND head_sha IS NOT NULL
    ORDER BY attempt DESC LIMIT 1
   ```

   It needs no kind filter because task 033's `head_sha` write at `finish_run` is the same
   for every kind: it records the worktree's `HEAD` when the run ended. A succeeded
   review's `head_sha` is the commit it cleared, and a succeeded fix's is the commit it
   produced. A failed fix is skipped even if it committed, so a dependent (ADR-0033
   point 5) branches from the last commit that some run finished cleanly.

   The **morning review (task 017) is the opposite case**, and the difference is
   deliberate. It renders the *newest* row's bundle (point 4), because the human is
   reviewing the branch as it actually is. Dependents build on the last commit that was
   verified, and the reviewer looks at everything that is on the branch.

6. **Readers of the whole history take every kind, with no filter.** These are:

   - `list_runs` (the Runs view). `RunFilter` gains `kind: Option<RunKind>`, and task 037
     adds the control.
   - `list_runs_for_task` (the panel's history, grouped into loops by task 037's view from
     the list itself). *2026-10-10 (task 037): superseded. Task 021 placed the grouping in
     core, where its phase rule lives, and `review_loop::history` returns it; the view
     renders `get_review_history` as returned and does not group again.*
   - `fetch_run` and everything built on it: `get_run`, `get_run_row`,
     `log_path_to_reveal`.
   - Both `SELECT`s in `prune_logs`. A kind filter here would leave review transcripts
     that could never be pruned.
   - `startup::missing_run_logs`.
   - `reconcile::open_runs`. A crash leaves a review open just as easily as an
     implementation.
   - `ensure_repository_is_reassignable` (any recorded run pins the repository, D13).
   - `observed_run_cost`. The setup cost that median is compared against is paid once for
     every spawned session, whatever its kind. Leaving out the short review runs would
     understate the overhead exactly where it hurts most.
   - Later: task 056's server-side retention, and the per-runner reconcile in task 043.

   D18 applies to review and fix rows unchanged, because `finish_run` writes their seven
   capture columns the same way.

7. **Analytics (ADR-0022, `analytics::runs_in`) selects `r.kind`, and each figure says
   which kinds it counts.**

   - **Every kind:** `spend_usd`, `spend_by_day`, `runs_without_cost`, `models`,
     `runs_without_model`, `longest_run`, `unattended_hours`, `tasks_attempted`,
     `tasks_completed`, and therefore `cost_per_completed_task_usd`. A task's review loop is
     part of what the task cost, and the module header's "every failed attempt included"
     extends to it. The model mix is where ADR-0017's "a review is often worth more effort"
     becomes visible.
   - **Implementation only:**
     - `implementation_spend_usd`, which today equals `spend_usd`, so its name would stop
       being true once loops exist.
     - `outcomes` and its `failure_rate`. A review's `succeeded` means the reviewer ran,
       not that the work was good.
     - `median_duration_seconds`. A median over forty-minute implementations and
       three-minute reviews describes neither.
     - `strategies`, which buckets by the task's `strategy_mode`, and that setting does not
       choose a review's model.
   - **New:** `review_loop_spend_usd` (review and fix rows) and `review_loop_outcomes:
     RunOutcomes`. The invariant `spend_usd == implementation_spend_usd +
     review_loop_spend_usd` is asserted.

8. **Loop numbers and digests are derived, never stored.** Which loop a row belongs to is
   the number of review rows after the task's newest implementation row. It is computed
   from the rows, never stored in a `loop` column, for D23 point 7's reason. Task 034's
   overnight digest lands before kinds exist. It reports **one entry per task**, with the
   task's status taken from its newest row. Task 035's audit adds the loop summary to that
   entry. A digest that listed runs would list one looped task three times.

9. **Every door carries the kind.**

   - `Run` gains `kind: RunKind`. The `SELECT *` readers (`list_runs_for_task`,
     `fetch_run`, `RUN_LIST_SELECT`'s `r.*`) pick it up through `FromRow`. The two
     explicit `query_as!` column lists add `kind AS "kind: RunKind"`: `fetch_run_row` in
     `runner/outcome.rs` and `fetch_last_run` in `tasks/service.rs`.
   - `mcp::responses::RunView` gains `kind`, so `get_task`'s `last_run` says which kind it
     is.
   - `finish_run`'s `apply_to_task` dispatches on the kind of the row it just closed. The
     `Implementation` arm is today's code. What a finished review or fix does to the task
     is task 021's, under ADR-0017's exits.
   - The `BoardPort` methods that open and close a run (D31, task 036), and their HTTP
     adapter (task 052), carry the kind as D31's `StartRun::kind`.
   - D28's rebuild of `runs` copies `kind` and recreates `idx_runs_task_attempt` unchanged.

   Task 035 regenerates `.sqlx` for the three changed query macros (D5).

**Every reader of `runs` on `main` @ 728a049, and which rule applies to it.**

| File | Reader | Rule |
| --- | --- | --- |
| `crates/core/src/scheduler/attempts.rs:96` | `attempt_rows` → `fold`, `resumable_session` | point 3 |
| `crates/core/src/runner/outcome.rs:670` | `start_run`'s `max(attempt)` | point 2, unchanged |
| `crates/core/src/runner/outcome.rs:683` | `start_run`'s `INSERT` | point 1, binds `kind` |
| `crates/core/src/runner/outcome.rs:747` | `finish_run`'s `UPDATE` | never writes `kind` |
| `crates/core/src/runner/outcome.rs:874` | `fetch_run_row` | point 9, adds `kind` |
| `crates/core/src/runner/outcome.rs:1044` | `observed_run_cost` | point 6, every kind |
| `crates/core/src/runs/mod.rs:123` | `RUN_LIST_SELECT` / `list_runs` | point 6, plus `RunFilter::kind` |
| `crates/core/src/runs/mod.rs:171` | `list_runs_for_task` | point 6 |
| `crates/core/src/runs/mod.rs:238` | `fetch_run` (`get_run`, `get_run_row`, `log_path_to_reveal`) | point 6 |
| `crates/core/src/runs/mod.rs:368`, `:382` | `prune_logs`, both criteria | point 6 |
| `crates/core/src/scheduler/reconcile.rs:218` | `open_runs` | point 6 |
| `crates/core/src/scheduler/reconcile.rs:281` | `has_scheduled_resume` | point 4, newest of any kind |
| `crates/core/src/startup.rs:163` | `missing_run_logs` | point 6 |
| `crates/core/src/tasks/service.rs:237` | `TASK_SUMMARY_SELECT` last-run join (D12) | point 4, adds `kind` |
| `crates/core/src/tasks/service.rs:1206` | `ensure_repository_is_reassignable` | point 6 |
| `crates/core/src/tasks/service.rs:1270` | `fetch_last_run` | point 4, adds `kind` |
| `crates/core/src/worktree/mod.rs:605` | `recorded_base_ref` | point 4, relies on the copied `base_ref` |
| `crates/core/src/analytics/mod.rs:361` | `runs_in` | point 7 |

Indirect readers: `scheduler/selection.rs:224` (`skip_reason`, reads the summary),
`mcp/responses.rs:608` (`RunView`), `scheduler/queue.rs:1022` and
`src-tauri/src/commands/runs.rs:198` (the two `resumable_session` callers), and
`runner/process.rs:1033` and `scheduler/reconcile.rs:131` (the two `attempts::history`
callers).

Test-only readers stay valid through the default: `tests/analytics.rs:171`,
`tests/runner_credentials.rs:328`, `tests/runner_strategy.rs:1471` (it asserts the planner
writes no row, and still must), `tests/scheduler.rs:3296`, `:3560` and `:3573`,
`tests/store.rs:1413`, `tests/tasks.rs:1907`, and `runs/mod.rs`'s own tests (`:822–849`).

In the frontend, `src/types.ts` (`Run`, `LastRunSummary`, `RunListEntry`) gains the field.
Task 037 renders it in `TaskCard`, `RunStateBadge`, `RunHistorySection`,
`RunDetailOverlay`, `RunsView` and `AnalyticsView`.

**Required tests (task 035):**

- `a_review_between_two_fix_phases_ends_the_first_fix_phases_budget`
- `a_waiting_review_resumes_as_a_review_and_not_as_an_implementation`
- `the_cards_last_run_is_the_newest_row_of_any_kind`
- `pruning_a_task_removes_its_review_and_fix_transcripts_too`
- `implementation_and_review_loop_spend_sum_to_total_spend`

**Required test (task 044):**

- `a_dependent_branches_from_the_last_succeeded_head_and_skips_a_failed_fix`

**Why.** Every reader asks one of three questions: what is happening to this task now, what
was the last verified result, and what has ever happened. Each question has one correct
set of kinds. Leaving each reader to pick its own set is how two of them come to disagree
about the same task. The newest-row question has to include reviews, or a review that is
waiting on a usage limit never resumes. The verified-commit question has to skip a failed
fix, or a dependent builds on half-finished work.

Keeping `attempt` as one sequence per task is cheaper than it looks, and the alternatives
cost more than they look:

- A `UNIQUE (task_id, kind, attempt)` index gives three rows the same `attempt` number.
  Every `ORDER BY attempt DESC LIMIT 1` above would then need a tiebreak. `started_at` is
  the only candidate, and `TestClock` gives equal timestamps, so the tests would stop
  pinning the order.
- A separate `review_runs` table has exactly the same row shape. Every "all kinds" reader
  in point 6 would become a `UNION`, and `runner_leases.run_id` would need a foreign key
  into two tables.

The session rule in point 3 is the one change that genuinely alters behaviour. It is
spelled out here because the failure it prevents is silent: a fix loop that starts one
retry short of its cap, or a "Retry now" that resumes a reviewer as an implementer. No
error is raised in either case, only a worse night.

**What is deliberately *not* here.** What a finished review or fix does to the task
(`run_state`, column, flags) is ADR-0017's and task 021's. The findings table is task
035's migration and not a reader of `runs`. The lease DDL is D28's. The push postcondition
on every successful phase is ADR-0033 point 4, as amended 2026-10-04, and task 057's.

**Binds.** 017, 021, 033, 034, 035, 036, 037, 038, 043, 044, 052, 056, 062. It amends D12
(`last_run.kind`) and D23 point 7 (the budget boundary).

### Amendment, 2026-10-04 — point 5 takes implementation and fix rows only (task 044)

Point 5's "no kind filter" is wrong in exactly one case. A reviewer that moves `HEAD` has
made commits nobody reviewed, and task 021 calls that a failed review
(`review_changed_branch`, "HEAD moved → Unreviewed"), but the row's `status` is still
`succeeded`. With every kind, a dependent would build on those commits. A reviewer that
leaves `HEAD` alone records the head it was handed, which is already the head of the row
before it, so the filter changes nothing in the clean case. The query becomes:

```sql
SELECT id, head_sha FROM runs
 WHERE task_id = ?1 AND status = 'succeeded'
   AND kind IN ('implementation', 'fix')
   AND head_sha IS NOT NULL AND trim(head_sha) <> ''
 ORDER BY attempt DESC LIMIT 1
```

It also selects `id`, because task 045 needs the run that produced the base, and it excludes
a blank `head_sha` in the `WHERE` like a NULL, so a blank row never ends the search early.

From task 044 on, **`runs.base_ref` is a label and `runs.base_sha` is authoritative.** A
chained run's `base_ref` is the dependency's branch name, or the commit when that branch is
gone, and its `base_sha` is the dependency's last successful head, which can be behind that
branch's tip. D28 part 6's comment on task 033's file ("`base_sha` is what that name
resolved to") is read with this refinement.

**Binds.** 044 (the query), 045 (reads the `id`), 061 (the same kinds in its batched read).

### Amendment, 2026-10-09 — point 8 counts phases, not rows (task 021)

Point 8's loop number counted review *rows* after the task's newest implementation row. A review
that hits a usage limit and resumes is two rows, and would count as two loops. **A loop number
counts review phases after the newest implementation phase**, a phase being a maximal run of
contiguous rows sharing `(kind, session_id)` — point 3's budget boundary. Task 035's digest
count, `DigestLoop::reviews_since_implementation`, is computed through the same builder
`ReviewLoopSummary` uses (`review_loop::current_loop`), so the digest and a card cannot disagree
about a retried review. Still derived, never stored.

The same reading applies to every count ADR-0017's loop makes: the budget (fix phases), the
witness that a review recorded (any row of its phase), and whether `HEAD` moved (across the
phase). ADR-0017's amendment of this date states them.

**Binds.** 021, 037.

---

## D30 — The run-scoped handle is served as `rimaia-run`

**Question.** Task 020 denied `mcp__rimaia*` to every implementation run whatever the
operator's blocklist says (`RIMAIA_TOOL_SURFACE` in `crates/core/src/runner/process.rs`,
spelled by `rimaia_tool_surface` in `crates/core/src/runner/provider/claude.rs`). The
reason: `run_environment = inherit` loads the operator's unscoped `/mcp`, and
`bypassPermissions` auto-approves MCP calls. The denial works by *tool name*, and the scoped
handle registers under the same server name, `rimaia` (`MCP_SERVER_NAME`,
`crates/core/src/mcp/mod.rs`). The process.rs doc comment titled "A trap for task 021", and
021's own Notes, list three ways out and choose none. Tasks 035 and 021 give the handle to
review and fix runs, and those inherit the operator's configuration. So they will hold the
operator registration and the handle at the same time. ADR-0032 §6 then asks for every MCP
server "whose URL points at the Rimaia server or the runner's loopback operator port" to be
denied "by name", but Claude Code denies by tool name only. It cannot be given a URL. Which
of the three ways out, how the planner's spelling changes, what a review or fix grant may
call, and how "deny by URL" becomes a list of names are all open.

**Decision.** Eight parts.

1. **Two server names, two constants.** `MCP_SERVER_NAME = "rimaia"` stays, and now means
   the **operator** surface only: the name the operator registers with `claude mcp add`,
   and the name `ServerHandler::get_info` reports on `/mcp`. A new
   `RUN_MCP_SERVER_NAME: &str = "rimaia-run"` sits beside it in `crates/core/src/mcp/mod.rs`,
   and is used in four places:
   - the key of the `--mcp-config` document (`mcp_config_json`);
   - the name `get_info` reports when `RimaiaServer.scope` is `RunScope::Run`;
   - the server segment of every `required_tools` entry;
   - the tool names a prompt or system append tells a run to call.

   Every `RimaiaHandle` is built with `server: RUN_MCP_SERVER_NAME`. That covers the two
   sites in `runner/strategy.rs` and 035's and 021's new ones. A test asserts that no
   handle is ever built with the operator name. The document the planner is handed becomes:

   ```
   --mcp-config {"mcpServers":{"rimaia-run":{"type":"http","url":"http://127.0.0.1:<port>/mcp/run/<token>"}}}
   ```

   This replaces the `"rimaia"` key in D17.4's and ADR-0006's 2026-08-28 example on that
   point only. Task 035 appends a dated one-line pointer to both, and edits nothing else in
   either. The route, `/mcp/run/{token}`, the token and `RunHandles` are unchanged. The
   operator's registration name is unchanged too: tasks 059 and 060 keep `rimaia` in every
   `claude mcp add` line, and nothing ever registers `rimaia-run` in a user's configuration.

2. **The operator-surface denial becomes unconditional, and moves to where no caller can
   drop it.** `forbidden_operations` (process.rs) appends
   `ForbiddenOperation::RimaiaToolSurface` itself, for **every** intent the runner builds:
   implementation, strategy, review and fix. The `extra` argument keeps only the
   caller-specific denials. `run_task` passes nothing, and the planner passes
   `PLANNER_FORBIDDEN`. The surface is always spelled at `MCP_SERVER_NAME` plus the aliases
   from (6), and **never** at `RUN_MCP_SERVER_NAME`. A test pins that
   `claude::spell_out` emits no `mcp__rimaia-run` pattern for any intent. The planner is
   `strict_local`, so its denial is belt and braces, and it costs nothing now that the
   names differ. The "A trap for task 021" doc comment is replaced by a pointer to this
   entry. ADR-0032 §6 calls this denial `runner::process::rimaia_tools_denied_to_a_run`. No
   item has that name. The ADR means `RIMAIA_TOOL_SURFACE` together with
   `rimaia_tool_surface`.

3. **`required_tools` is spelled at the handle's server, not at a constant.**
   `ClaudeProvider::plan_spawn` spells each entry as
   `self.tool_handle(handle.server, tool)`, using `intent.rimaia_handle`. The private
   `claude::tool_handle` helper, hard-wired to `MCP_SERVER_NAME`, is kept for
   `rimaia_tool_surface` only. An intent with a non-empty `required_tools` and
   `rimaia_handle: None` is refused by `negotiate` on `RefusalAxis::HandleInjection`: a
   grant with nothing to grant it through is a wiring bug, and must never become a silent
   `--allowedTools` for the operator surface. `tool_handle` also normalises the server
   segment the way the CLI does, turning any character outside `[A-Za-z0-9_-]` into `_`.
   That matters for the aliases in (6), which are operator-chosen names.

4. **The planner, respelled.** `runner/strategy.rs` changes three things.
   - `required_tools` stays `vec![Tool::SetTaskStrategy.as_str()]`, so argv carries
     `--allowedTools mcp__rimaia-run__set_task_strategy`.
   - `compose_strategy_prompt` receives `tool_handle(RUN_MCP_SERVER_NAME,
     "set_task_strategy")`.
   - `compose_strategy_system_append` receives the same value.

   The planner's argv also gains the (2) denial after `PLANNER_FORBIDDEN`'s patterns. Task
   035 updates the exact strings in the tests rather than loosening them:
   - `TOOL` in `tests/prompt.rs`;
   - `SET_TASK_STRATEGY_TOOL` in `tests/runner_strategy.rs`, and its
     `config["mcpServers"]["rimaia"]` lookup;
   - the handle cases in `tests/runner_process.rs` and `tests/provider_seam.rs`.

5. **A grant says what it is for, and the run table is keyed by it.** The grant and the
   scope change in `crates/core/src/mcp/scope.rs`:
   - `RunHandles::grant(task_id)` becomes `grant(task_id, Grant)`, where
     `Grant = Strategy | Review { run_id } | Fix { run_id }`;
   - `RunScope::Run { task_id }` becomes `RunScope::Run { task_id, grant: Grant }`;
   - `Tool::run_access(self)` becomes `Tool::run_access(self, GrantKind)`, where
     `GrantKind` is the `Copy` discriminant.

   An implementation run still gets no grant and `rimaia_handle: None`. ADR-0016's reason
   is unchanged. The table:

   | Tool | `Strategy` | `Review` | `Fix` |
   | --- | --- | --- | --- |
   | `get_task` | own task | own task | own task |
   | `get_base_instructions`, `list_repositories` | ✔ | ✔ | ✔ |
   | `set_task_strategy`, `update_task`, `add_task_link`, `remove_task_link` | own task (as today) | ✘ | ✘ |
   | `record_review_findings` | ✘ | own run (`run_id`) | ✘ |
   | `resolve_review_finding` | ✘ | ✘ | own task's open findings |
   | everything else, including 034's review actions | ✘ | ✘ | ✘ |

   035 adds the two findings tools and owns their arguments. The findings store is D28's
   DDL and task 035's, not this entry's. `RunScope::authorize` refuses both of them on
   `RunScope::Operator` through a new `Tool::is_run_output(self) -> bool`: a finding the
   operator wrote would look exactly like a reviewer's in the morning. No UI command writes
   a finding, so ADR-0021's parity is not affected. 034's approve, reject and
   needs-changes tools are `Refused` for every grant, because a run approving a review is a
   run marking its own homework. On the hosted server (ADR-0035 §5, task 055), the
   server's second check calls the **same** `run_access`. It maps lease purpose `strategy`
   to `Strategy`, `review` to `Review`, and `fix` to `Fix`. A lease with purpose
   `implementation` is allowed no tool.

6. **"Deny by URL" is resolved to names before the spawn.** Task 055 adds
   `AgentProvider::inherited_mcp_servers(&self, home: &Path, workspace: &Path) ->
   InheritedMcp`. The result carries `servers: Vec<McpRegistration { name, urls, source }>`
   and `unreadable: Vec<(PathBuf, String)>`. The default implementation returns nothing.
   Claude's reads, in the child's environment as `SpawnPlan` leaves it: `CLAUDE_CONFIG_DIR`
   is stripped by `inherited_identity_vars`, so the run reads `$HOME/.claude.json`. The
   files are:
   - `<home>/.claude.json`: its top-level `mcpServers`, and `projects.*.mcpServers` for
     **every** project key;
   - `<workspace>/.mcp.json`, read whether or not the operator approved it;
   - `managed-mcp.json` at `/Library/Application Support/ClaudeCode/` on macOS,
     `/etc/claude-code/` on Linux, and `C:\Program Files\ClaudeCode\` on Windows.

   `urls` is the `url` of an `http` or `sse` entry. For a `stdio` entry it is every
   `command` or `args` element that parses as an `http(s)` URL, which catches
   `mcp-remote http://127.0.0.1:4517/mcp`.

   A pure function, `mcp::OwnEndpoints::is_own(&self, url) -> bool`, parses with
   `reqwest::Url` (no new dependency, per D6). A URL is Rimaia's when either holds:
   - its host is loopback (`localhost`, `127.0.0.0/8`, `[::1]`) and its port is the bound
     MCP port, `mcp::configured_port`, or `DEFAULT_PORT`, whatever the path;
   - its origin equals the connected server's origin, taken from the runner store (tasks
     040 and 052).

   Every matching name joins the denial as
   `ForbiddenOperation::RimaiaToolSurface { aliases: Vec<String> }`, and is spelled like
   `rimaia` itself: `mcp__<name>` plus `mcp__<name>__<tool>` for `Tool::ALL`.

   The resolver runs **per spawn** and **only when `run_environment = inherit`**.
   `--strict-mcp-config` loads nothing else. The aliases are filled in at the same point as
   `prompt` and `workspace`, so `plan_spawn` stays a pure function of the intent and argv
   stays pinnable byte for byte. Per spawn, never cached, because a previous phase may
   have committed a `.mcp.json`: an implementation run's file is read before its review is
   spawned, and a review's before the fix.

   A missing file contributes nothing. An unparseable one is a warning on the run and
   contributes nothing, because the CLI cannot load servers from it either. An inherited
   registration **named** `rimaia-run` refuses any intent that carries a handle, with a
   message naming its `source`. Which of two same-named servers the CLI keeps is not
   verified, and is not guessed.

   The same task also strips Rimaia's own environment: the prefix `RIMAIA_`, matched
   case-insensitively, joins the union that `inherited_identity_vars` strips (D27.5). That
   includes runner and personal access tokens, and `RIMAIA_DATA_DIR`. Variables Rimaia
   *sets* for the child are applied after stripping and survive it.

   Not read, and stated as ADR-0032 §6's residual:
   - plugin-provided MCP servers;
   - claude.ai connectors. These cannot carry ADR-0035 §1's bearer header anyway.

   On a connected machine the loopback operator endpoint also requires a token
   (ADR-0030 §6, task 059). That, not the resolver, is the control that holds against a
   registration the resolver cannot see.

7. **A clean review is an explicit call, never an absence.** A review run's prompt requires
   exactly one `record_review_findings` call, with `findings: []` when it found nothing.
   Whether the review happened is read from the store: did that `run_id` record anything.
   From task 036 the board reads it inside `finish_run`, which decides what a finished run
   means (D31 point 4). Nothing parses printed output. This is D17.9's rule, for the same
   reason. A review that exits without the call is a **failed** review: the card goes to
   `in_review`, flagged unreviewed (ADR-0017), and never lands clean. Each
   handle-carrying intent lists exactly the tools its prompt tells it to call, in
   `required_tools`:
   - strategy: `set_task_strategy`;
   - review: `record_review_findings`;
   - fix: `resolve_review_finding`.

   It does so whatever the permission posture is. Under `acceptEdits` the list is what
   makes the call possible at all, and under `bypassPermissions` it is harmless and
   records the grant in argv. This entry does not choose the review or fix posture, or
   their `run_environment`; task 021 does. A provider with
   `IsolationSupport::InheritExceptWhenHandleInjected` refuses an `inherit` review on
   `RefusalAxis::Isolation`, and that refusal is a failed review by the rule above.

   *Amendment, 2026-09-30.* "Did that `run_id` record anything" is read from
   `runs.findings_recorded_at` (D28's amendment of this date), never from a count of
   `review_findings` rows: a clean review's call writes no row but does set the column. Set
   means the review called; `NULL` means it did not.

8. **Two CLI facts are recorded, not assumed.** Task 035 records a fixture with the pinned
   CLI, in `crates/core/tests/fixtures/cli/` (D27.6). It shows two things:
   - A hyphenated server name is callable as `mcp__rimaia-run__set_task_strategy`.
   - `--disallowedTools mcp__rimaia` does **not** deny `mcp__rimaia-run__*`, because the
     rule matches the server segment exactly and not as a prefix.

   If the second fact fails, `rimaia_tool_surface` drops the bare `mcp__<name>` entry and
   keeps the per-tool entries, which are exact strings. The fixture's test is named for
   whichever behaviour it pins. A `MINIMUM_VERSION` bump re-records it. Task 055 records
   the same way the rewrite from (3) and the config-file list from (6).

**Why.** The property to keep is the one the denial bought: "a run reaches its **own** card
and nothing else". Of the three ways out that 021 lists, only a distinct name keeps it
without a condition:
- *"Deny only when no grant was minted"* is wrong, not merely weaker. The runs that get a
  grant are exactly the review and fix runs that inherit the operator's `rimaia`
  registration. It would open the whole operator surface (`move_task`, `create_task`, every
  configuration tool) to precisely the runs that carry a handle, under `bypassPermissions`.
- *"`--allowedTools` overrides `--disallowedTools`"* is unverified. It would also make
  safety depend on precedence between two variadic flags that the next CLI release is free
  to change.

A second name costs one constant and a respelled test string, and the denial never needs to
know whether a grant exists.

(5) exists because handing review and fix runs a handle would otherwise inherit the
planner's table. That table lets a run `update_task` its own plan and `set_task_strategy`
its own model. A fixer rewriting the plan to match what it did is marking its own homework.
On a team it also becomes a plan revision written during a run, which ADR-0032 §3 and §6
exist to catch.

(6) is the only honest reading of "deny by URL" against a CLI that denies by name. The URL
lives in configuration files the runner can read. The name is what the CLI can enforce. The
resolution has to happen per spawn and outside `plan_spawn`, or argv stops being a value a
test can hold.

(7) closes the failure ADR-0017 names first, false confidence. A reviewer whose write-back
silently failed must not read as "no findings", and D17.9 already showed that the store is
the only reliable witness.

(8) keeps to CLAUDE.md's rule for CLI behaviour: record it against the pinned version,
never assume it.

See also [ADR-0006](adr/0006-embedded-local-mcp-server.md)'s 2026-08-28 amendment (the
scoped route and its threat model), [ADR-0017](adr/0017-review-and-fix-loop.md) (the
loop), [ADR-0026](adr/0026-a-provider-seam-for-the-agent-cli.md) point 4 (denials as
operations), [ADR-0032](adr/0032-assignment-and-consent-to-run-on-a-machine.md) §6 (the
laundering rules), [ADR-0035](adr/0035-mcp-when-the-board-is-remote.md) §5 (the server's
second check), and D17.4 and D17.9.

**Binds.** 035, 021, 034, 055, 059, 060, 064.

---

## D31 — The board port: everything a runner says to the board

**Question.** ADR-0027 point 5 decides that runner code reaches the board through a trait,
with an in-process adapter for solo, an HTTP adapter for everything else and one contract
suite over both. It names the verbs: claim, heartbeat, report a transition, finish a run,
upload a transcript. Five later records add to what crosses that line:

- ADR-0031: the lease generation every report carries, and the purpose every claim has.
- ADR-0033: the head commit and the review bundle a finish carries.
- ADR-0035: the run-scoped MCP call a runner forwards.
- ADR-0036: the transcript chunk and the tail.
- ADR-0029: the team that scopes every call.

None of them writes the signature. Tasks 036, 043, 052, 055 and 056 each build one side of
it, and left alone they would write five different ones.

Today there is no seam to put it on:

- `runner::process::run_task` claims, releases at five sites, opens and closes the `runs`
  row, and reads the task three times.
- `runner::outcome::finish_run` lands the task through `apply_to_task`.
- `runner::strategy` writes the card's strategy twice, on top of the planner's own MCP call.
- `scheduler::queue::try_step` claims, reads the resumable session and releases.

Every one of them takes a `ServiceContext` and reaches the pool directly. So: what is the
trait, what does each method mean, which of today's calls goes behind which method and in
which task, and where does the suite live?

**Decision.**

1. **Where it lives.** Four files in `crates/core/src/board/`:
   - `port.rs`: the trait.
   - `types.rs`: its DTOs.
   - `service.rs`: one function per method, and the only code behind either adapter.
   - `in_process.rs`: task 036's adapter.

   The HTTP adapter is `crates/runner/src/board/http.rs` (task 052). It is runner code, and
   it needs D34's TLS-enabled `reqwest`, which `rimaia-core` does not take. The trait lives
   in `rimaia-core` so the shell, the runner and the server all use one definition. That
   keeps ADR-0027 point 6's rule (the server does not depend on the runner) without a third
   crate.

2. **The trait.** It uses boxed futures, not `async fn`. The reason is the one
   `Clock::sleep_until` already gives in `crates/core/src/clock.rs`: the trait stays
   object-safe without an `async-trait` dependency, which D6 forbids and D34 does not
   approve. It is held as `Arc<dyn BoardPort>`, the shape D27.2 uses for the provider.

   ```rust
   // crates/core/src/board/port.rs
   pub type BoardFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

   pub trait BoardPort: Send + Sync + 'static {
       // Scoped by the runner the adapter was built for, never by a request field.
       fn preview<'a>(&'a self, task_id: &'a str) -> BoardFuture<'a, RunContext>;
       fn claim<'a>(&'a self, target: ClaimTarget) -> BoardFuture<'a, Option<Claim>>;
       fn heartbeat<'a>(&'a self, held: &'a [LeaseRef]) -> BoardFuture<'a, Heartbeat>;

       // Scoped by `lease`, which carries the generation and the team.
       fn run_context<'a>(&'a self, lease: &'a LeaseRef) -> BoardFuture<'a, RunContext>;
       fn record_branch<'a>(&'a self, lease: &'a LeaseRef, branch: &'a str)
           -> BoardFuture<'a, ()>;
       fn start_run<'a>(&'a self, lease: &'a LeaseRef, run: StartRun) -> BoardFuture<'a, ()>;
       fn append_transcript<'a>(&'a self, lease: &'a LeaseRef, chunk: TranscriptChunk)
           -> BoardFuture<'a, TranscriptAck>;
       fn publish_tail(&self, lease: &LeaseRef, tail: RunTail);
       fn finish_run<'a>(&'a self, lease: &'a LeaseRef, run_id: &'a str, finish: FinishRun)
           -> BoardFuture<'a, FinishReceipt>;
       fn release<'a>(&'a self, lease: &'a LeaseRef) -> BoardFuture<'a, ()>;
       fn record_strategy<'a>(&'a self, lease: &'a LeaseRef, plan: StrategyPlan)
           -> BoardFuture<'a, ()>;
       fn record_review_findings<'a>(
           &'a self,
           lease: &'a LeaseRef,
           run_id: &'a str,
           findings: Vec<NewReviewFinding>,
       ) -> BoardFuture<'a, ()>;
       fn run_tool<'a>(&'a self, lease: &'a LeaseRef, call: RunToolCall)
           -> BoardFuture<'a, serde_json::Value>;
   }
   ```

   ```rust
   // crates/core/src/board/types.rs. Every type is #[serde(rename_all = "camelCase")].
   pub struct LeaseRef {
       pub task_id: String,
       pub generation: i64, // 0 until 043, then strictly increasing per task
       pub team_id: String, // from 038
   }
   pub enum LeasePurpose { Implementation, Strategy, Review, Fix } // D28's CHECK spelling

   pub enum ClaimTarget {
       Next { capacity: FreeCapacity, repositories: Vec<String>, wait: Duration },
       Run { task_id: String, trigger: RunTrigger, continue_session: bool },
       Plan { task_id: String },
   }
   pub struct FreeCapacity { pub total: usize, pub per_repository: BTreeMap<String, usize> }

   pub struct Claim {
       pub lease: LeaseRef,
       pub purpose: LeasePurpose,
       pub trigger: RunTrigger,           // ADR-0031 point 7's permission posture
       pub resume: Option<ResumePoint>,   // D29; chosen by the board
       pub context: RunContext,
   }
   pub struct RunContext {
       pub task: TaskDetail,
       pub repository: Repository,        // 066 and 054 narrow it to the board's pathless row
       pub base_instructions: String,
       pub strategy: EffectiveStrategy,   // ADR-0016's precedence chain, resolved board-side
       pub catalogue: Catalogue,          // for this runner's provider
       pub limits: TeamLimits,            // the team's half; the runner applies the stricter
       // 021 adds `review`, 044 `base`, 045 the authorship facts (point 6)
   }
   pub struct TeamLimits { pub max_turns: u32, pub disallowed_tools: Option<Vec<String>> }
   pub struct Heartbeat { pub fenced: Vec<LeaseRef>, pub cancel: Vec<String> }

   pub struct StartRun {
       pub run_id: String, // minted by the runner (D10)
       pub kind: RunKind,  // D29
       pub session_id: String,
       pub prompt: String,
       pub base_ref: Option<String>,
       pub base_sha: Option<String>, // 033
   }
   pub struct TranscriptChunk { pub run_id: String, pub offset: u64, pub bytes: Vec<u8> }
   pub struct TranscriptAck { pub stored_through: u64 }

   pub struct FinishRun {
       pub outcome: RunOutcome,           // `resume_after` must be None
       pub head_sha: Option<String>,      // 033
       pub bundle: Option<ReviewBundle>,  // 033
       pub window_closes_at: Option<DateTime<Utc>>,
       pub transcript: TranscriptEnd,
   }
   pub enum TranscriptEnd { Complete { length: u64 }, KeptOnRunner }
   pub struct FinishReceipt { pub run: Run, pub next: NextStep }
   pub enum NextStep {
       Released { resume_after: Option<DateTime<Utc>> },
       Continue { kind: RunKind },
   }

   pub struct RunToolCall { pub tool: Tool, pub arguments: serde_json::Map<String, Value> }

   pub enum BoardMethod { // `as_str` is the trait method's name, and the HTTP route's
       Preview, Claim, Heartbeat, RunContext, RecordBranch, StartRun, AppendTranscript,
       PublishTail, FinishRun, Release, RecordStrategy, RecordReviewFindings, RunTool,
   }
   ```

   The name is `LeaseRef`, not `Lease`. `scheduler::inflight::Lease` is D19's in-process
   slot until 042 renames it `LocalSlot`, and for the six tasks in between the two must not
   share a name.

3. **Every method carries the generation and the scope.** Solo ignores them by outcome, not
   by a code branch.

   - A method that acts under a lease takes a `LeaseRef`.
   - A method that acts before any lease exists is scoped by the runner the adapter was
     built for: its identity in process, its `rmr_` token over HTTP (047). Never by a field
     in the request body.
   - Because `LeaseRef` carries both, a field added later is not a signature change.
     `generation` exists from 036 and is `0` until 043. `team_id` arrives with 038.

   Both adapters pass both values to the same `board::service` function:
   - From 043, the service compares `generation` with the live lease by equality. A
     mismatch is `Conflict`.
   - From 038/039, the service runs the call under a context scoped to `team_id`. A lease
     whose task is not in that team is `NotFound` (ADR-0029 point 5).

   In solo neither check can fail: nothing expires a solo lease, and nothing re-claims one
   from under its holder. That is the only sense in which solo "ignores" them. The contract
   suite still makes both checks fail on purpose through the in-process adapter.

   The in-process adapter uses `team_id` to scope its context without a lookup. The server
   treats it as a claim to verify, never as an input.

   `generation` must never repeat for a task, including after a release. Otherwise a holder
   fenced long ago could match a later lease. D28's `tasks.lease_generation` is how the
   counter survives a released row, and 043 increments it inside the claim's transaction.

   A `LeaseRef` names a lease, not what the lease is for. `purpose` lives on the lease row
   and on `Claim`, because `start_run` moves it (implementation → review → fix) under one
   generation. A purpose copied onto the reference would go stale, and fencing on it would
   check the wrong field.

4. **What each method means.**

   - **`preview`** returns the context a claim would return, and writes nothing. It is the
     only read without a lease, and it exists only for a starter's preflight: the
     repository id that D19's slot is taken for, ADR-0012's opt-in, `negotiate`, and Plan
     now's refusals (`claim_for_planning`). It is advisory. A run is always composed from
     its claim's context, never from a preview.
   - **`claim`** is ADR-0031's single path for every process a runner starts.
     - `Run` with `continue_session: false` is Run now (today's `scheduler::claim::claim`).
     - `Run` with `continue_session: true` is Retry now, or a due retry (today's
       `claim_retry`). Only this form comes back with `resume` set.
     - `Plan` takes purpose `strategy`: a lease, no `runs` row and no `run_state` edge,
       exactly D17's planner.
     - `Next` is the runner loop's form (042). The board picks the task and the purpose,
       including a strategy run the hosted server recorded as a request (060).

     `None` means there is nothing to report: nothing became eligible within `wait`, or
     another starter got there first (today's `ClaimOutcome::Lost`). The starter words
     that the way it does now. A refusal that a person has to read (not eligible for this
     runner, pinned to another runner) is an `Err`. In process, `wait` is honoured by
     waiting on `ServiceContext::subscribe` and `Clock::sleep_until`, never on
     `tokio::time::sleep`.
   - **`heartbeat`** renews, in one transaction, every lease in `held` whose generation is
     current, and returns the others in `fenced`. That is ADR-0031 point 4's "refused as
     `Conflict`", answered per lease, so one stale lease does not cost the others their
     renewal. It refines ADR-0031 point 3's "renews all of that runner's leases": the
     runner's list is the definition of "all". A lease the runner does not list is not
     renewed and expires. That is also how a claim whose reply never arrived gets recovered
     (053).

     `cancel` lists tasks whose run someone stopped from another client. Apart from the
     claim, the heartbeat is the board's only channel to a runner, so the request travels
     there (053). In process both lists are empty, because a solo Cancel reaches
     `InFlight::cancel` directly.
   - **`run_context`** re-reads the context under a lease. It is used after a planner has
     written, before a prompt is composed, and for the planner's "did it write" check on
     `strategy_updated_at`.
   - **`record_branch`** writes `tasks.branch`. `tasks.worktree_path` never crosses the
     port (ADR-0028 point 2).
   - **`start_run`** opens the `runs` row. The runner mints `run_id` (D10), so the report
     is idempotent by id and needs no reply. That is what lets 056's outbox hold it. The
     board:
     - computes `attempt`;
     - records the runner;
     - sets the lease's `run_id` and moves its purpose to `kind` (043);
     - in process, fills `log_path` from its `AppPaths`, until 056's `transcript_key`
       replaces it.
   - **`append_transcript`** is keyed by byte offset.
     - A chunk already stored is acknowledged again, without writing it twice.
     - A chunk that would leave a gap gets `stored_through` back and nothing is written,
       so the outbox rewinds.
     - In process it acknowledges without copying, because in solo the runner's file is
       the board's copy (ADR-0028 point 4).
     - A planner transcript has no `runs` row to attach to (D17), so it stays on the
       runner.
   - **`publish_tail`** is synchronous and cannot fail, because D14's rules hold end to
     end: a dropped tail message costs nothing and is never replayed. In process it is
     `ServiceContext::publish_tail`. Over HTTP it goes into a bounded channel with
     `try_send`, and the server hands it to 048's fan-out.
   - **`finish_run`**: the runner reports facts and the board decides what they mean. The
     board decides:
     - `resume_after`, by ADR-0011's table, from `attempts::history` and `retry::decide`;
     - where the task lands (`apply_to_task`);
     - whether a review loop continues (ADR-0017, task 021).

     The runner supplies only what the board cannot know: the outcome, `window_closes_at`
     from its own run window, and the usage-limit reset its CLI reported (inside the
     outcome). A runner that sends `outcome.resume_after` is refused as `Invalid`.

     The reply is one of two steps:
     - `Released`: the lease ended with the finish. If the exit class is `usage_limit`,
       the runner then raises its own usage-limit pause (`pause::note_usage_limit`) from
       `resume_after`.
     - `Continue`: the lease was kept, and the next phase is a `start_run` with that kind.
       No task before 021 returns it. `Continue` is the next phase's claim, decided in the
       same transaction, so the board applies the eligibility and consent checks a claim
       applies (ADR-0031 point 1, ADR-0032 point 5) and answers `Released` when they fail.
   - **`release`** is ADR-0027 point 5's "report a transition": it ends a claim that no
     `finish_run` ended. An implementation lease moves the task from `running` to `failed`,
     if it is still `running`. That is `scheduler::claim::release`'s rule, and it keeps a
     retryable verdict that was already written. A strategy lease leaves `run_state`
     alone. Callers treat it as best effort, as both of today's releases are.
   - **`record_strategy`** is `tasks::strategy::set_task_strategy` with
     `StrategySource::Planner`, always. A runner never writes a user's strategy.
   - **`record_review_findings`** is task 035's writer. `run_id` is the review run that
     produced the findings.
   - **`run_tool`** forwards every other `rimaia-run` tool (D30), meaning the `OwnTaskOnly`
     and `Unscoped` rows of `Tool::run_access`. The board authorizes the call again as
     `RunScope::Run { task_id: lease.task_id, grant }`, with the grant D30 point 5 maps from
     the lease's purpose (ADR-0035 point 5). It refuses `set_task_strategy` and
     `record_review_findings`, because those writes have the typed methods above. Each
     write then has exactly one door.

   **The port has no general `set_run_state`.** A runner never names a run state:
   - the claim takes both edges;
   - `finish_run` lands the task;
   - `release` abandons it.

   A setter on the port would be a second writer of that column, with a network in front of
   it. That is the defect `set_run_state`'s own doc comment exists to prevent (ADR-0006).

5. **`run_task` takes a claim.** From 036 its signature is:

   ```rust
   pub async fn run_task(
       board: &dyn BoardPort,
       ctx: &ServiceContext,
       paths: &AppPaths,
       config: &RunnerConfig,
       claim: Claim,
       request: RunRequest,
   ) -> Result<Run>
   ```

   - `RunRequest` keeps `cancel` and `in_flight`. `task_id`, `trigger` and `resume` move to
     the claim.
   - `runner::process::claim` retires. Both production starters already claim before they
     call `run_task`: `start_task_run` and `retry_task_now` through `scheduler::claim`, and
     `try_step` before `supervise`.
   - The preflight those two commands run in the shell today becomes one `rimaia-core`
     function that both call: `preview`, D19's slot, the opt-in, `negotiate`, `probe_cli`,
     then `claim`. It is a rule, and ADR-0006 does not let rules live in the shell.
   - Task 008's and ADR-0026's "refused before any run state is written" guarantees hold
     in that function. The tests that assert them by calling `run_task` on an unclaimed
     task move to it.
   - `run_task` negotiates again against the claim's context. If the two disagree (the
     board changed between the two reads), that is a refusal after the claim, and it
     releases. The queue path already behaves this way today.
   - The `ServiceContext` argument stays until 041. From 036 it is used for the clock and
     for ADR-0028's runner-owned state, and nothing else.

6. **Fields that arrive later, and who adds them.**
   - **021:** `RunContext::review` (`review_instructions` and the task's override, the loop
     configuration, and the open findings a fix phase is composed from), and
     `NextStep::Continue`.
   - **044:** `RunContext::base`, the commit a worktree branches from.
     `worktree::base_ref::resolve` moves board-side to produce it.
   - **045:** the authorship facts ADR-0032 point 7 puts in the prompt, and consent
     re-checked on `preview`, `claim`, `run_context` and `finish_run`'s `Continue`. A
     re-read never returns consent-gated content its owner has not consented to; it answers
     `Conflict` instead.
   - **033** lands before this port, so `StartRun::base_sha`, `FinishRun::head_sha` and
     `FinishRun::bundle` exist from 036. The runner computes the bundle, because it has the
     worktree, and hands it to `finish_run` as a value. The board never runs git (ADR-0033
     point 7).
   - **057:** the runner decides the push postcondition before `finish_run`, by rewriting
     the outcome the way `override_as_fatal` does. There is never a board-side check.

   Every field added after 052 is an `Option` or `#[serde(default)]`, and no DTO uses
   `deny_unknown_fields`. Version skew is handled by ADR-0037's protocol header, not by
   parse failures.

7. **Where today's calls go.** One row per call site. Line numbers drift; names do not.

   | Today | Touches | Through the port | Task |
   | --- | --- | --- | --- |
   | `queue::try_step` → `claim::claim`, `claim::claim_retry` | `set_run_state` ×1–2 | `claim(Run { trigger: Queued, continue_session })` | 036 |
   | `queue::try_step` → `attempts::resume_point` (D29); the same read in `retry_task_now` | reads `runs` | `Claim::resume` | 036 |
   | `queue::try_step` → `selection::plan`, `next_batch` | reads the board | `claim(Next)` | 042 |
   | `start_task_run` → `scheduler::claim`; `retry_task_now` → `claim_retry`; `process::claim` | `set_run_state` ×0–2 | `claim(Run { trigger: Manual, … })` in the starter; `process::claim` retires | 036 |
   | `process::release` (five sites in `run_task`); `claim::release` in `try_step` and `supervise` | `set_run_state(Failed)` | `release` | 036 |
   | `run_task` → `repo::get`, `ensure_unattended_runs_allowed` | read | `RunContext::repository`; the starter checks it on `preview`; the runner's half reads the checkout's `unattended_consent` (done in 066), and 045 adds the team ceiling to the claim | 036 |
   | `run_task` → `tasks::get_task` ×3, `settings::base_instructions`, `max_turns`, the `DISALLOWED_TOOLS` read in `forbidden_operations` | reads | `Claim::context`, `run_context` | 036 |
   | `strategy::resolve`, `effective_for` → `global_default`, `repository_default`, `catalogue`, `get_task` ×2 | reads | `RunContext::{strategy, catalogue}`, `run_context` | 036 |
   | `strategy::plan` → `get_task` for `strategy_updated_at` | read | `run_context` | 036 |
   | `strategy::claim_for_planning` → `get_task`, `repo::get`, `effective_mode` | reads | `preview`, then `claim(Plan)` | 036 |
   | `strategy::record_failure`, `stamp_run_metadata` → `set_task_strategy` | `UPDATE tasks` | `record_strategy` | 036 |
   | the planner's own `set_task_strategy` over the scoped handle | `UPDATE tasks` | `record_strategy`, from the run route | 055 |
   | `outcome::start_run` | `INSERT runs`, two publishes | `start_run` | 036 |
   | `process::apply_retry_policy` → `attempts::history`, `retry::decide` | reads `runs` | inside `finish_run`, board-side | 036 |
   | `apply_retry_policy` → `schedule::window::active` | runner-owned state | `FinishRun::window_closes_at` | 036 |
   | `apply_retry_policy` → `pause::note_usage_limit` | runner-owned state | never; the runner calls it from `NextStep::Released` | 036; storage moves in 041 |
   | `outcome::finish_run` → `apply_to_task` → `move_task_to_bottom`, `set_run_state` | `UPDATE runs`, column, run state, publishes | `finish_run` | 036 |
   | `execute` → `EventStream::create(ctx, …)` → `publish_tail` | tail channel | `publish_tail` | 036 |
   | the JSONL transcript | runner disk | `append_transcript`, `FinishRun::transcript` | 056 |
   | `worktree::prepare` → `write_worktree_columns` (also reached from `plan_claimed`) | `UPDATE tasks SET branch, worktree_path` | `record_branch`; the path is `MachineStore::record_worktree` in `runner.db` (done in 066; `write_worktree_columns` is gone, and the two leaseless branch clears share `worktree::clear_branch`) | 066 |
   | `worktree::prepare` → `base_ref::resolve` | reads dependencies | `RunContext::base` | 044 |
   | `reconcile::reconcile` after `startup::survey` | `finish_run`, `set_run_state` | per runner: `finish_run` with the interrupted outcome, then `release` | 043 |
   | `QueueHandle` verbs, `tick_schedules`, `capacity::resolve`, `pause::active_until`, `settings::run_environment` | runner-owned state | never (ADR-0031 point 6) | 041 |

   036 ships every method except `run_tool`. Its in-process body needs the run-tool
   dispatch that 055 extracts from `mcp::server`, so 055 adds it, with the signature
   above. Some methods have no production caller yet (`record_branch` got its first, in
   `worktree::prepare`, in 066):
   `claim(Next)` until 042, `heartbeat` until 053, `append_transcript` until 056 and
   `record_review_findings` until 055. Each still has its in-process body and its contract
   cases from the day it lands.

8. **Where the port is held.** `Arc<dyn BoardPort>` is a parameter of `scheduler::build`
   and a field of `PlannerAccess` and of `AppState`, where it is named `board_port` because
   D32 point 3 gives `AppState.board` to the solo command host. That is D19 point 1's shape
   for `InFlight`, for the same reason. It is not a field of `RunnerConfig`, because a board
   has no sensible `Default`. It is not a field of `ServiceContext`, because the in-process
   adapter itself holds a `ServiceContext`. It is built once in each place:
   - `src-tauri/src/lib.rs`, in `setup()`: in process in solo (036), HTTP when connected
     (059);
   - the `rimaia-runner` binary: HTTP (058);
   - `testing::context`, as `TestContext::board()`, over the same context the test uses to
     arrange its data.

9. **The in-process adapter (036).** `InProcessBoard { ctx, paths, provider }`.
   - `provider` is read only to build the catalogue. At 046 it becomes D32's
     `ProviderProfile`, so the board side never holds something that can spawn.
   - `ctx` is re-sourced to `MutationSource::System` at construction. A report comes from
     the runner, whoever pressed the button; the claim's `trigger` records which button.
   - 038 adds the solo runner's id. 043 adds `LeaseTerm::Never`, which is ADR-0031 point 5
     as a constructor argument.
   - Every method is one call into `board::service`.
   - Before 043 there is no lease row. `claim` returns generation `0` and runs today's
     routes (`claim::claim`, `claim_retry`, `resume_point`), and `heartbeat` answers with
     empty lists.

10. **The HTTP adapter (052).** `HttpBoard` lives in `crates/runner/src/board/http.rs`. It
    holds one `reqwest::Client`, the server's origin, the runner token and the runner's
    `ProviderId`. It adds the `ProviderId` to the `preview`, `claim` and `run_context`
    bodies, because the catalogue and ADR-0031 point 1's model check need it.
    - **Routes.** Every method is `POST /api/v1/runner/<BoardMethod::as_str()>` with a JSON
      body. The exception is `append_transcript`: its body is the raw bytes, with `run_id`,
      `generation` and `offset` as query parameters (ADR-0034 point 2).
    - **Headers.** Every request sends `Authorization: Bearer rmr_…` and 046's
      protocol-version header.
    - **Errors.** An error body is D8's `{ code, message }`. The adapter rebuilds the
      `Error` from `code`, never from the HTTP status.
    - **Retries.** The adapter does not retry. 056's outbox resends reports, and callers
      ask again for reads.
    - **Server side.** The handlers live in `crates/server/src/runner_api.rs`. Each one
      resolves the token to a runner, builds a context scoped from it, and calls the same
      `board::service` function the in-process adapter calls. They are not in D32's
      registry, because ADR-0034 point 2 keeps the runner protocol out of the UI's command
      list. `BoardMethod::ALL` is their registry, and 052's wiring test asserts exactly one
      route per variant.

11. **One reaction to `Conflict`.** A `Conflict` from any method that takes a lease means
    that lease is fenced. The runner reacts exactly as it does to an entry in `fenced` from
    a heartbeat (ADR-0031 point 4):
    - stop the process through the normal cancel path;
    - keep the worktree, and do not push;
    - drop the lease from `held`;
    - claim again through the pin.

    This is one function, owned by 053.

12. **A resent report is the same fact (056).** `start_run` is keyed by `run_id`,
    `append_transcript` by offset, and `finish_run` by `run_id`. If the board already
    applied a report, a resend gets the same answer as the first send. That check runs
    before the generation check, so a resend that arrives after the lease has moved on is
    acknowledged, not fenced. Until 056, `finish_run` keeps today's "has already been
    finalized" refusal.

13. **The contract suite.** It lives in `crates/core/src/testing/board_contract.rs`,
    behind the `testing` feature, so both crates' tests can reach it. It exports
    `board_contract!(Harness)`, which expands to one `#[tokio::test]` per case, so a
    failure names its case. `Harness` implements a trait with four members:
    - `async fn start()`;
    - `fn runner(&self, which: Which) -> Arc<dyn BoardPort>`, for two runners `A` and `B`
      over one board;
    - `fn board(&self) -> &ServiceContext`, to arrange and inspect the board through core
      services;
    - `fn clock(&self) -> &TestClock`.

    It is invoked from two places:
    - `crates/core/tests/board_port_in_process.rs` (036);
    - `crates/runner/tests/board_port_http.rs` (052). This one serves the real
      `rimaia-server` router on `127.0.0.1:0`, against a temporary board database driven
      by the same `TestClock`.

    `rimaia-server` is a dev-dependency of `rimaia-runner`, which is the direction
    ADR-0027 point 6 allows. Nothing sleeps: expiry, grace periods and waking up are all
    clock advances.

    Each task adds its own cases:
    - **036, the lifecycle:** claim, start, and a finish that lands the task as today;
      release by purpose; a runner-chosen `resume_after` refused; `record_strategy` sourced
      as `planner`; the tail delivered; a transcript acknowledged; an unknown lease
      answered `NotFound`.
    - **038/039:** a lease that names another team's task is `NotFound`.
    - **043:** two runners racing for one claim get exactly one `Claim`; a stale generation
      is `Conflict`; generation increases across a release and a re-claim; the heartbeat
      renews and fences per lease; `LeaseTerm::Never` survives a week of clock time.
    - **053:** expiry closes the run as `interrupted` and pins the task; restart grace;
      sleep recovery.
    - **055:** `run_tool` refuses a `Refused` tool, another task, and the two typed tools.
    - **056:** resends.

    One case, `every_lease_method_refuses_a_stale_generation`, iterates `BoardMethod::ALL`,
    the way `every_registered_tool_has_a_run_scope_decision` iterates `Tool::ALL`. A method
    added without fencing then fails the suite instead of waiting for a reviewer to notice.

14. **Not on the port.**
    - State that ADR-0028 point 2 assigns to a runner, and ADR-0031 point 6's queue
      control. These are read through `ServiceContext` until 041 and 066 move them to
      `runner.db`, and never go through the port.
    - The operator's reads and writes: `plan_all`'s `list_tasks` and every board command.
      These go through the operator's transport (ADR-0034).
    - Reports a runner makes without a lease: its checkout set, its doctor result, its
      consent per repository, and cleanup results (ADR-0033 points 2 and 8, ADR-0034
      point 5, ADR-0032 point 4). These, and the one leaseless read a runner needs to map a
      clone, are on the port after all: see the 2026-10-04 amendment below (task 054).

**Why.** ADR-0027 point 5 already decided that the port exists and that one suite binds
its adapters. It could not decide a signature, and the signature is exactly where two tasks
would each guess. The choices above are the ones a diff would not explain on its own.

**The runner reports facts; the board makes the decisions.** `apply_retry_policy` runs in
the runner today only because the runner and the board are one process. If it stayed
there, the HTTP runner would need the attempt history to choose its own retry time. A
runner that chooses its own retry time, or decides for itself when its review loop is
finished, is a second copy of ADR-0011's table and ADR-0017's budget, running on a machine
the board does not control. ADR-0027 point 1 makes the server the only writer of the board,
and ADR-0006 makes a rule held in two places a defect. So the runner sends only what it
alone can know (the outcome, the run window, the reset its CLI reported), and the board
answers. The same reasoning is why the port has no general `set_run_state`.

**The lease is one value, not separate arguments, because its fields arrive across three
tasks.** `generation` means nothing until 043, and `team_id` does not exist until 038. If
either had been a parameter, 036's signature would have changed twice before a second
machine existed.

**`run_task` taking a claim matches what production already does.** Both commands and the
queue claim before `run_task` is called. The routes that let `run_task` claim a task itself
were written before the queue existed, when it was the only claimer, and no production
caller reaches them now. Minting `run_id` in the runner is what lets a start be reported
after the connection has dropped, which ADR-0036 point 1's outbox requires of every report.

**Two write-backs are typed, and the rest are forwarded, because those two are a phase's
result.** A planner exists to call `set_task_strategy`, and a reviewer exists to record
findings. The board's next decision reads them, and the runner's own code writes one of
them (`record_failure`, `stamp_run_metadata`). Card edits and reads are incidental. Giving
each its own method would make the port grow with every row ADR-0021 adds to the run
surface. Refusing the two typed writes through `run_tool` keeps one door per write.

**The suite sits in `rimaia-core`'s `testing` module because both adapters' test crates can
reach it there.** It is keyed to `BoardMethod::ALL` for the same reason `tests/mcp_scope.rs`
is keyed to `Tool::ALL`: a method added without its fencing case should fail the suite, not
wait for someone to notice.

See also:
- [ADR-0027](adr/0027-a-server-for-the-board-and-runners-for-the-work.md) point 5: the
  port;
- [ADR-0031](adr/0031-runners-claim-work-with-leases.md): leases, fencing, pinning;
- [ADR-0035](adr/0035-mcp-when-the-board-is-remote.md) point 5: the forwarded run call;
- [ADR-0036](adr/0036-transcripts-and-review-artifacts-leave-the-machine.md) points 1–3:
  chunks, tail, bundle;
- D14 (the tail), D17 (the planner has no `runs` row), D19 (the in-process slot), D28
  (`runner_leases`, `tasks.lease_generation`), D29 (`runs.kind`, `ResumePoint`), D30
  (`rimaia-run` and its grants), D32 (`ProviderProfile`, `AppState.board`).

**Binds.** 021, 033, 035, 036, 038, 039, 041, 042, 043, 044, 045, 046, 047, 048, 052, 053,
054, 055, 056, 057, 058, 059, 060, 066.

### Amendment, 2026-10-04 — two leaseless methods, and cleanup on the heartbeat (task 054)

Point 14 gave task 054 one method for a runner's leaseless reports. Two gaps, found before
054 was written, make that two methods and one heartbeat field:

- **A headless runner cannot read the board any other way.** It holds only an `rmr_` token,
  and 047 refuses that token on every board route. Mapping a clone needs the board's
  repository, and 058's `checkout add` needs to find repositories by remote. Widening a
  board route to runner tokens is exactly what 047's test forbids, so the read goes on the
  port.
- **ADR-0033 point 8's "reports the result" had no request to answer.** Point 4 makes the
  heartbeat the board's only channel to a runner, and it carried fences and cancels only.

Point 2's trait gains two methods in its runner-scoped group, and `BoardMethod` gains
`FindRepositories` and `ReportRunner`:

```rust
fn find_repositories<'a>(&'a self, lookup: RepositoryLookup)
    -> BoardFuture<'a, Vec<RepositoryRef>>;
fn report_runner<'a>(&'a self, report: RunnerReport)
    -> BoardFuture<'a, RunnerReportReceipt>;
```

```rust
pub enum RepositoryLookup { ById(String), ByRemote(NormalizedRemote) }
pub struct RepositoryRef {
    pub id: String,
    pub team_id: String,
    pub team_name: String,
    pub name: String,
    pub default_branch: String,
    pub normalized_remote: Option<String>,
    pub served_remote_less_by: Option<String>, // see below
}

pub struct RunnerReport {
    pub checkouts: Vec<CheckoutReport>,       // the whole verified set; replaces the last
    pub doctor: Option<DoctorSummary>,        // None leaves the last one standing
    pub branches_deleted: Vec<BranchDeleted>, // events; each is idempotent
    pub cleanups: Vec<CleanupDone>,           // events; each is idempotent
}
pub struct CheckoutReport {
    pub repository_id: String,
    pub normalized_remote: Option<String>,    // the checkout's verified column
    pub unattended_consent: bool,
    pub push: Option<PushCheck>,              // None leaves the stored push columns
}
pub struct PushCheck { pub checked_at: DateTime<Utc>, pub error: Option<String> }
pub struct DoctorSummary { pub ran_at: DateTime<Utc>, pub checks: Vec<ReportedCheck> }
pub struct ReportedCheck {
    pub check: String,                        // `Check::as_str`, so a newer check parses
    pub status: CheckStatus,
    pub repository_id: Option<String>,
}
pub struct BranchDeleted { pub task_id: String, pub branch: String }
pub struct RunnerReportReceipt { pub unknown_repositories: Vec<String> }

pub enum CleanupTrigger { Archived, Done } // tasks.cleanup_pending: 'archived' | 'done'
pub struct CleanupRequest {
    pub task_id: String,
    pub team_id: String,
    pub trigger: CleanupTrigger,
}
pub struct CleanupDone {
    pub task_id: String,
    pub trigger: CleanupTrigger,
    pub outcome: Option<ArchiveOutcomeSummary>, // Some for Archived, None for Done
}
pub enum ArchiveOutcomeSummary {
    Nothing,
    WorktreeRemoved { bytes_freed: u64 },
    ScriptRan { exit_code: Option<i32> },
    Failed,
}
```

`Heartbeat` becomes `{ fenced, cancel, cleanup: Vec<CleanupRequest> }`, the new field
`#[serde(default)]` (point 6). What the additions mean, beside point 4:

- **`find_repositories`** reads repositories in the teams the adapter's runner's owner
  belongs to, and nothing else. An id outside them and an id that does not exist both get
  an empty answer, never `NotFound` (ADR-0029 point 5). `ByRemote` returns every match, one
  per team, so the same remote in two of the owner's teams returns both.
  `served_remote_less_by` is set only on a remote-less repository that some other runner,
  not unpaired and with its owner still a member, reports. Point 4's "`preview` is the only
  read without a lease" now has this exception, and it returns no task.
- **`report_runner`** is defined by task 054's Scope: the checkout set is a snapshot,
  unknown ids are named in the receipt and written nowhere, and both event lists are
  idempotent.
- **`Heartbeat::cleanup`** lists the calling runner's owed cleanups: every task whose
  `tasks.cleanup_pending` is set and whose latest `runs` row, of any kind, names this
  runner. The board sets the column when a task is archived or enters `done` through a call
  that has no machine to react on, which means on a server. Unarchiving, or moving out of
  `done`, clears it. A `CleanupDone` clears it only while it still equals the reported
  trigger. The runner reacts with the functions solo calls in the same call:
  ADR-0025's policy for `Archived`, and D20.3's best-effort removal, under the runner's own
  `worktree_auto_cleanup`, for `Done`. It then reports. In process the list is always
  empty, because solo has already reacted.
- **`ArchiveOutcomeSummary` is D26 point 3's `OnArchiveOutcome` without its
  prose.** A script's output and a failure's reason can name paths, and ADR-0028 point 2
  keeps paths off the board. They stay in the runner's log.
- `every_lease_method_refuses_a_stale_generation` excludes both new methods, as it
  excludes `Claim` and `Heartbeat`, because neither acts under a lease.

**Binds.** 052 (routes and adapter bodies through its structure), 053 (the heartbeat's
answer grows; its body does not), 054 (implements all of it), 058 and 059 (map through
`find_repositories`), 061 (renders what the report stores).

### Amendment, 2026-10-04 — `preview` gains a second reader (task 059)

Point 4 says `preview` "exists only for a starter's preflight". From 059 it has a second
reader, `preview_composed_prompt`, so that a connected desktop's prompt preview is composed
from the context a claim would return and stays byte-for-byte a run's prompt (task 006).
Nothing else about the method changes: it is advisory, it writes nothing, and a run is still
composed from its claim's context. D35 point 8 is the decision. A third reader amends D35.

**Binds.** 059.

### Amendment, 2026-10-09 — what task 036 found building the in-process side

Three refinements, each found while writing the in-process adapter. None changes the trait
signature in point 2.

**Point 4: the runner notes the usage-limit pause twice, and once before `finish_run`.**
Point 4 has the runner raise its pause from `NextStep::Released`, after the board decided
`resume_after`. That opens a window the old `apply_retry_policy` did not have:
`finish_run` publishes, the publication wakes the queue, and a free slot can start another
task into the window this run just found closed, before the pause exists. So for a
`usage_limit` outcome whose `usage_limit_resets_at` is known, the runner calls
`pause::note_usage_limit` at that reset **before** `finish_run`, and again at
`resume_after` after `Released`, if there is one. `note_usage_limit` only ever lengthens
the pause, so the second call can only move it later. Three cases follow, and all three
are deliberate:

- **The board resumes the task.** The stored pause ends at `resume_after` (the reset plus
  this run's jitter), as it did before 036. The setting is now written twice and publishes
  twice.
- **The board does not resume it.** That happens when the reset outlasts the run window
  (`GiveUp::OutlastsRunWindow`), or when the attempt history cannot be read. The pause now
  holds until the reset, where before 036 there was none. The limit is the account's, not
  this task's, so a task started before the reset would hit the same wall.
- **The CLI reported no reset time** (the fallback-poll case). There is nothing to note
  before `finish_run`, so the window remains open between the publication and the note at
  `resume_after`. This is a named residual. Closing it would mean the runner guessing the
  fallback poll, which is the board's decision.

Tests: `a_usage_limit_holds_new_starts_before_the_board_hears_the_run_finished`,
`the_pause_a_usage_limit_leaves_is_the_instant_the_board_chose_to_resume_at` and
`a_usage_limit_that_outlasts_the_run_window_still_holds_new_starts_until_the_reset`, in
`crates/core/tests/runner_board_port.rs`.

**Point 7: `ClaimTarget::Next` and `FreeCapacity` arrive with 042, not 036.** Point 7 says
"036 ships every method except `run_tool`", which read as 036 shipping every `ClaimTarget`
variant too. Before 042 the in-process body of `Next` could only be a refusal, and a variant
whose only behaviour is a refusal cannot be told apart from a bug. 036 ships `Run` and
`Plan`. 042 adds `Next` and `FreeCapacity` together with the board-side selection that gives
them a body. Adding a variant is not a trait signature change.

**Point 6: `Catalogue` keeps `deny_unknown_fields`, and 052 decides it.** Point 6 says no
DTO uses `deny_unknown_fields`. Every type in `board/types.rs` keeps that rule. One carried
core type does not: `Catalogue`, `CatalogueEntry` and `PlannerBudget` refuse unknown keys,
and that refusal is what turns a misspelled key in the stored setting into a warning
(`strategy/catalogue.rs`). In process the attribute costs nothing, because both ends are one
binary. Over HTTP it would make a newer board's catalogue unreadable to an older runner.
052 decides between a wire mirror of the catalogue and relaxing the attribute. 036 changes
neither, and gives the carried core types only the `Deserialize` derive, so their
`Serialize` output, which `src/types.ts` reads, is unchanged.

**Binds.** 041 (the pause moves to `runner.db`; both notes move with it), 042 (`Next`,
`FreeCapacity`), 052 (`Catalogue` on the wire), 056 (the residual window, if the outbox
changes when `finish_run` is heard).

### Amendment, 2026-10-10 — the machine port, beside the board port (task 041)

Point 14 keeps runner-owned state off the board port and says it is "read through
`ServiceContext` until 041 and 066 move them to `runner.db`". Task 041 moved it, and the
move needed a second port, because the rules for those facts stay in `rimaia-core` while
the queries that store them live in `rimaia-runner` (D33 point 2), and core never depends
on the runner (ADR-0027 point 6). What it is, so 042, 043, 046, 048, 059 and 066 build on
one shape:

- **The module is `rimaia_core::machine`.** `port.rs` defines `MachineStore`, the storage
  half of every runner-owned fact, object-safe with boxed futures (`MachineFuture`) for
  point 2's reason, held as `Arc<dyn MachineStore>`. Four groups of methods, each storage
  only: settings (get, set, clear a `runner_settings` key), checkouts (list, get, insert,
  patch, remove), worktree records (get, list, record, forget) and schedules (one method
  per write `schedule::` makes). A refusal the schema enforces is `Error::invalid` with the
  sentence `port.rs` names, in both implementations. `types.rs` defines `Checkout`,
  `CheckoutPatch` and `WorktreeRecord`; `Schedule` keeps its type in `db::models`.
- **`MachineContext { store, clock, changes, event_team }`** is the runner-side
  counterpart of `ServiceContext`: no pool, no team scope, no `MutationSource`. In solo
  `changes` is the board context's sender, and `event_team` is the solo team, read by
  nothing but `MachineContext::publish`, which builds every machine event under it. Each
  publish site names task 048, which replaces both fields with `LocalEvents`.
- **The rules stay where D3 put them** and take `&MachineContext` as their first argument:
  `scheduler::{state, pause, capacity}`, `schedule::window`, the `schedule::` CRUD,
  `db::settings::{run_environment, onboarding_dismissed, doctor_dismissals}` and their
  setters, `mcp::settings` and `worktree::cleanup::{auto_cleanup, set_auto_cleanup}`.
  039's board-context runner accessors are gone; `db::settings::{get_runner, set_runner,
  clear_runner}` are their machine-store replacements. Where a function still needs a
  board fact until 066, it takes the board context as a second argument and names it:
  `capacity::resolve(machine, board)` for each repository's cap, `doctor::run(machine,
  board, env)` for the repository list, `schedule::preview(machine, board, id)` for the
  plan. `run_task` and `plan_claimed` take `(board_port, machine, prepare_ctx, …)`, the
  board context held only for `worktree::prepare` until 044; the run's transcript stream
  is built with `EventStream::forwarding(clock, …)`, so it needs no board context either.
- **Two implementations, one contract.** `rimaia-runner` implements `MachineStore` on
  `RunnerStore` with checked queries: the production implementation, and the only one.
  `testing::machine::MemoryMachine` is core's in-memory implementation, for core's tests.
  `testing::machine_contract`'s `machine_store_contract!(Harness)` runs one suite over
  both, from `crates/core/tests/machine_store_memory.rs` and
  `crates/runner/tests/machine_store_sqlite.rs` (point 13's pattern), covering the
  `worktrees → checkouts` foreign key both ways, the primary keys, absent against empty
  settings and every nullable column. 043 adds `held_leases` methods and their cases here.
- **Built once in each place** (point 8's shape): `src-tauri`'s `setup()` puts the
  `RunnerStore` on `AppState.runner_store` and the context on `AppState.machine`, and
  hands the same context to `scheduler::build`, which now takes it; `testing::context`
  exposes `TestContext::machine()` over a `MemoryMachine` sharing the test's clock,
  channel and solo team, and `testing::teams::TwoTeams` carries one as `machine`.
- **Machine reactions to board actions** take `Option<&MachineContext>`:
  `tasks::{archive_task, archive_tasks, move_task}` and `review::{approve, reject}`. Given a
  machine they react on it (the on-archive policy, D20.3's auto-removal, reject's worktree
  removal before its transaction); with `None` the board write stands alone and an archive
  reports `Nothing`. The shell passes `Some(&state.machine)`; the MCP board tool passes
  `self.local.as_ref().map(|l| &l.machine)`. 046 point 3a carries these on
  `BoardRequest.machine`.
- **`LocalTools { machine, doctor, planner }`** is what the MCP server's local router
  reaches this machine through. `RimaiaServer`'s tools are two `#[tool_router]` blocks,
  `board_router` and `local_router`, combined with `+` (`RimaiaServer::tool_router`, which
  the anti-drift test iterates) when the host passes `Some(LocalTools)`, and the board
  router alone with `None`, where a local tool is an unknown tool. `mcp::build`,
  `RimaiaServer::new` and `RimaiaServer::scoped` take `local: Option<LocalTools>`, and the
  shell passes `Some` to both doors, so a run is offered all 22 local tools and refused
  them by `RunScope::authorize`. The server also takes the agent provider whose catalogue
  its board tools read, as its own `provider` field rather than off `LocalTools`, because
  the board router serves without a machine: it is point 9's `InProcessBoard` provider on
  the MCP side, read for the catalogue and nothing else, and 046 replaces it with D32's
  `ProviderProfile`.
- **Adoption.** `machine::adoption::read_board(ctx)` is the one board read 040's
  `machine_state` step makes, and `machine::adoption::machine_state` the pure mapping that
  skips a pathless repository and its worktrees and derives a missing root with
  `repo::default_worktree_root`, the function `register` uses. `adopt_board` takes the
  shell's `AppPaths` for that root. 065 deletes `read_board`.

**Binds.** 042 (the loop reads only through `MachineContext`, and adds the runner's
`max_turns` and `disallowed_tools` overrides as two more `runner_settings` keys), 043
(`held_leases` on the port and in the suite), 044 (removes `prepare_ctx`), 046 (point 3a;
its server passes `None` for `LocalTools`), 048 (replaces `changes` and `event_team`), 059
(converts the board reads the local handlers make through named core functions), 065
(deletes `read_board`), 066 (reads checkouts and worktree records through the port).

### Amendment, 2026-10-10 — the checkout and worktree half (task 066)

The amendment above moved the runner keys and the schedules; task 066 moved every reader of
the per-repository and per-task half, so no board query reads a retired column and no board
DTO carries an absolute path. What it settled, so 042, 043, 045, 046, 048, 054 and 056
build on one shape:

- **`machine::local`** holds the rules over checkouts and worktree records: `CheckoutView`
  (the local DTO `list_checkouts` and the per-repository setters answer), `not_set_up` (the
  one refusal every reader gives a board repository with no checkout here:
  `"<name>" is not set up on this computer`), `checkout_of`, `consented_repositories` (the
  set of repositories whose checkout has `unattended_consent`, which the queue reads once
  per pass and 042 sends as `ClaimTarget::Next.repositories`), and the writers
  `insert_checkout`, `patch_checkout`, `remove_checkout`, `record_worktree` and
  `forget_worktree`. Each writer announces itself through `MachineContext::publish`: a
  checkout as `Repositories([id])`, a worktree record as `Tasks([task_id])`, the wire names
  048's `LocalChange::{Checkouts, Worktrees}` keep.
- **The board's `Repository`** keeps `id`, `name`, `default_branch`, `created_at` and the
  team ceiling `allow_unattended_runs`, which is `#[serde(skip)]` until 045 names it and
  read by nothing. `RunContext::repository` is this pathless row.
- **Every function that touches the clone takes the checkout from the machine**:
  `repo::{remote_info, gh_status, path_problem, has_credential}` take a `Checkout`;
  `worktree::{prepare, status, diff_summary, remove, reconcile, local_path}` and
  `worktree::cleanup::{inventory, remove_worktree, remove_done_worktrees,
  remove_merged_worktrees}` take `&MachineContext`; `capacity::resolve(machine)` reads each
  cap off the checkouts and no longer takes a board context; `doctor::run` lists the board's
  repositories by name and checks only those with a checkout here;
  `repo::ensure_unattended_runs_allowed(machine, repository)` answers the checkout a run then
  uses, or refuses before any run state is written; `archive::run_on_archive(ctx, machine,
  task)` reads the policy and script off the checkout and the worktree off its record.
- **`worktree::prepare(ctx, machine, board, lease)`** records the path with
  `record_worktree` and the branch with `BoardPort::record_branch` under the lease, its
  first production caller. The two branch writes with no lease, the clear in
  `worktree::remove` when the branch is deleted and reconcile's clear when the branch is
  gone, share `worktree::clear_branch`, named for 054's `report_runner` and 043's per-runner
  reconcile.
- **Log paths are derived**: every reader computes `runner::events::transcript_path(paths,
  task_id, run_id)` (`runs::{list_runs, get_run, transcript_of, log_path,
  log_path_to_reveal, prune_logs}` and `startup::survey`). `outcome::insert_run` still
  writes the column, marked `-- runs.log_path written until 056`, and nothing reads it.
- **`repo::remove(ctx, Option<&MachineContext>, id)`**: the board removal first, refused
  before any write in either store; then, given a machine, every worktree record of the
  repository is forgotten and the checkout removed.
- **`review::digest(ctx, Option<&MachineContext>)`** reads consent the way the machine
  reactions do: `Some` from the shell and the solo MCP server; with `None`, no task is
  reported as skipped for consent, since a server cannot see a runner's consent before 045.
- **`repo::checkouts(ctx, machine)`** is what `list_checkouts` answers through both doors:
  this machine's checkouts of the repositories the caller's one team holds, so one machine
  holding two teams' clones tells neither about the other (ADR-0029 point 5). Its board fact
  comes from `repo::list`, a named read 059 converts.
- **The test that keeps it true** is `no_board_query_reads_a_retired_column`
  (`crates/core/tests/checkouts.rs`), which parses every entry of `crates/core/.sqlx/` and
  exempts only `-- machine_state adoption` and, for `log_path` in its text alone, the
  `start_run` insert.

**Binds.** 042, 043, 045, 046, 048, 054, 056, 059, 065.

### Amendment, 2026-10-10 — `ClaimTarget::Next` and the runner loop (task 042)

What task 042 pinned building `Next`'s body and moving the loop.

- **Point 2, the fields.**
  - `repositories` lists the repositories the runner has a checkout of *and* has given
    unattended consent for (`checkouts.unattended_consent`, read through `MachineContext`).
    Before task 045 that list is the whole opt-in: a repository is opted in exactly when the
    runner listed it, and a repository not in it is skipped as `UnattendedRunsNotAllowed`.
    Listing only consented checkouts keeps the board from offering a task the runner would
    refuse after the claim, a `release` into `failed`. The team ceiling is not read here;
    045 adds it beside this check, with its personal-team exemption.
  - `capacity` is what the runner has *free*, already net of its own in-flight runs. A
    `per_repository` of one means one more run, never a cap the board subtracts from again,
    and a listed repository missing from `per_repository` has no free slot.
    `scheduler::view::for_runner` is the one builder: `capacity::resolve` (D24's window
    override included) minus `InFlight::counts()`. The loop, the Runs view's plan and a
    schedule's preview all plan over its repositories.
  - `wait` is a `std::time::Duration`, serialised as integer milliseconds (`"wait": 0`). In
    process the body tries once, waits on `ServiceContext::subscribe` and
    `Clock::sleep_until` until a change event or the deadline, tries again, and returns
    `None` once the deadline has passed with nothing claimed.
  - The body is `board::service::claim`'s: `selection::plan` over the listed repositories,
    `selection::first_startable` against the capacity, then today's two routes, a lost race
    moving to the next entry. A `Next` claim returns at most one `Claim`, with trigger
    `Queued`.
- **Point 4, `wait: ZERO` and the deadline's solo source.** The solo loop always sends a zero
  `wait`. A waiting claim carries the free capacity of the moment it started and would sleep
  through a slot freed in another repository; dropping it to ask again is unsafe before 043,
  because the claim's two edges are two transactions and a claim dropped between or after
  them strands its task. So the loop keeps its own wake sources and each claim is a
  non-blocking try. A `Next` that found nothing carries no deadline, so a pass that ends
  idle asks the board for the earliest `resume_after` itself (`rimaia_runner::queue::solo`'s
  `SoloBoard::next_deadline`, judged at the instant the board was last asked, so a retry
  that came due between the claim and that read wakes the loop at once). The long poll, a
  `wait` above zero from a production caller, the clamp, the `earliest_due` wake and
  cancelling a claim that has not committed are 053's.
- **Point 8.** The loop is `rimaia_runner::queue`, and `build` is
  `rimaia_runner::queue::build(machine, board, changes, solo, in_flight, paths, runner)`:
  the `MachineContext`, the port, the board's change receiver, `SoloBoard` and the slot
  registry. It takes no bare board `ServiceContext`; the four board reads it still makes
  without the port (the deadline above, the Runs view's plan, the doctor and the fire-time
  preflight log) and the context `run_task` keeps for `worktree::prepare` until 044 are
  `SoloBoard`'s, in the one file 058 and 059 replace. `rimaia_core::scheduler::queue` is
  gone and nothing re-exports it.
- **Point 13.** 042 owns the `Next` cases, its race included:
  `two_runners_claiming_next_for_one_task_get_exactly_one_claim`. 043's "two runners racing
  for one claim" case is the lease form of that one.

**Binds.** 043, 045, 052, 053, 058, 059.

### Amendment, 2026-10-10 — what task 043 decided

Task 043 made the lease real, in process, before a network is in front of it. None of this
changes point 2's trait.

- **`tasks::run_state::transition(conn, clock, id, from, to)`** is the one conditional write of
  `run_state`: it checks the edge, runs `UPDATE … WHERE id = ? AND run_state = ?from` inside the
  caller's transaction, and answers whether a row moved. It never commits or publishes.
  `set_run_state` is `BEGIN IMMEDIATE`, read, `transition` from the value it read, commit,
  publish, with its messages unchanged. `tasks/run_state.rs` stays the only file that writes the
  column.
- **The claim is one transaction** (`board::lease::claim`): `BEGIN IMMEDIATE`, the task re-read,
  `eligible`, the edges through `transition`, `lease_generation + 1`, the `runner_leases` row,
  one commit, one `ChangeEvent`. The edges: `idle`, `failed` or `cancelled` → `queued` →
  `running` for a fresh start; `queued` → `running` for a task with no lease row (only a build
  older than 043, and task 057's `release_pin`, leave one there); `waiting_retry` → `running` for
  a resume; none for `Plan`. A task that already has a lease row, or whose state no longer has
  the edge, is lost (`Ok(None)`), and a lost `Next` moves on to the next entry. Everything the
  claim returns (the context, the team, the resume point) is read before the transaction opens,
  because the transaction reads nothing over the pool.
- **The purposes.** `Plan` is `strategy`. A resume is the purpose of the kind that was waiting
  (D29 point 3). A fresh start is `strategy` when `needs_planning(task, effective.mode)` holds,
  which is ADR-0016's inline planner (ADR-0031 point 1), and `implementation` otherwise.
  `start_run` moves the purpose to the run's kind and sets `run_id` under the same generation
  (D29 point 1). `run_task` runs the inline planner exactly when `Claim::purpose` is `strategy`
  and derives nothing itself.
- **The fence, `board::lease::current`,** runs inside the transaction of the first write each
  lease method guards (`run_context`, which writes nothing, is fenced in a transaction of its
  own and then read). A task that does not exist or is outside the lease's team is `NotFound`; a
  task with no lease row, or another generation or runner, is `Conflict` (D8's 043 amendment).
  `finish_run` is fenced in both of its transactions: the one that closes the row (with "already
  finalized" kept after the fence, point 12) and the one that lands the task. The attempt
  history it decides `resume_after` from is read before the first opens.
- **`publish_tail` is not fenced in process.** It is synchronous and the fence is async, and D14
  makes a stale tail worth nothing. Task 052's async handler drops a tail whose lease `current`
  refuses.
- **`release` is keyed on the run state, not the purpose.** In one transaction: the fence, a
  task still `running` taken to `failed`, the lease deleted. A `Plan` claim's task is not
  `running`, so its run state is left alone; an inline planner's lands `failed`.
- **`finish_run` lands the task, the lease and the pin in one transaction** (`lease::land`,
  over `outcome::land_within`, which also makes the column move and the run-state write one
  transaction for every caller). `Released` deletes the lease. `Continue` keeps it after
  `eligible(conn, task, holder, next)` for the next phase's purpose; a refusal answers
  `Released` and lands the task in `in_review`, as a finish that does not continue lands it.
  Tasks 067 and 045 add to that one call, never a second. A `Continue` publishes nothing,
  because nothing visible changed.
- **Eligibility is one function, `board::lease::eligible(conn, task, runner, purpose)`.** Its
  one rule in 043 is pinning: a task pinned to another runner is not this runner's, for every
  purpose. `selection::plan` takes a `RunnerView` (the runner's id, its `ProviderId` for 067,
  and its listed repositories) and leaves an ineligible task out before positions are
  numbered, with no `SkipReason`, so `status_with_plan` and `claim(Next)` agree; the claim
  transaction asks again. `claim(Run)` and `claim(Plan)` refuse it as `Invalid`: "this task is
  pinned to <label>, which has its worktree and the agent's conversation. Only that runner can
  run it until someone chooses to run it elsewhere."
- **The pin and its three writers.** Only `board::lease` writes `tasks.pinned_runner_id`. In
  043, `finish_run` sets it to the holder when the closed run is `interrupted` or the task lands
  `waiting_retry`, and clears it when the pinned runner's finish lands anywhere else. A
  `Continue`, `give_up`, a cancel, an edit and a move leave it. Task 053's expiry and task 057's
  `release_pin` are the other two writers.
- **`LeaseTerm::{Never, Renewable(d)}` and `LEASE_LIFETIME`** (three minutes) live in
  `board/lease.rs` and nowhere else. `InProcessBoard::new` takes the term; the solo host passes
  `Never`, which writes `expires_at` NULL. The heartbeat renews, in one transaction, each listed
  lease current for this runner to `now + d` (a `Never` lease stays NULL) and answers the rest in
  `fenced`; `cancel` stays empty in process. It has no production caller until 053.
- **The harness.** Point 13's `Harness` gains `start_with(term)`, and `start()` is
  `start_with(LeaseTerm::Never)`. The in-process harness serves a board in a `TempDir` file over
  `db::connect`'s multi-connection pool (`TestContext::over_file`), so the race between the two
  runners is real. 052's HTTP harness inherits the requirement.
- **`held_leases` on `MachineStore`**: record (replacing a task's earlier record), set run and
  purpose, forget, list, with their cases in `machine_store_contract!`. The rules are
  `machine::leases`. Every starter records after its claim returns and before it spawns
  (`runner::start::record_claim`, from the manual starter, `claim_for_planning` and the loop's
  `try_step`); `run_task` notes the run after `start_run`, notes no run and the next purpose
  after a `Continue`, and forgets the lease on a `Released` finish or a release whose answer
  says the board holds no such lease.
- **Per-runner reconcile** (`scheduler::reconcile`). `reconcile_held(board, machine)` settles
  this runner's records through the port: an open run finished as interrupted (which pins it),
  no run released, a run the board already closed released, and a record the board answers
  `Conflict` or `NotFound` dropped untouched. Then **the solo arm**,
  `reconcile_unrecorded(ctx, runner_id, held)`: leases the board holds for this runner that its
  record does not (a crash between the claim's commit and the record), and tasks in `running` or
  `queued`, or with a run still open, that no lease names (a build older than 043). **Team mode
  has neither set**: the first expires on the server (053), the second cannot be written once the
  claim writes the edges and the lease together, and 065 is where its query can go. Then
  `worktree::reconcile`, never before the lease steps. `ReconciliationReport` loses
  `tasks_left_running`, and the shell's two steps are named "reconcile the leases this runner
  held" and "reconcile runs no lease recorded" (D11).

**Binds.** 044, 045, 052, 053, 055, 056, 057, 058, 060, 061, 065, 067.

---

## D32 — One command registry: board and local commands, one dispatcher, one caller

**Question.** ADR-0034 classifies every command as board or local. It serves board commands at
`POST /api/v1/<name>` as well as through `invoke`, and says `check-command-wiring.sh` checks
the classification. Five things are left open:

- where the classification is written down;
- how one list can feed four consumers at once: a Tauri macro that accepts only literal paths
  (`generate_handler!`), an axum router built at runtime, a TypeScript module and a bash
  script;
- what a handler receives where it receives `State<AppState>` today;
- what an HTTP request carries in place of the in-process trust the shell relies on;
- which of today's 97 commands is which.

Tasks 046, 047, 049 and 059 all depend on this seam. Without one answer, each would keep a
list of its own.

**Decision.** Nine points. Task 046 carries all of them unless a point names another task.

1. **The registry is one table in `rimaia-core`, in `crates/core/src/api/registry.rs`.** It
   lives in a new `api` module. Core already depends on `axum` and `serde_json`, so the module
   needs no new crate. There is no `futures` crate either: `BoxFuture` is a local alias,
   `Pin<Box<dyn Future<Output = T> + Send + 'a>>`, in `api/mod.rs`.

   ```rust
   pub struct Command {
       pub name: &'static str,
       pub kind: Kind,
   }

   pub enum Kind {
       /// Served by `api::dispatch`: over HTTP by `rimaia-server`, in process by the solo shell.
       Board { effect: Effect, handler: BoardHandler },
       /// Served by the desktop shell's `generate_handler!` list, and by nothing else.
       Local,
   }

   #[derive(Debug, Clone, Copy, PartialEq, Eq)]
   pub enum Effect {
       /// Changes no stored state and publishes no `ChangeEvent`.
       Read,
       /// May do either, including recording a request for a runner to claim.
       Write,
   }

   pub type BoardHandler =
       Box<dyn Fn(BoardRequest, Value) -> BoxFuture<'static, Result<Value>> + Send + Sync>;

   pub static COMMANDS: LazyLock<Vec<Command>> = LazyLock::new(|| {
       vec![
           board("create_task", Write, tasks::create_task),
           board("get_task", Read, tasks::get_task),
           // …
           local("get_app_info"),
           local("debug_provoke_error"),
           // …
       ]
   });

   pub fn find(name: &str) -> Option<&'static Command>;
   ```

   `board(name, effect, f)` erases any `f: Fn(BoardRequest, A) -> impl Future<Output =
   Result<O>>` with `A: DeserializeOwned` and `O: Serialize`. The erased handler does three
   things:

   - deserializes the argument object into `A`. A failure is `Error::invalid`, naming the
     command;
   - awaits `f`;
   - serializes `O`.

   `local(name)` records the name and nothing else. `Effect` exists on board rows only.

   A handler is therefore an ordinary typed `async fn`:

   ```rust
   // crates/core/src/api/board/tasks.rs
   pub async fn get_task(request: BoardRequest, args: ById) -> Result<TaskDetail> {
       tasks::get_task(&request.ctx, &args.id).await
   }
   ```

   Board handlers live under `crates/core/src/api/board/`, with one file per shell module
   they come from: `analytics.rs`, `repositories.rs`, `runs.rs`, `settings.rs`, `strategy.rs`
   and `tasks.rs`. The wire inputs move with them: `NewTaskInput`, `TaskPatchInput` with
   `opt_patch`/`to_patch`, `TaskFilterInput`, `NewTaskLinkInput`, `TaskLinkPatchInput`,
   `UpdateRepositoryInput` and `RunFilterInput`.

   **Their serde attributes do not change**, because the invoke payloads they describe are
   what 24 frontend test files assert on. A top-level argument struct is
   `#[serde(rename_all = "camelCase")]`, which gives exactly the key Tauri derives from a
   parameter name today. It ignores unknown keys, as Tauri does.

2. **A board handler receives a `BoardRequest`, never `AppState`.**

   ```rust
   // crates/core/src/api/mod.rs
   #[derive(Clone)]
   pub struct BoardHost {
       /// The unscoped base. `dispatch` never hands it to a handler as it is.
       pub context: ServiceContext,
       pub provider: ProviderProfile,
   }

   #[derive(Clone)]
   pub struct BoardRequest {
       /// `host.context.for_caller(&caller)`: scope, actor and source already set.
       pub ctx: ServiceContext,
       pub caller: Caller,
       pub host: BoardHost,
   }

   pub async fn dispatch(host: &BoardHost, caller: Caller, name: &str, args: Value)
       -> Result<Value>;
   ```

   `dispatch` is the only way a board handler runs. It:

   - looks the name up. A name that is not a board row is `Error::not_found`;
   - reads `null` arguments as `{}`;
   - builds the request;
   - opens one `command` span recording `name`, the door and `user_id`. **It never records
     the arguments or the output**, because they carry plans (ADR-0037 §6).

   `ProviderProfile` is new, in `crates/core/src/runner/provider/mod.rs`. It holds `id`,
   `display_name`, `inherit_cost_usd` and `default_catalogue`, copied out of an
   `AgentProvider` by `ProviderProfile::of`.

   - **The board sees a provider's profile, never the provider.** Nothing on the board side
     can spawn a process.
   - **`fanout_noun` is deliberately absent.** ADR-0031 §1 keeps prompt composition, and the
     vocabulary it uses, on the runner.
   - **Two functions change signature.** `strategy::catalogue::catalogue` and
     `runner::outcome::observed_run_cost` take the profile instead of `&dyn AgentProvider`.
   - **The runner protocol never reads `BoardHost.provider`.** A claim's catalogue is the
     claiming runner's provider's, named by the `ProviderId` the runner sends (D31 point
     10), because one server serves runners of more than one provider.
   - **`BoardHost` gains a field only through the task that needs it.** For example, 048
     adds the tail relay.

3. **Board commands are dispatched through the registry, not registered with Tauri.** The
   solo shell installs one composite invoke handler:

   ```rust
   // src-tauri/src/lib.rs
   let builder = builder.invoke_handler(commands::board::route(tauri::generate_handler![
       commands::app::get_app_info,
       commands::app::reveal_app_data_dir,
       #[cfg(debug_assertions)]
       commands::app::debug_provoke_error,
       // … every local row, and only local rows
   ]));
   ```

   `commands::board::route` is new, in `src-tauri/src/commands/board.rs`. It wraps the
   closure `generate_handler!` produces:

   - **For a name that `registry::find` reports as board,** it takes the JSON from
     `invoke.message.payload()` and the `SoloBoard { host, caller }` from `AppState.board`.
     It answers with `invoke.resolver.respond_async(dispatch(…))`. An error is serialized to
     the same `{ code, message }` that a `#[tauri::command]` returning `Result<_, Error>`
     produces today.
   - **Any other name goes to the `generate_handler!` closure.** That closure returns `false`
     for a name it does not know, which Tauri reports as not found, as it does today.
   - **`AppState.board` is `Some` in solo and `None` when connected.** When connected, a
     board name is refused with `Error::invalid`, saying the command is sent to the server. A
     transport bug then fails on the first click, instead of writing to a board nobody reads.

   **Invoke names and payloads do not change.** `commands.ts` still sends `invoke("get_task",
   { id })`. The 31 test files that mock `@tauri-apps/api/core` keep passing without edits.

   **The server mounts one route per board row, by iterating `COMMANDS`.**

   - **Route:** `POST /api/v1/<name>`, nested under `/api/v1` with its own JSON fallback. An
     unknown name or a local name gets `404 {"code":"not_found",…}` and never falls through to
     the web bundle's `index.html` (ADR-0034 §6).
   - **Request:** the body is the same object `commands.ts` passes as `invoke`'s second
     argument, byte for byte. Its keys are `camelCase`, not MCP's `snake_case` (D16.1). An
     empty body is `{}`. The route reads the body as bytes and parses it itself, so a missing
     `Content-Type` or malformed JSON comes back as `invalid` in the error shape, not as
     axum's plain-text rejection.
   - **Response:** `200` with the output as JSON. A unit output is `null`, not `204`, so the
     transport parses every success the same way.
   - **Errors:** the body is always `Error`'s `{ code, message }` (D8, ADR-0034 §2). The
     status code exists for proxies, logs and metrics. The frontend branches on `code` and
     nothing else.

     | `code` | Status |
     | --- | --- |
     | `invalid` | 400 |
     | `unauthenticated` | 401, with `WWW-Authenticate: Bearer` |
     | `not_found` | 404 |
     | `conflict` | 409 |
     | `upgrade_required` | 426 |
     | `database`, `io`, `internal` | 500 |

   **There is no 403.**

   - An entity outside the caller's teams does not exist, so it is `not_found` (ADR-0029 §5).
   - A member attempting an owner's action gets `invalid`, with a sentence naming the role
     that action needs. That is a presentation choice, which is what D8 says a code is for.

   **Three `ErrorCode` variants arrive with this work.** 046 adds `unauthenticated` and
   `upgrade_required`, and 043 adds `conflict`. Each one is a matching edit to `ErrorCode` in
   `src/types.ts`.

4. **Local commands keep `#[tauri::command]`, in one `generate_handler!` list.**
   - **The two `cfg`-selected lists in `src-tauri/src/lib.rs` become one.** The single
     debug-only entry carries `#[cfg(debug_assertions)]`. The locked tauri-macros 2.6.3
     parses outer attributes on each entry (`CommandDef::parse` calls
     `Attribute::parse_outer`) and emits them on the match arm.
   - **The list stays literal, and a release build has nothing to forget.**
   - **The gating rule is unchanged.** Only a `debug_`-prefixed name may be gated.

5. **A check script keeps the lists in sync, and nothing is generated.**
   `scripts/check-command-wiring.sh` reads four files and fails on any disagreement between
   them. It stays bash 3.2-safe and uses no `jq`. It fails closed when a file no longer has
   the shape it parses. It checks:

   - **The registry** (`crates/core/src/api/registry.rs`). It collects every `board("<name>",
     Read|Write` and every `local("<name>")`, matching across the whole file with line
     comments stripped, so a row that rustfmt wraps cannot hide. A name that appears twice
     fails, and so does a file with no rows at all.
   - **The handler list** (`src-tauri/src/lib.rs`). There must be exactly one
     `generate_handler![` block. An entry under a `#[cfg(debug_assertions)]` line must be
     `debug_`-prefixed. The list must equal the local rows, in both directions. A board name
     in the list fails, because board commands have exactly one path.
   - **The definitions** (`src-tauri/src/commands/*.rs`). Every `#[tauri::command]` function
     must be in the handler list, so no command is defined without being classified. The
     existing `debug_` prefix rule for gated definitions stays.
   - **The wrappers** (`src/lib/commands.ts`). Each command name must appear in exactly one
     `board<T>("<name>"` or `local<T>("<name>"` call (point 6), and the function used must
     match the row's kind. Every row must have its wrapper. No `call<` may remain.
   - **The protocol version.** The `PROTOCOL_VERSION` literal in `commands.ts` must equal
     `rimaia_core::api::PROTOCOL_VERSION` (point 7).

   The HTTP half is not a job for bash, because the routes are built from the registry at
   runtime. `crates/server/tests/commands.rs` holds one case per board row: a call against a
   two-team fixture. Its tests are:

   - **`every_board_command_has_a_case`.** This is ADR-0029 §5's registry test. A board row
     added without a cross-team case fails it.
   - **`every_board_command_has_a_route_and_no_local_command_does`.** A board name sent with
     no credentials gets `401 unauthenticated`, which proves the route exists and
     authentication ran. A local name gets `404 not_found`.
   - **`a_team_cannot_see_another_teams_ids`.** Every case is run as team A against team B's
     ids, and every answer must be `not_found`.
   - **`both_transports_answer_every_case_identically`.** Each case runs through `dispatch` in
     process and through the route over a loopback listener with `reqwest`, and the two
     answers are compared as JSON. `reqwest` is already a dependency, so no crate is added.
     This is how 046 meets ADR-0034's requirement that the two transports behave
     identically. Below the transport they share one function, so only the transport layer
     needs testing.
   - **`reads_publish_no_change_event`.** Every `Read` case must leave the change channel
     empty.

6. **`commands.ts` records each command's kind through the function its wrapper calls.**
   - **The private `call<T>` splits in two:** `board<T>(command, args?)` and
     `local<T>(command, args?)`.
   - **In 046 both send through 028's `CommandTransport` exactly as `call` does,** so the
     split lands with no change in behaviour.
   - **049 gives `board` its HTTP transport and `local` its browser refusal:** `{ code:
     "invalid", message: "… is only available in the desktop app" }`. The one exception is
     `get_client_capabilities`, which the browser transport answers itself (ADR-0034 §5).
   - **The module's existing role is unchanged.** `commands.ts` stays the only module that
     imports `invoke`, and becomes the only module that sends a command over HTTP.

7. **Every board request carries a `Caller`, from the first route onward.**

   ```rust
   // crates/core/src/api/caller.rs
   #[derive(Debug, Clone, PartialEq, Eq)]
   pub struct Caller {
       /// Who: `users.id` (ADR-0030 §8). Solo: the installation's solo user (D28).
       pub user_id: String,
       /// Which teams, with the caller's role in each. Read from `team_memberships` on this
       /// request; never cached on a session or a token (ADR-0030 §2).
       pub teams: Vec<TeamGrant>,
       /// Which door the request came through.
       pub door: Door,
   }

   #[derive(Debug, Clone, PartialEq, Eq)]
   pub struct TeamGrant {
       pub team_id: String,
       pub role: Role, // the `team_memberships.role` enum task 038 adds
   }

   #[derive(Debug, Clone, PartialEq, Eq)]
   pub enum Door {
       /// The solo desktop's own window, through `invoke`. No credential (ADR-0030 §7).
       Shell,
       /// A browser session: the `sessions` row id, never the cookie value.
       Browser { session_id: String },
       /// A connected desktop's `rmd_` token: the token row id, never the secret.
       Desktop { token_id: String },
       /// An `rmp_` token on the hosted `/mcp`; `None` on the solo loopback endpoint.
       Mcp { token_id: Option<String> },
       /// A paired runner's `rmr_` token.
       Runner { runner_id: String },
   }
   ```

   **Mapping to ADR-0019's source.** `Door::source()` maps `Shell`, `Browser` and `Desktop` to
   `MutationSource::Ui`, `Mcp` to `Mcp`, and `Runner` to `System`. `tasks.source` therefore
   keeps ADR-0019's three values, and the finer-grained door goes to the span.

   **One place turns a caller into a context:** `ServiceContext::for_caller(&caller)`. It sets
   `source`, plus the scope and actor fields that task 038 adds (ADR-0029 §5, ADR-0030 §8).

   **A handler that needs the door reads it in core.** For example, an account command that
   must know which session is asking reads `request.caller.door` in its core handler, never
   in a transport.

   **The solo shell's caller comes from `Caller::solo(…)`.** It is built over the solo team
   and user ids that task 038 records (D28): `Door::Shell`, the solo user, and the solo team
   with the owner role.

   Resolving a caller is split between the crates by what each owns:

   - **`rimaia_core::api::caller::Authenticate`** has one method: `fn authenticate<'a>(&'a
     self, credential: Credential<'a>) -> BoxFuture<'a, Result<Caller>>`. `Credential` is
     either `Session { secret, csrf }`, where `secret` is the cookie's value and only its
     hash is stored (D28), or `Bearer(token)`.
     - 046 ships `RefuseAll`, which is the only production implementation until 047.
     - 046 also ships `testing::api::FixedCaller`, behind the `testing` feature.
     - 047 implements `Authenticate` over sessions and hashed tokens.
   - **`crates/server/src/caller.rs`** holds `impl FromRequestParts<ServerState> for Caller`.
     The orphan rule allows it because `ServerState` belongs to the server crate.
     - It reads either the `rimaia_session` cookie with the `X-Rimaia-CSRF` header, or
       `Authorization: Bearer`. It refuses a request that carries both.
     - Its cookie half lands with 047, which is when a session can first exist and when D34
       admits `axum-extra`. Until then a cookie alone is `unauthenticated`.
     - Every board route takes it as its first extractor, and there is no other way to mount
       a board route. No route can therefore exist without a caller.

   This entry fixes which doors each surface accepts. Any other door is `unauthenticated`:

   | Surface | Accepts | Owner |
   | --- | --- | --- |
   | `POST /api/v1/<board command>`, `GET /api/v1/events` | `Browser` (CSRF header on every request), `Desktop` | 046, 048 |
   | `/api/v1/runner/<name>`: its own table, not this registry (ADR-0034 §2) | `Runner`, through a `RunnerCaller(Caller)` newtype | 052 |
   | `/mcp` on the hosted server | `Mcp` | 060 |
   | The solo shell's `invoke` | `Shell`, built in process. Solo has no HTTP door | 046 |

   A session's CSRF token is compared using `subtle`'s constant-time equality. How the web
   app obtains the token is 047's decision.

   **Every client sends `Rimaia-Protocol`, the browser included.**

   - **The value** is `rimaia_core::api::PROTOCOL_VERSION`. `commands.ts` carries a copy,
     which the script compares against it.
   - **For a version the server does not support** (ADR-0037 §4), a `Read` board command is
     answered and a `Write` board command gets `upgrade_required`. ADR-0037 says the server
     "answers board reads … refuses claims and reports". Board writes are put on the refusing
     side, so the rule fails closed.
   - **The browser is not exempt.** A tab left open across a deploy keeps running the old
     bundle against the new server. For the browser, `upgrade_required` means reload the page
     (050).

8. **A row's kind says where the command is served today. The appendix says where it ends
   up.**
   - **Eight commands are not yet served where they will end up.** They are board commands
     in ADR-0034's sense, but they still spawn, cancel or read on this machine: Run now,
     retry, cancel, the run tail, the three transcript reads, and registering a repository
     by path. The three planning commands spawn here too, and stay `local` for good (the
     2026-10-04 amendment on the planning commands, below).
   - **They enter the registry as `local` rows served by the shell.** The task in the
     appendix's *From* column flips each one to `board`.
   - **A flip is one commit.** That commit also moves the handler into `api/board/` and
     switches the wrapper to `board<T>`. The script fails any commit that does one of these
     without the others.
   - **A task that adds a command before 046 appends a row to the appendix in the same
     commit.** The row gives the name, the kind, and the ADR point that decides it, so 046
     can migrate it without making a judgement of its own. From 046 onward the registry is
     the list, and the appendix is history.

   **A local handler never reads the board's `ServiceContext`.** When it needs board state it
   goes through the runner's `BoardPort` (D31) or dispatches a board command:

   - in solo, in process, with the solo `Caller`;
   - when connected, over HTTP with the desktop token (059).

   A connected machine has no board database of its own. A handler that reached for the board
   pool would not fail. It would answer from a stale file.

9. **What the registry does not record.**
   - **No MCP pairing.** 20 of today's 45 board commands have no MCP tool:
     - `delete_task`, deliberately (ADR-0021 §5);
     - `retry_task_now`, deliberately (D23);
     - 18 others, which are ADR-0021 point 1 defects that predate this entry.

     The pairing is not mechanical. `get_worktree_inventory` is served by the
     `list_worktrees` tool, and `set_task_strategy` is a tool with no command. ADR-0021 also
     rejects deriving one surface from the other. Parity on the hosted server is 060's job.
   - **No runner protocol.** `/api/v1/runner/*` belongs to ADR-0031. It is authenticated only
     by runner tokens, and ADR-0034 §2 keeps it out of the UI's command list.

**Why.**

**Points 1 to 3: one dispatcher, instead of 48 Tauri wrappers with names of their own.**

- **Named wrappers would declare every wire shape twice.** Each would restate the parameter
  list that the HTTP route also deserializes, for 48 commands.
- **The transports would share only the service call.** They would not share a deserializer
  or a serializer, which is exactly where two transports drift apart. With `dispatch` behind
  both, identical behaviour follows from the structure, and the contract test only has to
  cover the transport.

**Why a composite handler rather than one catch-all `board` Tauri command:** it keeps invoke
names and payloads as they are. The 24 test files that assert
`toHaveBeenCalledWith("update_task", …)` pass without edits, which is ADR-0034 §4's "nothing
else changes".

**Every Tauri API it needs is public in the locked 2.11.5:**

- `Builder::invoke_handler` accepts any `Fn(Invoke<R>) -> bool` (`src/app.rs:1658`).
- `InvokeMessage::command`, `payload` and `webview` are public, and so is
  `InvokeResolver::respond_async` (`src/ipc/mod.rs`).
- `generate_handler!` expands to a closure that returns `false` for an unknown name. That is
  what allows it to be wrapped.

**Point 2: the provider profile.**

- **The server needs a provider's catalogue anyway.** ADR-0031 §1 has it resolve the
  effective strategy for each claim, which needs the catalogue.
- **It must not hold the provider itself.** A profile gives it the data and nothing it could
  spawn or compose with.

**Point 4: one handler list.** The two `cfg`-selected lists exist only because the comment
above them assumed `generate_handler!` could not gate a single entry. The locked macro can.
The drift the script's first assertion was written to catch cannot happen at all when there is
one list.

**Point 5: a check script rather than a generated file.**

- **A generator needs an input it can read at build time.** That means one of two things:
  - a build-dependency on `rimaia-core`, which compiles the crate a second time for the
    host, every `query!` macro included;
  - or a data file that Rust, TypeScript and bash all parse: a fourth format added to the
    three that already exist.
- **Generated code is not reviewed.** A generated file committed to the repository also needs
  a freshness check, and that check would be a script.
- **The script already fits.** It exists, already parses `lib.rs`, and already runs in CI.
  ADR-0015 names it as the stand-in for the end-to-end test that would catch a missing
  registration.
- **What the script guards is now small.** With board commands out of `generate_handler!`, the
  literal list is 60 names at 046 and 52 once 056's flip lands, before the local commands
  later tasks add.

**Point 7: a caller on every route from the start.**

- **A missing caller cannot compile.** ADR-0029 §5 wants a missing scope to be a compile
  error. Here, a route without a caller cannot be mounted at all.
- **The alternative is a retrofit.** If 046 mounted routes and 047 added authentication, 047
  would have to edit every route, and any route it missed would serve every team's board to
  anyone.
- **The interim server fails closed.** With `RefuseAll`, a server built between 046 and 047
  refuses everything. ADR-0032 and ADR-0037 take the same fail-closed direction.
- **A 403 would be the forbidden probe.** Having no 403 carries ADR-0029's not-found rule
  through to the status line. A 403 meaning "this exists, but not for you" is exactly the
  probe that rule forbids.
- **Door names are pinned here.** The cookie, header and door names are fixed in this entry
  because the server (047) and the frontend (049, 050) must agree on them.
- **CSRF is checked on every cookie request, reads included.** ADR-0030 only requires it for
  requests that change something. But every command is a `POST` and the web app sends the
  header anyway. Checking it everywhere means no route can be the one that was forgotten.

**The `Effect` column.** ADR-0037 §4's skew rule needs to tell reads from writes. Every route
is a `POST`, so the HTTP method cannot carry that distinction.

**Point 8: kind says where the command is served today.**

- **An early board classification would be dishonest.** Suppose the registry marked Run now
  `board` at 046. The server would mount a route with nothing honest behind it, because the
  only implementation spawns a process in the shell. A connected desktop would then send Run
  now to a server that cannot start anything.
- **Flipping in the task that builds the mechanism keeps the registry true at every commit.**
  The script makes each flip atomic across Rust, the handler list and TypeScript.
- **The local-handler rule guards against the one failure no test sees.** A handler that reads
  the board pool compiles, passes every solo test, and then answers from a board that is not
  there.

**Point 9: no MCP pairing.** A mechanical pairing is the alternative that ADR-0021 considered
and rejected.

See also [ADR-0034](adr/0034-one-api-for-the-web-and-the-desktop.md), which this entry
implements in full; ADR-0029 §5 (scope at the edge, the registry test); ADR-0030 §2–§6
(credentials); ADR-0037 §4 (protocol skew); ADR-0021 §1 and §5 (parity and its exceptions); D7
and D8, which this entry extends to HTTP; D16.1 (MCP's casing); and D20.6, D23 and D25 (the
recorded no-tool commands).

**Binds.**

- **046** carries all nine points. It also rewrites two pieces of text that describe two
  lists: the script's line in CLAUDE.md's command list, and the comment above the handler
  list in `lib.rs`.
- **033–045 and 066:** any command one of them adds is appended to the appendix (point 8).
- **041 and 066:** follow the local-handler rule as the 2026-10-04 amendment below states it.
  066 takes `worktreeRoot` out of `update_repository`.
- **045:** `set_repository_unattended_runs` stays the runner's consent. The team ceiling
  becomes a new board row.
- **047:** implements `Authenticate`, with the cookie, header and CSRF rules above.
- **048:** flips `get_run_tail`, adds the tail relay to `BoardHost`, and puts `Caller` on
  `/api/v1/events`.
- **049:** builds the transports behind `board` and `local`.
- **050:** reloads the page on `upgrade_required`.
- **051:** role refusals are `invalid`, and team commands are board rows.
- **052:** flips the three run controls and adds `RunnerCaller`.
- **054:** splits `register_repository`.
- **056:** flips the transcript reads.
- **059:** sets `AppState.board` to `None` when connected, and local handlers reach the board
  over HTTP, including through the core read functions 041 and 066 left them.
- **060:** keeps the planning commands `local` and adds `request_task_strategy` and
  `request_tasks_strategy` (the amendment on the planning commands, below), and `/mcp`
  builds its callers through `Authenticate`.
- **062:** `/healthz` and `/metrics` are not board routes and take no `Caller`; every board
  route still does.
- **064:** the final docs pass.

### Amendment, 2026-10-04 — the local-handler rule before 059 (tasks 041 and 066)

Point 8 says a local handler never reads the board's `ServiceContext`. Before 059 there is
no remote board for one to reach, and before 046 no dispatcher, so 041 and 066 cannot follow
it to the letter. What they follow instead:

- **No handler they add or rewrite, Tauri command or MCP tool, issues a board query of its
  own.** No `sqlx` call against the board pool appears in a local handler.
- **Board facts come through named `rimaia-core` read functions over `AppState.context`.**
  Each such call is listed in that task's PR.
- **059 converts them**, when it sets `AppState.board` to `None` and local handlers reach the
  board over HTTP. Until then each call answers from the one board that exists, which is
  current.

Why: the stale-board failure point 8 guards against first becomes possible when 059 makes a
desktop connected. Routing through named functions now leaves 059 a list to convert rather
than a search. 046 does not own this: its Out of scope hands the conversion to 059.

### Amendment, 2026-10-04 — the planning commands stay local (task 060)

The appendix first marked `plan_task_strategy`, `plan_tasks_strategy` and `cancel_plan_pass`
"board, 060". They are `local` rows for good. 060 adds two board rows of its own instead,
`request_task_strategy` and `request_tasks_strategy`, over its request service.

- **Starting a planner is local; recording a request is board.** ADR-0034 point 1 classifies
  a command by what it touches. The three commands claim on this machine's runner, spawn a
  planner, and report a pass the window watches. A request writes two board columns that
  some eligible runner acts on later.
- **Flipping the names would break solo.** Plan now would become a request that a stopped
  queue never serves, 023's watched pass would become N unwatched claims, and the board
  would gain a `cancel_plan_pass` with nothing to cancel. ADR-0035 point 6 says that in solo
  they "still start the planner locally".
- **MCP keeps ADR-0035's names.** The hosted `/mcp` serves `plan_task_strategy` and
  `plan_tasks_strategy` over the request service. A command name is one row of one kind, so
  the command side needs names of its own.

Counts, restated: of today's 97 commands, 45 are board (37 from 046, 8 flipped later) and 52
are local. `cancel_plan_pass` was one of point 9's 19 defects, so 18 remain. 050's browser
gates on the three planning commands stay. A browser Plan button calls the request commands
instead (061).

**Binds.** 046 and 050 (no planning flip, no planning gate deleted), 060, 061, 064.

### Appendix — today's 97 commands, classified

The table follows the order of the handler list in `src-tauri/src/lib.rs` (lines 394–593 on
`main` @728a049). The columns mean:

- **Kind** is ADR-0034's classification.
- **Effect** applies to board rows only.
- **From** is the task whose commit makes the registry row that kind. A row marked "local
  until then" is served by the shell as a `local` row until that task flips it (point 8).

There are 45 board commands: 37 from 046, and 8 flipped later. There are 52 local commands.
060's two request rows follow the planning rows; they are not among today's 97.

| Command | Module | Kind | Effect | From | Note |
| --- | --- | --- | --- | --- | --- |
| `get_app_info` | app | local | — | 046 | Paths, version and `onboardingDismissed` belong to this machine (ADR-0028 §2) |
| `reveal_app_data_dir` | app | local | — | 046 | Opens a local directory |
| `debug_provoke_error` | app | local | — | 046 | Debug builds only |
| `list_repositories` | repositories | board | Read | 046 | The DTO loses `path`, `worktreeRoot` and the per-machine columns (ADR-0028 §2) |
| `register_repository` | repositories | board | Write | 054, local until then | Split in two. The board half keeps the name and takes a remote (ADR-0033 §1). The local half maps a clone (ADR-0033 §2), and 054 names it |
| `update_repository` | repositories | board | Write | 046 | Name and default branch. `worktreeRoot` is a runner setting and left the patch in task 066 for the local `set_repository_worktree_root` (below). No MCP tool, as before |
| `set_repository_unattended_runs` | repositories | local | — | 046 | The runner's consent (ADR-0032). 045 adds the team ceiling as a separate board command |
| `set_repository_on_archive` | repositories | local | — | 046 | Runner configuration, per checkout (ADR-0033 §8) |
| `set_repository_max_concurrency` | repositories | local | — | 046 | A per-runner cap (ADR-0031 §6) |
| `remove_repository` | repositories | board | Write | 046 | Owner only (ADR-0029 §3) |
| `get_repository_remote_info` | repositories | local | — | 046 | Reads this machine's clone and `gh` |
| `get_repository_credential_status` | repositories | local | — | 046 | Credentials stay on the runner (ADR-0033 §6) |
| `set_repository_credential` | repositories | local | — | 046 | As above. No MCP tool (D25) |
| `remove_repository_credential` | repositories | local | — | 046 | As above |
| `create_task` | tasks | board | Write | 046 | |
| `get_task` | tasks | board | Read | 046 | `worktreePath` leaves the DTO (ADR-0028 §2) |
| `list_tasks` | tasks | board | Read | 046 | |
| `update_task` | tasks | board | Write | 046 | |
| `delete_task` | tasks | board | Write | 046 | No MCP tool (ADR-0021 §5) |
| `archive_task` | tasks | board | Write | 046 | The on-archive cleanup runs on the runner that holds the worktree, not in the handler (ADR-0033 §8) |
| `archive_tasks` | tasks | board | Write | 046 | As `archive_task` |
| `unarchive_task` | tasks | board | Write | 046 | |
| `move_task` | tasks | board | Write | 046 | D20.3's auto-removal on `done` becomes the runner's reaction to the change event once `worktree_auto_cleanup` is a runner setting (ADR-0028 §2) |
| `set_task_run_state` | tasks | board | Write | 046 | |
| `add_task_link` | tasks | board | Write | 046 | |
| `update_task_link` | tasks | board | Write | 046 | |
| `remove_task_link` | tasks | board | Write | 046 | |
| `reorder_task_link` | tasks | board | Write | 046 | |
| `set_task_dependencies` | tasks | board | Write | 046 | |
| `get_blocking_reason` | tasks | board | Read | 046 | |
| `get_base_instructions` | settings | board | Read | 046 | A team setting (ADR-0028 §2) |
| `set_base_instructions` | settings | board | Write | 046 | A team setting. Owner only (ADR-0029 §3) |
| `get_run_environment` | settings | local | — | 046 | A runner setting |
| `get_run_cost_summary` | settings | board | Read | 046 | The median is over the caller's teams' runs. Provider fields come from `BoardHost.provider` |
| `set_run_environment` | settings | local | — | 046 | A runner setting |
| `preview_composed_prompt` | settings | local | — | 046 | Composition and its vocabulary stay on the runner (ADR-0031 §1). Task 006 promises the preview matches a run's prompt byte for byte. Not available in the browser: a browser preview would need ADR-0031 §1 amended, not a composer on the server |
| `get_strategy_catalogue` | strategy | board | Read | 046 | A team setting. `defaultJson` and `providerInfo` come from `BoardHost.provider` |
| `set_strategy_catalogue` | strategy | board | Write | 046 | A team setting |
| `get_strategy_defaults` | strategy | board | Read | 046 | A team setting, global or per repository (ADR-0033 §1) |
| `set_strategy_defaults` | strategy | board | Write | 046 | |
| `get_strategy_approval` | strategy | board | Read | 046 | |
| `set_strategy_approval` | strategy | board | Write | 046 | |
| `accept_task_strategy` | strategy | board | Write | 046 | |
| `clear_task_strategy` | strategy | board | Write | 046 | |
| `plan_task_strategy` | strategy | local | — | 046 | Claims on this machine's runner and starts the planner. Stays local (amendment above, 060) |
| `plan_tasks_strategy` | strategy | local | — | 046 | This machine's watched pass, with `plan-pass:progress`. Stays local (amendment above) |
| `cancel_plan_pass` | strategy | local | — | 046 | Cancels this machine's pass. Stays local (amendment above) |
| `request_task_strategy` | strategy | board | Write | 060 | Records a request that an eligible runner claims with purpose `strategy` (ADR-0035 §6). Not among today's 97 |
| `request_tasks_strategy` | strategy | board | Write | 060 | As `request_task_strategy`, over a selection. Not among today's 97 |
| `get_worktree_status` | worktree | local | — | 046 | Live git in this machine's worktree. Other clients read the bundle on `get_run` (ADR-0033 §7) |
| `get_diff_summary` | worktree | local | — | 046 | As `get_worktree_status` |
| `reveal_task_worktree` | worktree | local | — | 046 | Only in the desktop app of the runner that holds the worktree (ADR-0033 §3) |
| `list_open_in_targets` | worktree | local | — | 046 | |
| `open_task_worktree_in` | worktree | local | — | 046 | As `reveal_task_worktree` |
| `get_worktree_inventory` | worktree | local | — | 046 | This machine's disk. Its MCP tool is `list_worktrees` |
| `remove_task_worktree` | worktree | local | — | 046 | D20's guards read task state through the runner (point 8), not through the board pool |
| `cleanup_done_worktrees` | worktree | local | — | 046 | As `remove_task_worktree` |
| `cleanup_merged_worktrees` | worktree | local | — | 046 | As `remove_task_worktree` |
| `get_worktree_auto_cleanup` | worktree | local | — | 046 | A runner setting |
| `set_worktree_auto_cleanup` | worktree | local | — | 046 | A runner setting |
| `start_task_run` | runs | board | Write | 052, local until then | Asks the server to claim the task for one runner. Only that runner's owner may ask (ADR-0031 §7) |
| `cancel_task_run` | runs | board | Write | 052, local until then | Reaches the runner holding the task through its lease (ADR-0031 §3). 052 decides who may ask |
| `retry_task_now` | runs | board | Write | 052, local until then | A claim through the task's pin (ADR-0031 §4). No MCP tool (D23) |
| `give_up_on_task` | runs | board | Write | 046 | A run-state transition. The pinned runner learns of it from the change event |
| `get_run_tail` | runs | board | Read | 048, local until then | The server relays tails (ADR-0036 §2) |
| `list_runs_for_task` | runs | board | Read | 046 | `logPath` leaves the DTO (ADR-0028 §2) |
| `list_runs` | runs | board | Read | 046 | As `list_runs_for_task` |
| `get_run` | runs | board | Read | 046 | Carries the review bundle (033), which is the diff every other client sees (ADR-0033 §7) |
| `read_run_transcript_page` | runs | board | Read | 056, local until then | Reads the server's copy (ADR-0036 §1). A summaries-only runner's transcript can be read only on its own desktop (ADR-0036 §5), through a local command that 056 adds |
| `search_run_transcript` | runs | board | Read | 056, local until then | As `read_run_transcript_page` |
| `summarize_run_transcript` | runs | board | Read | 056, local until then | As `read_run_transcript_page` |
| `reveal_run_log` | runs | local | — | 046 | Opens a local file |
| `get_run_log_size` | runs | local | — | 046 | This machine's transcript cache (ADR-0036 §1) |
| `prune_run_logs` | runs | local | — | 046 | As `get_run_log_size`. Server-side retention is a team setting (ADR-0036 §6) |
| `start_queue` | queue | local | — | 046 | Queue control belongs to the runner (ADR-0031 §6) |
| `pause_queue` | queue | local | — | 046 | As `start_queue` |
| `resume_queue` | queue | local | — | 046 | As `start_queue` |
| `stop_queue` | queue | local | — | 046 | As `start_queue` |
| `get_queue_status` | queue | local | — | 046 | This runner's queue, and why it skips each task |
| `get_run_capacity` | queue | local | — | 046 | A runner setting (ADR-0028 §2) |
| `set_schedule_mode` | queue | local | — | 046 | A runner setting |
| `set_max_concurrency` | queue | local | — | 046 | A runner setting |
| `list_schedules` | schedules | local | — | 046 | Schedules belong to the runner (ADR-0031 §6) |
| `create_schedule` | schedules | local | — | 046 | As `list_schedules` |
| `update_schedule` | schedules | local | — | 046 | As `list_schedules` |
| `set_schedule_enabled` | schedules | local | — | 046 | As `list_schedules` |
| `delete_schedule` | schedules | local | — | 046 | As `list_schedules` |
| `preview_schedule_preflight` | schedules | local | — | 046 | What this runner would do if the schedule fired now |
| `list_timezones` | schedules | local | — | 046 | Classified with the schedule commands as a group, as its MCP tool's scope decision is |
| `get_mcp_status` | mcp | local | — | 046 | The loopback server on this machine. `mcp_port` is a runner setting |
| `set_mcp_port` | mcp | local | — | 046 | As `get_mcp_status` |
| `test_mcp_connection` | mcp | local | — | 046 | As `get_mcp_status` |
| `get_analytics` | analytics | board | Read | 046 | Over the caller's teams' runs (ADR-0022, ADR-0036 §6) |
| `get_subscription_cost` | analytics | board | Read | 046 | A user setting (ADR-0028 §2) |
| `set_subscription_cost` | analytics | board | Write | 046 | A user setting |
| `run_doctor` | doctor | local | — | 046 | Checks this machine. The browser shows each runner's last reported result instead (ADR-0034 §5) |
| `dismiss_onboarding` | doctor | local | — | 046 | A runner setting |
| `dismiss_doctor_warning` | doctor | local | — | 046 | A runner setting |
| `restore_doctor_warning` | doctor | local | — | 046 | A runner setting |

#### Added after 728a049 (task 034)

Six commands added before 046, as point 8 requires. The counts above describe `main` at
728a049 and are left as they are. Each cites ADR-0021 point 3 and task 034.

| Command | Group | Kind | Effect | From | Note |
| --- | --- | --- | --- | --- | --- |
| `approve_task` | review | board | Write | 046 | As `move_task`: D20.3's auto-removal on `done` becomes the runner's reaction to the change event once `worktree_auto_cleanup` is a runner setting (ADR-0028 §2). ADR-0021 point 3, task 034 |
| `reject_task` | review | board | Write | 046 | The board handler writes the note, `branch = NULL`, the move and the marker. The uncommitted-changes refusal and the worktree removal (`review::actions::set_aside_worktree`, with the worktree record's `forget_worktree` inside it since task 066) run on the runner that holds the worktree, not in the handler (ADR-0033 §7). In connected mode the refusal is a runner-side check, and a synchronous refusal to the caller is not guaranteed (task 034). ADR-0021 point 3 |
| `request_task_changes` | review | board | Write | 046 | Touches no disk. ADR-0021 point 3, task 034 |
| `get_task_dependents` | review | board | Read | 046 | ADR-0021 point 3, task 034 |
| `get_review_digest` | review | board | Read | 046 | Rows only, no git. ADR-0021 point 3, task 034 |
| `mark_review_digest_seen` | review | board | Write | 046 | A user setting (`review_digest_seen_through`, D28 part 4). ADR-0021 point 3, task 034 |
| `list_review_findings` | review | board | Read | 046 | Rows only. The two findings writes have no command: only a run writes one (D30 point 5). ADR-0021 point 3, task 035 |
| `get_review_settings` | review | board | Read | 046 | A team setting (D28 point 4, ADR-0028 §2). Refused to every grant (ADR-0021 §4). ADR-0021 points 3 and 4, task 021 |
| `set_review_settings` | review | board | Write | 046 | A team setting. Refused to every grant: a run must not enable its own loop (ADR-0021 §4). `review_model` and `review_effort` are validated against the catalogue from `BoardHost.provider`, as `set_strategy_defaults` is (D32 point 2). ADR-0021 points 3 and 4, task 021 |
| `set_repository_review_config` | review | board | Write | 046 | As `set_review_settings`, per repository. The config is a column on `repositories`, so it is board state, not a per-checkout runner setting (ADR-0033 §1). ADR-0021 points 3 and 4, task 021 |
| `set_task_review` | review | board | Write | 046 | As `set_review_settings`, per task. 045 makes `review_instructions` consent-gated content with a revision (ADR-0032 §3); the handler stays on the board. ADR-0021 points 3 and 4, task 021 |
| `get_review_history` | review | board | Read | 046 | Rows only, no git: runs and findings, grouped by core. Refused to every grant, as `list_review_findings` is (D30 point 5's "everything else" row). ADR-0021 point 3, task 037 |
| `get_review_level` | review | board | Read | 046 | One level of the loop's configuration beside what it inherits and what it resolves to, so the interface never resolves the precedence chain itself. Refused to every grant, with the rest of the configuration (ADR-0021 §4). ADR-0021 points 3 and 4, task 037 |

#### Added by task 066

Four local commands, each replacing a per-machine field a board DTO lost (ADR-0028 §2), as
point 8 requires. Each is served by the shell and reads the machine store; none issues a
board query of its own (the 2026-10-04 amendment above).

| Command | Group | Kind | Effect | From | Note |
| --- | --- | --- | --- | --- | --- |
| `list_checkouts` | repositories | local | — | 046 | This machine's clone of each repository: path, worktree root, cap, consent, archive policy (ADR-0033 §2). Its board fact, which repositories the caller sees, comes through `repo::list`. The local MCP router's 23rd tool, `list_checkouts`, refused to runs (D16.1's snake case), keeps the capability `list_repositories` carried until 066 (ADR-0021 point 1). Task 066 |
| `set_repository_worktree_root` | repositories | local | — | 046 | Where this machine creates the repository's worktrees; `update_repository`'s `worktreeRoot` until 066 (D32's Binds). Inherits `update_repository`'s missing tool: the root had no MCP surface before, so nothing is lost. Task 066 |
| `list_local_worktrees` | worktree | local | — | 046 | `{ taskId, path }` from this machine's worktree records; what task DTOs carried as `worktreePath`. Paired with the existing `list_worktrees` tool, which already serves this machine's worktree paths. Task 066 |
| `get_run_log_path` | runs | local | — | 046 | One run's transcript path on this machine, derived from its ids (ADR-0013); what `Run.logPath` carried. A new ADR-0021 point 1 gap, recorded as point 9 records the existing ones; 056 replaces the derivation with `transcript_uploads.path`, and 071 closes the gap. Task 066 |

---

## D33 — Two offline query caches, one per schema (amends D5)

**Question.** Task 040 gives the runner its own crate, `crates/runner`, with its own migration
set under `crates/runner/migrations/` and "its own offline query cache" (ADR-0028 point 3; its
Consequences: "CLAUDE.md's single `DATABASE_URL` and root `.sqlx` prepare step becomes one per
crate, and CI runs both"). D5, ADR-0003's amendment and CLAUDE.md know of one cache only. It
sits at the workspace root and is regenerated by one `cargo sqlx prepare --workspace` against
one `DATABASE_URL`. Where do the two caches live? What regenerates each one? What changes in
CLAUDE.md and in CI?

**Decision.**

1. **One cache per crate that holds query macros, in that crate's directory.**
   `crates/core/.sqlx/` is the board's cache, described against `src-tauri/migrations`.
   `crates/runner/.sqlx/` is the runner's cache, described against `crates/runner/migrations`.
   Task 040 moves the existing cache with `git mv .sqlx crates/core/.sqlx` and then
   regenerates it with the recipe in point 3. **No `.sqlx/` remains at the workspace root.** A
   test in `rimaia-core`, `no_offline_query_cache_at_the_workspace_root`, asserts that
   `Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.sqlx")` does not exist. That check runs
   inside `cargo test -p rimaia-core`, so CI gains no step that CLAUDE.md lacks.

2. **Every query macro is described against the schema of the crate that holds it.**
   `rimaia-core` and `rimaia-runner` are the only crates that hold `query!`, `query_as!` or
   `query_scalar!`. `rimaia-runner` holds none that reads a board table. The first-launch copy
   (ADR-0028 point 5) reads the board through a `rimaia-core` function and writes `runner.db`
   through the runner's own queries. `rimaia-server` (task 046) and `src-tauri` hold no query
   macros. A third crate that believes it needs one stops and asks, for D4's reason: it would
   also need a third cache and a third recipe step. `crates/runner` embeds its migrations with
   `sqlx::migrate!("migrations")`. It gets a `build.rs` that prints
   `cargo:rerun-if-changed=migrations`, for the reason `crates/core/build.rs` gives.

3. **The regeneration recipe.** It replaces CLAUDE.md's three-line block. Run it from the
   workspace root, both steps, in this order, whichever crate's query or migration changed:

   ```bash
   # Absolute paths, passed with -D and never exported (seam-contract D33).
   BOARD_DB="sqlite:$PWD/target/sqlx-prepare-board.db?mode=rwc"
   RUNNER_DB="sqlite:$PWD/target/sqlx-prepare-runner.db?mode=rwc"
   rm -f target/sqlx-prepare-*.db*

   # 1. Board: rimaia-core against src-tauri/migrations, into crates/core/.sqlx
   cargo sqlx migrate run --source src-tauri/migrations -D "$BOARD_DB"
   (cd crates/core && cargo sqlx prepare -D "$BOARD_DB" -- --all-targets)

   # 2. Runner: rimaia-runner against crates/runner/migrations, into crates/runner/.sqlx.
   #    The check builds rimaia-core offline from the cache step 1 just wrote, so the
   #    prepare below does not rebuild it online against a database with no board tables.
   #    Errors from rimaia-runner in the check come from its stale cache, which the next
   #    line regenerates.
   cargo sqlx migrate run --source crates/runner/migrations -D "$RUNNER_DB"
   SQLX_OFFLINE=true cargo check -p rimaia-runner --all-targets || true
   (cd crates/runner && cargo sqlx prepare -D "$RUNNER_DB" -- --all-targets)
   ```

   The `cargo install sqlx-cli --version 0.8.6 …` line does not change.

4. **CLAUDE.md**, changed in task 040:
   - **Commands block.** Add two lines, each next to its `rimaia-core` twin:

     ```bash
     cargo test -p rimaia-core             # logic tests, no system deps needed
     cargo test -p rimaia-runner           # runner loop and runner.db, no system deps needed
     cargo fmt --all --check
     cargo clippy -p rimaia-core --all-targets -- -D warnings
     cargo clippy -p rimaia-runner --all-targets -- -D warnings
     cargo check --workspace --all-targets # includes the Tauri shell
     ```

   - **Layout table.** Add two rows:
     - `crates/runner/` | `rimaia-runner`: the runner loop, `runner.db` and the headless
       binary (ADR-0027, ADR-0028)
     - `crates/runner/migrations/` | `runner.db` migrations. Board migrations stay in
       `src-tauri/migrations/`
   - **The `SQLX_OFFLINE` paragraph.** It becomes: "`SQLX_OFFLINE=true` is set in CI, so
     clippy, `cargo test` and `cargo check` all compile the query macros against a checked-in
     cache instead of a live database. The board's queries use `crates/core/.sqlx/` and the
     runner's use `crates/runner/.sqlx/`. There is no cache at the workspace root, and a test
     fails if one appears. After changing any query, or any migration a query reads, in either
     crate, regenerate both caches and commit them:". The paragraph is followed by the point 3
     recipe.

5. **CI.** In `.github/workflows/ci.yml`'s `core` job:
   - Add `Clippy (runner)` after `Clippy`. It is `if: matrix.os == 'ubuntu-latest'` and runs
     `cargo clippy -p rimaia-runner --all-targets -- -D warnings`.
   - Add `Test (runner)` after `Test`. It runs on all three operating systems and runs
     `cargo test -p rimaia-runner`.
   - The runner links `rimaia-core`, so the job's libdbus step already covers it.
   - Job names do not change, because they are what required status checks match on.
   - The workflow-level `SQLX_OFFLINE: "true"` stays and is enough. Do not add
     `SQLX_OFFLINE_DIR`, `sqlx-cli` or `prepare --check` (D5 still applies).
   - The `shell` job's `cargo check --workspace --all-targets` compiles both crates offline,
     and so proves that both caches are complete for every target.

   The root `Cargo.toml` `members` becomes `["crates/core", "crates/runner", "src-tauri"]`.

6. **The rest of D5 stands:** the `query!` family for fixed-shape queries, no CI prepare
   step, and `SQLX_OFFLINE=true` exported for local verification commands. That now includes
   `SQLX_OFFLINE=true cargo test -p rimaia-runner`. Task 040 also appends a short amendment to
   ADR-0003's "The offline cache lives at the workspace root". The amendment says ADR-0028 and
   this entry supersede that section.

**Why.**

*Why one `cargo sqlx prepare --workspace` with one `DATABASE_URL` cannot serve both crates.*
A cargo invocation has one environment, and the macros read it before anything that belongs to
a single crate. `cargo sqlx prepare` gives the `cargo check` it starts `SQLX_OFFLINE=false`
and `DATABASE_URL`. sqlx-macros-core 0.8.6's `init_metadata` then takes `DATABASE_URL` from
the process before it reads the crate's own `.env`
(`env("DATABASE_URL").ok().or(database_url)`). Every crate compiled in that run is therefore
described against the same database. Against the board's database, every runner query fails
with `no such table`. Against the runner's, every board query fails the same way.

There are two ways to force a single prepare through, and each one breaks something:

- **A scratch database with both migration sets applied.** It compiles, but each crate is
  checked against a schema that neither shipped file has. A runner query that names `tasks`
  compiles and then fails against the first real `runner.db`. That is the exact failure
  compile-time checking exists to prevent, on the exact boundary ADR-0028 point 3 draws.
  Applying the second set also needs `migrate run --ignore-missing`, because
  `validate_applied_migrations` refuses applied versions that are missing from its source
  (D4, 2026-09-02). That flag turns off the one check that catches a deleted migration. And
  if both stores use the same table name, the second set fails outright.
- **Per-crate `.env` files with one workspace cache.** One directory cannot hold two
  descriptions of the same SQL. Each file is named
  `format!("query-{}.json", hash_string(&input.sql))` (`expand_input`), and neither the crate
  nor the database is part of that name. Suppose the runner keeps its settings in D3's
  key/value shape. Then the same `SELECT value FROM settings WHERE key = ?1` text in both
  crates is the normal case, not an edge case. Whichever crate was described last wins, and
  the other crate compiles against a description of a table it does not have. A committed
  `.env` that sets `DATABASE_URL` also sends online every build that has not exported
  `SQLX_OFFLINE`. Today no URL is set anywhere, so the macros fall back to the cache. That
  fallback is why `npm run tauri dev` needs no setup.

*Why the crate directory, and nothing at the root.* The macros look for a cache in this order:
the `SQLX_OFFLINE_DIR` named in a `.env`, then `<crate>/.sqlx`, then `<workspace root>/.sqlx`
(sqlx-macros-core 0.8.6, `expand_input`). A cache in the crate directory is therefore found
with no configuration anywhere, and CI's `SQLX_OFFLINE: "true"` needs nothing added. The root
directory is the fallback for both crates, so a stale copy left there silently answers any
query missing from a crate's own cache. It can even answer with the other schema's types.
That is exactly the incomplete-cache failure D5 relies on CI to catch. The point 1 test
changes it from silent to failing. Running the old `prepare --workspace` from habit creates
that root copy even when the run fails, because the command clears and fills the directory
before the failing crate is compiled.

*Why absolute URLs, `-D`, and fresh files.*

- **Absolute URLs.** Without `--workspace`, `cargo sqlx prepare` writes `.sqlx` into the
  current directory, and it refuses a virtual workspace root ("failed to get package in
  current working directory"). So each prepare runs inside its crate. rustc, and the macros
  inside it, run wherever cargo starts them, which for a workspace member is the workspace
  root. A relative URL would therefore name two different files. The one under
  `crates/core/target/` would not even be ignored, because `/target` in `.gitignore` is
  anchored at the root.
- **`-D` instead of `export DATABASE_URL`.** An exported URL outlives the recipe. The next
  build without `SQLX_OFFLINE` then goes online against whichever database was exported last.
  If that was the runner's, every `rimaia-core` query fails.
- **`rm -f` first.** D4's 2026-09-02 amendment found that reusing the scratch file makes
  `migrate run` refuse after a branch switch. With two files, that can happen twice.

*Why the offline check before step 2.* Without `--workspace`, prepare forces a recompile of
the current package only. But cargo resolves features per selected package set. So the
`rimaia-core` built for `rimaia-runner` becomes a different artifact from the one step 1
built inside `crates/core` as soon as the runner enables a feature on a dependency the two
crates share.

If that artifact is stale, step 2 compiles it online against a database with no board tables,
and the prepare fails on a `rimaia-core` query. The failure is loud, not a wrong cache, but it
happens on every clean `target/`. The check builds that artifact offline from the cache step 1
just wrote. Cargo does not track a proc macro's environment reads on stable, because sqlx uses
`proc_macro::tracked_env` only under `procmacro2_semver_exempt`. The prepare therefore finds
the artifact fresh. The `|| true` is there because `rimaia-runner`'s changed queries are, by
definition, not in its cache yet.

*Why separate `-p` lines instead of `-p rimaia-core -p rimaia-runner`.* A single invocation
unifies the features of both crates. A feature that `rimaia-core` uses but only the runner
enables would then compile there and nowhere else, and `rimaia-server` builds `rimaia-core`
on its own.

**Binds.** 040 lands points 1–6.

- **Tasks that change a query or migration** in either crate from 040 on run the point 3
  recipe and commit both caches: 041, 066, 042, 043, 045, 047, 051, 054, 056, and any later
  task that touches a query macro. The runner migrations are the ones D4 reserves:
  `20261003130000_runner_store.sql`, `20261003130100_machine_state.sql`,
  `20261003130200_checkout_mapping.sql` and `20261003130300_outbox.sql`, under
  `crates/runner/migrations/`.
- **Tasks bound by one point each:**
  - 046: point 2, no query macros in `rimaia-server`.
  - 058: the headless binary uses the runner's cache and adds no third one.
  - 062: the image builds with `SQLX_OFFLINE=true`, so the build context must include
    `crates/core/.sqlx/`. A `.dockerignore` that drops dot-directories drops the cache.
  - 064: the final pass confirms that CLAUDE.md's commands and `ci.yml`'s steps still match
    line for line.
- **Tasks before 040** (033, 035, 038, 039) regenerate with D5's recipe unchanged.

Add D33 to the "How to use this" row of every task listed above.

---

## D34 — Team mode's dependencies, approved up front (a D6 amendment)

**Question.** Team mode (ADRs 0027–0037) needs a headless browser, an SSE client that can
send a header, a signed updater, token hashing and randomness, an HTTP server's middleware,
a command line, and TLS. The tasks that build it run unattended, one after another, on one
long-lived branch. Which of these may they add, in which task, on which version line, and
what did they decline? Without an answer, each task either stops to ask in the middle of an
unattended run, or adds a dependency without anyone having decided it. D6 exists to rule out
the second.

**Decision.** Three npm packages and ten Cargo entries are approved. Each is introduced
by the task named here and by no earlier one. Later tasks reuse what is already in the tree.
**The list is still closed**, and the D6 prohibition now also covers `devDependencies`
explicitly, as well as the crates this backlog creates (`crates/server`, `crates/runner`).
D6 gets a one-line pointer, `### Amendment, 2026-09-30 — team mode's list, see D34`, so a
reader who starts at D6 finds this entry.

npm:

| Package | Task | Line | Where |
| --- | --- | --- | --- |
| `@playwright/test` | 028 | `^1`, a `devDependency` | `npm run screenshot` only |
| `@microsoft/fetch-event-source` | 049 | `2.0.1`, exact | imported only by `src/lib/events.ts` |
| `@tauri-apps/plugin-updater` | 063 | `^2`, like the other plugins | imported only under `src/lib/` |

- **`@playwright/test`.** Task 028's Notes name it and require the ask. This entry is the
  ask, answered. The runner is the reason for choosing it over the bare `playwright`
  library. Its `webServer` option starts Vite on 028's own port (not 1420, which
  `npm run tauri dev` owns) and waits for it. Its `projects` express the colour schemes and
  viewports that 028 Scope 3 asks for, without a hand-written loop; the view list is a table
  in the spec.
  **One engine, WebKit**, installed with `npx playwright install webkit`. On macOS the Tauri
  webview is WKWebView (ADR-0002 targets macOS first), so WebKit is the engine closest to
  what ships. The browser is never installed in CI: 028's Out of scope says nothing in CI
  depends on the screenshots, and `npm ci` downloads no browser for this package.
  *Considered:* Puppeteer, whose install step downloads Chrome on every `npm ci`, CI
  included. Also `tauri-driver`, which has no macOS support (028's own Out of scope).
- **`@microsoft/fetch-event-source`.** The browser's `EventSource` cannot set an
  `Authorization` header, and a connected desktop authenticates with a bearer desktop token
  (ADR-0030 point 3, ADR-0034 point 4). The workaround would put the token in the query
  string, which puts it in server access logs. ADR-0037 point 6 treats those logs as holding
  team data. One client serves both the cookie mode and the bearer mode, so the browser does
  not get a second SSE path. It is pinned exactly because the package has not had a release
  since 2021. It has no dependencies of its own, and D7 already confines subscription code
  to `src/lib/events.ts`, so replacing it touches one file. Its default is to retry forever,
  so a `401` (`Unauthenticated`) or an `UpgradeRequired` response must throw from `onopen`.
  Neither error goes away by reconnecting. *Considered:* a hand-written `fetch` +
  `ReadableStream` parser. Parsing the stream is the easy part. Reconnect, backoff and
  close-on-abort are the parts that are easy to get wrong, and this library already handles
  them.
- **`@tauri-apps/plugin-updater`.** ADR-0037 point 5 requires signed updates, and there is
  no hand-written version of verifying a signed update. It is the same kind of entry as
  `plugin-dialog` and `plugin-notification`. It takes the four coordinated edits D6 lists
  (`package.json`, `src-tauri/Cargo.toml` with `tauri-plugin-updater = "2"`, the plugin
  init in `src-tauri/src/lib.rs`, and `updater:default` in
  `src-tauri/capabilities/default.json`) plus a fifth: `plugins.updater.pubkey` /
  `endpoints` and `bundle.createUpdaterArtifacts` in `src-tauri/tauri.conf.json`. The
  signing keys come from a person, and tests use fakes. ADR-0034 point 4 says no component
  imports a Tauri plugin, so the import stays under `src/lib/`, and the browser build's
  `get_client_capabilities` answer hides the update action.

Cargo. Each crate is a `[workspace.dependencies]` line in the root `Cargo.toml`, as every
existing dependency is. It is referenced with `{ workspace = true }` only from the crates
named here:

| Crate | Task | Line | Crates that use it |
| --- | --- | --- | --- |
| `sha2` | 047 | `0.10` | `rimaia-core` (identity) |
| `rand` | 047 | `0.10` | `rimaia-core` (identity) |
| `subtle` | 047 | `2` | `rimaia-core` (identity) |
| `axum-extra` | 047 | the release line built on `axum` 0.8, feature `cookie` | `rimaia-server` |
| `tower-http` | 046 | `0.6`, features `trace` (046), `cors` and `fs` (050) | `rimaia-server` |
| `reqwest`, TLS feature | 047 | the existing `0.13` | `rimaia-server` (047), `rimaia-runner` (052), `src-tauri` (059) |
| `clap` | 058 | `4`, feature `derive` | `rimaia-runner` only |
| `tracing-subscriber` | 058 | the shell's `0.3`, default features plus `env-filter` | `src-tauri`, `rimaia-runner` (058), `rimaia-server` (062) |
| `tracing-appender` | 058 | the shell's `0.2` | `src-tauri`, `rimaia-runner` |
| `tauri-plugin-updater` | 063 | `2` | `src-tauri` only |

- **`sha2` 0.10.** ADR-0030 point 3 stores only a SHA-256 hash of every `rmd_`/`rmr_`/`rmp_`
  token. The PKCE `S256` challenge is the same function. `sqlx-core`'s `migrate` feature
  already compiles `sha2 v0.10.9` into `rimaia-core`, so, as with `base64` in D6's task-022
  amendment, promoting it to a direct dependency adds nothing new to the dependency tree.
  *Considered:* the SHA-256 in `aws-lc-rs`, which arrives with the TLS feature below. That
  would tie a hash-at-rest format to the choice of TLS provider.
- **`rand` 0.10.** A cryptographically secure source (a CSPRNG) is needed for the 256-bit
  token and session secrets, the PKCE verifier and the OAuth `state`. The CSRF token needs
  none of its own, because 047 derives it from the session secret (D28). The pairing code
  (ADR-0030 point 5) is drawn from a reduced alphabet that a person can type. Picking
  uniformly from that alphabet without modulo bias is what `rand` provides and calling
  `getrandom` directly does not. `rmcp` already pulls in `rand v0.10.2`, so this is the same
  line. *Considered:* `Uuid::new_v4`, which has 122 bits, not 256, and gives no guarantee
  about its randomness source.
- **`subtle` 2.** ADR-0030's Consequences require constant-time comparison. It applies to
  every secret compared in memory: the CSRF token, the OAuth `state`, and a pairing-code
  hash. The crate has no dependencies, and a hand-written constant-time compare is exactly
  the code a compiler optimisation silently breaks.
- **`axum-extra`, feature `cookie` only.** The session cookie must be `HttpOnly`, `Secure`
  and `SameSite=Lax` (ADR-0030 point 2), and `CookieJar` sets those attributes from typed
  fields. A hand-formatted `Set-Cookie` string would drop an attribute without anyone
  noticing. Sessions are ADR-0030's own `sessions` table, so no signed or private cookie
  features are needed. The bearer header is parsed with `strip_prefix("Bearer ")`, so there
  is no `typed-header`. **The version is whichever line depends on the workspace's
  `axum = "0.8"`.** `cargo tree -d` must show one `axum`.
- **`tower-http` 0.6.** Already compiled, through `reqwest`, with none of these features.
  `trace` provides request spans that carry `source` and `user_id` (ADR-0030 point 8). A
  span records method, matched route and status, never a body, because of ADR-0037 point 6.
  `cors` allows the Tauri origins and no others (ADR-0034 point 6). `fs` provides
  `ServeDir` for the built bundle at `/`. Nothing else: axum's own `DefaultBodyLimit`
  covers body limits, raised on `append_transcript` alone. *Considered:* writing each of
  the three layers by hand. CORS preflight in particular is a place where one wrong header
  lets through an origin that should have been refused.
- **`reqwest`'s TLS feature.** The approval says `rustls-tls`, but that is the 0.12 name.
  The workspace pins `reqwest = "0.13"` (resolved `0.13.4`), and its manifest has no
  `rustls-tls`: **the feature is spelled `rustls`**. It turns on rustls with the `aws-lc-rs`
  provider and `rustls-platform-verifier`, which checks certificates against the OS trust
  store, so a proxy with a corporate CA works. The feature is enabled **at each use site,
  never on the workspace line**. The workspace line keeps `default-features = false`, so
  `rimaia-core`'s `mcp::probe` stays plain HTTP to `127.0.0.1`, as its comment in
  `Cargo.toml` says. Note that Cargo's feature unification still links TLS into any binary
  that also depends on one of those crates, which is accepted. Request bodies are built
  with `serde_json::to_vec` and an explicit `Content-Type`, so neither `json` nor `form` is
  added. `aws-lc-sys` is built from C with the platform toolchain. 047 confirms all three CI
  runners build it in the same commit. If any runner needs a system package for it, that is
  a stop-and-ask, as `libdbus` was for `keyring`. *Considered:* `native-tls`, which puts
  OpenSSL on the Linux build's critical path, the thing `keyring`'s `crypto-rust` was chosen
  to avoid. Also `rustls-no-provider` plus a direct `ring`: one more direct dependency, to
  avoid a C build that CI already handles.
- **`clap` 4, `derive`, in `rimaia-runner` only.** `rimaia-runner pair <server> <code>` and
  `rimaia-runner run`, with `--help` and `--version`, run on a machine where the only
  interface is a terminal (ADR-0030 point 5). `--version` is what a person compares against
  the web UI's out-of-date list (ADR-0037 point 5). `rimaia-server` does **not** take it: a
  container is configured through its environment (062), and `std::env::var` is enough to
  read five variables. `rimaia-core` never takes it: a library has no command line.
  *Considered:* matching on `std::env::args` by hand, which works for two subcommands but
  leaves usage text and argument errors to be written, tested and kept in step.
- **`tracing-subscriber` and `tracing-appender`, promoted, not added.** A headless runner
  and a hosted server are diagnosed from their logs, and ADR-0037 point 6 needs the
  server's to be tested for content (062). Both are `src-tauri`-only lines today, already
  in `Cargo.lock` at these versions, so promoting them to `[workspace.dependencies]` adds
  nothing to the tree: the argument D6 made for `base64` and this entry makes for `sha2`.
  058 promotes both and switches `src-tauri` to `{ workspace = true }`; 062 reuses the
  subscriber line in `rimaia-server`. **Default features stay on**, because they carry the
  `fmt` layer every one of them writes with. No `json` feature, which would add
  `tracing-serde`.

Hand-written, as the approval already says:

- **OAuth PKCE.** The whole of it is `BASE64URL_NOPAD(SHA256(verifier))` with a verifier
  from `rand`. `base64`'s `URL_SAFE_NO_PAD` engine is already a direct dependency (D6, task
  022). The GitHub side is two HTTPS requests, behind ADR-0030's `IdentityProvider` seam.
  `oauth2` was declined: it is an abstraction over every grant type, built on its own choice
  of HTTP client, to make two requests to one provider.
- **Rate limiting.** A fixed-window counter keyed by endpoint and caller, reading the
  injected `Clock`, so the tests use the fake clock CLAUDE.md requires. `governor` /
  `tower_governor` were declined. They carry their own clock, which a test cannot advance
  without a second fake, and they key on the peer IP, which behind the host's proxy is the
  proxy.

Things this backlog does **not** take, so that each absence reads as a decision:

- No `jsonwebtoken` (ADR-0030 rejects JWTs), and no `tower-sessions`: the `sessions`
  table *is* the session store.
- No `argon2` or `bcrypt`. A slow hash protects low-entropy secrets. Tokens are 256 random
  bits, and the one short secret, the pairing code, is single-use, expires after ten
  minutes and is rate limited.
- No WebSocket crate (ADR-0034 point 3 chose SSE), and no `msw`. 049's HTTP mock replaces
  `fetch` at the same boundary where the suite already mocks `@tauri-apps/api/core`.
- No `tauri-plugin-process` for relaunching after an update. The relaunch is a local
  command calling `AppHandle::restart()`.
- No object-storage client (ADR-0036 point 7 keeps transcripts on disk), and no `regex`
  for redaction: ADR-0036 point 4 matches names by suffix and redacts values exactly.
- No `metrics` or `prometheus` crate for ADR-0037 point 7. The counters are atomics
  rendered as text by a handler. Litestream and rclone are binaries in 062's image, pinned
  by version and checksum in the `Dockerfile`, not dependencies that this entry or D6
  governs. rclone is a backup tool the server runs as a child, not the object-storage
  client declined above: the transcript store stays on disk.
- **A known gap, recorded rather than guessed at.** `axum::response::Sse` takes a
  `Stream`, and 048 cannot build one from `tokio::sync::broadcast` without `futures-util`
  or `tokio-stream` as a direct dependency. Neither is approved here. 048 asks. The ask
  should be cheap, because both are already in `Cargo.lock` (`tokio-stream v0.1.19`, and
  `futures-util` through `tower-http`), which is the same no-new-tree argument as `base64`
  and `sha2`.

**Why.** The two reasons behind D6 hold with more force on this branch, not less. First,
every task on it edits a lockfile. When the workflow's fix rounds regenerate that lockfile,
they silently revert whatever changed it before them. Second, a Cargo dependency links into
binaries that an unattended agent runs, and that a hosted server exposes to the internet.
Approving all of it up front is the only form of the list that an unattended workflow can
follow: an ask in the middle of a run is a `blocked` status and a stopped queue. Pinning
each entry to one task keeps each lockfile change inside the commit that needs it, where a
reviewer sees it next to its reason. Recording the declined alternatives, and the one gap,
exists so that the next agent does not re-open a question that was already settled, or
settle one that was not.

See also D6 (the rule and its earlier amendments), D7 (`src/lib/events.ts` as the only
subscription seam), [ADR-0030](adr/0030-identity-people-sign-in-machines-pair.md) (tokens,
sessions, PKCE, rate limits), [ADR-0034](adr/0034-one-api-for-the-web-and-the-desktop.md)
(SSE, CORS, bundle at `/`), and [ADR-0037](adr/0037-hosting-backups-and-version-skew.md)
(updater, logs, metrics).

**Binds.** 028, 046, 047, 048, 049, 050, 052, 058, 059, 062, 063. Every other task is bound
as a prohibition, on both sides of every crate boundary and in `devDependencies`.

### Amendment, 2026-10-04 — 048's ask, answered

The known gap above is closed before 048 starts, so that 048 never has to stop and ask in
the middle of an unattended run. **`tokio-stream` is approved**, introduced by 048:

| Crate | Task | Line | Crates that use it |
| --- | --- | --- | --- |
| `tokio-stream` | 048 | `0.1`, `default-features = false` | `rimaia-server` only |

- **What for.** `tokio_stream::wrappers::ReceiverStream` turns the bounded `mpsc` receiver
  that 048's per-stream pump fills into the `Stream` that `axum::response::Sse` takes. In
  0.1.19, `ReceiverStream` is outside every feature gate, so no feature is turned on. The
  default `time` feature is turned off, because nothing uses it.
- **Nothing new in the tree.** `tokio-stream v0.1.19` is already in `Cargo.lock`, through
  `rmcp` and `sqlx-core`. This is the argument D6 made for `base64` and this entry made for
  `sha2`. It is one `[workspace.dependencies]` line, referenced with `{ workspace = true }`
  from `crates/server/Cargo.toml` alone. `cargo tree -d` must show one `tokio-stream`.
- **Core never takes it.** `Subscription::next()` is a plain `async fn`, so core's tests
  drive it without a `Stream`. The desktop shell's forwarder loops over the same
  subscription and needs no adapter either.
- *Considered:* `futures-util`, which is larger than one adapter needs; and implementing
  `http_body::Body` by hand, which needs `http-body` as a direct dependency to build a
  `Frame`, so it is more code and still an ask. Also `BroadcastStream` straight over the
  channel, which needs the `sync` feature and `tokio-util`, and would skip the subscription's
  admission and re-read logic.

---

## D35 — Task 059's cross-cutting choices

**Question.** Task 059 makes the desktop the second host of a connected runner. Ten choices
sit under it that no ADR makes, and that 060, 061, 064 and 069 each meet again: how the
mode is known, where the new secrets live, which tokens a run's output is scrubbed of, what
the loopback endpoint admits, what it serves, how one host differs from the headless one,
what "in the foreground" means for Run now, how a local handler reads a board it does not
hold, what pairing asks, and how the mode changes. Left to the implementer, each would be
decided in a diff with nothing for a reviewer to check it against.

**Decision.**

1. **The mode is derived, never stored.** `rimaia_runner::connection::resolve_mode(
   board_file_exists, identity)` answers `Unchosen`, `Solo` or `Connected` from
   `runner_identity` and whether `rimaia.db` exists, checked before `db::connect` (which
   opens with `mode=rwc`). `desktop_state` adds `SignedOut` when the desktop token is
   missing or the keychain is locked. Both are pure functions in the runner crate, and
   `DesktopMode` is an enum. No column and no migration (D28's amendment makes one a
   stop-and-ask).
2. **Keychain accounts are keyed by runner id.** `runner-token:<runner_id>` and
   `desktop-token:<runner_id>` (added by 056), and `loopback-mcp-token:<runner_id>` (059),
   all as `CredentialKey` variants. The one new stored value is the `runner_settings` key
   `loopback_mcp_token_id`, with a typed accessor (D3).
3. **Every Rimaia token the machine holds is a host secret.** `secrets::host_secrets` reads
   all three items, so `RunnerConfig::host_secrets` redacts the `rmr_`, `rmd_` and loopback
   `rmp_` values from every transcript, stderr log and tail. It becomes `HostSecrets`, an
   `Arc` over a lock around 056's `Redactor`, read once per spawn, and
   `RunnerHost::add_host_secret` merges a token minted while the host runs. Minting is
   refused while `InFlight` is not empty, because a run already spawned holds the redactor
   it started with.
4. **The loopback gate.** `mcp::build` takes a `LoopbackGate`, a shared handle over
   `LoopbackAuth::{Open, Locked, Token { hash }}`, read on every request. Solo is `Open`.
   Connected is `Token` with a stored loopback token and `Locked` without one. Only `/mcp`
   is gated, never `/mcp/run/{token}`. A refusal is `401`, `WWW-Authenticate: Bearer`, and
   D8's `unauthenticated` body. A token missing from `list_api_tokens` at a connected
   launch locks the gate, and from 060 an upstream `401` locks it at once.
5. **The connected loopback serves local tools only, under the name `rimaia`.**
   `BoardTools::{Local(ServiceContext), Remote { origin }}` decides the board half. Under
   `Remote`, a board tool is a tool error naming the server until 060's relay replaces the
   refusal. In the relay, a local name shadows an upstream tool of the same name, and calls
   go upstream with the loopback token, so the server records `Door::Mcp`.
6. **One composition, two flags.** The desktop starts 058's `RunnerHost` with
   `serve_run_proxy: false` (its `mcp::build` mounts the run route on the one loopback
   listener) and `start_queue: false` (D15: a launch starts paused). The headless binary
   passes `true` for both. The desktop takes 058's `runner.lock` before opening
   `runner.db`.
7. **ADR-0031 point 7's "with the app in the foreground" is met by the door, not by a
   focus query.** `run_here` is a `local` command with no MCP tool and no HTTP route, so
   only a click in this machine's Rimaia window reaches it, and that window is in the
   foreground when it takes the click. No OS focus check is made: one could only disagree
   with the click by racing it, and refusing then would turn a deliberate click into an
   unattended run. Every other way to start a run on this machine (the board row, 052's
   relay, a schedule, the loopback) is unattended. The posture is fixed at the claim.
8. **A local handler reads the board through `BoardCommands` or the runner's
   `BoardPort`, in both modes.** `rimaia_core::api::BoardCommands` has one method,
   `call(name, args)`. `InProcessCommands` wraps 046's `api::dispatch` with the solo
   `Caller`. `rimaia_runner::connection::HttpCommands` posts to `/api/v1/<name>` with the
   desktop token. Three consequences are decided here:
   - **`BoardPort::preview` gains a second reader**, `preview_composed_prompt`, beside D31
     point 4's starter preflight. It stays advisory and writes nothing, and a run is still
     composed from its claim's context.
   - **A worktree's live status and diff measure from the recorded run's base** (the latest
     `implementation` or `fix` run's `base_sha`, else `base_ref`, else the repository's
     `default_branch`), in both modes. 044's fresh resolution and its warning are removed
     from the live path.
   - **D20 guard 1 refuses when the board cannot be reached.** It has no override, and an
     unknown run state is not a spare directory.

   `src-tauri/src/commands/` names `state.board` nowhere, and
   `check-command-wiring.sh` enforces it.
9. **Pairing asks two questions, and records the answers with the server.**
   `connect_to_server` takes `uploadTranscripts` and `runEnvironment`. They are required on
   a first connect, optional on Sign in again, and written in the runner-store transaction
   that writes `runner_identity`, never before it. The form preselects *Upload full
   transcripts* (ADR-0036 point 5's default) and *Strict / local* with ADR-0032 point 6's
   recommendation, matching 058's `pair`. `get_transcript_upload` answers `{ value,
   disclosure }`, with `disclosure` set to `transcripts::UPLOAD_DISCLOSURE`, so the
   paragraph has one source. After pairing, the setting's control is Settings →
   Connection, not 069's `This machine's limits`.
10. **Changing mode restarts the app.** The choice is written, then `restart_app` calls
    `AppHandle::restart()`. There is no teardown path: `setup()` builds the runner, the MCP
    listener, the board port, `BoardCommands` and `AppState` once per mode.

**Why.** (1) and (10) keep one construction path per mode. A stored mode could disagree with
the rows it summarises, and a live switch would need a teardown nothing else exercises. (2)
is ADR-0023's problem: two data directories on one machine must not share a keychain item.
(3) follows ADR-0036 point 4 to its end. The loopback token is the one a run is most likely
to see, because in `inherit` mode Claude Code's own configuration holds it as a header, and
a redactor that learns of a token only at the next launch leaves a window in which it does
not. (4) and (5) are ADR-0030 point 6 and ADR-0035 point 4 made concrete. Once connected,
loopback reaches every team the user belongs to, and the gate is the control D30 point 6
relies on for what the run denial cannot see. (6) is what keeps 058's host the only
composition: the desktop differs by two booleans, not by a second builder. (7) is the reading
an implementer would otherwise make silently, in one direction or the other. Writing it
down means 061 does not add a focus check, and no later door is mistaken for an interactive
one. (8) is D32 point 8 applied. The recorded base is the one the morning review already
shows, and a single path means solo cannot drift from connected. (9) applies ADR-0036
point 5 and ADR-0032 point 6 to the desktop's pairing. Writing the answers with the server
means a failed connect cannot change a solo machine's `run_environment`, which both modes
share since 041.

See also [ADR-0030](adr/0030-identity-people-sign-in-machines-pair.md) points 4–6,
[ADR-0031](adr/0031-runners-claim-work-with-leases.md) point 7,
[ADR-0035](adr/0035-mcp-when-the-board-is-remote.md) point 4, D20 point 1, D30 point 6,
D31 point 4, and D32 point 8 with its 2026-10-04 amendment.

**Binds.**

- **059** carries all ten points.
- **060** builds the relay of point 5 behind the gate of point 4, adds the upstream `401`
  lock, and keeps the local-name shadowing.
- **061 and 069** treat `run_here` as the only interactive door (point 7), and add no
  focus check. 069's `This machine's limits` does not carry `upload_transcripts` (point 9).
- **Any later local handler** reads the board through point 8's seams. A new reader of
  `BoardPort::preview` amends point 8.
- **064** checks CLAUDE.md's connected-mode lines against points 2, 4 and 6.

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
| [017](../tasks/017-morning-review-flow.md) | D4 · D6 · D7 · D8 · D9 · D12 · D18 · D29 · D32 · D34 |
| [018](../tasks/018-preflight-doctor-and-packaging.md) | D11 · D16 · D22 |
| [020](../tasks/020-per-task-execution-strategy.md) | D2 · D3 · D4 · D5 · D8 · D10 · D12 · D16 · D17 · D19 |
| [021](../tasks/021-review-and-fix-loop.md) | D3 · D4 · D5 · D6 · D8 · D9 · D10 · D12 · D17 · D18 · D19 · D20 · D23 · D24 · D25 · D27 · D28 · D29 · D30 · D31 · D32 |
| [022](../tasks/022-per-repository-git-credentials.md) | D4 · D5 · D6 · D8 · D10 · D14 · D20 · D25 |
| [023](../tasks/023-batch-strategy-planning.md) | D16 · D17 · D19 · D21 |
| [024](../tasks/024-analytics.md) | D4 · D5 · D12 · D18 · D20 |
| [025](../tasks/025-startup-failure-dialog.md) | D6 · D11 |
| [026](../tasks/026-open-worktree-in-editor.md) | D6 · D12 · D20 |
| [027](../tasks/027-dismissable-doctor-warnings.md) | D3 · D4 · D8 · D22 |
| [028](../tasks/028-let-a-run-see-the-ui-it-changed.md) | D6 · D7 · D8 · D9 · D12 · D14 · D21 · D22 · D32 · D34 |
| [030](../tasks/030-archiving-tasks-and-on-archive-cleanup.md) | D4 · D5 · D8 · D12 · D19 · D20 · D26 |
| [031](../tasks/031-a-provider-seam-for-the-agent-cli.md) | D3 · D4 · D6 · D8 · D14 · D17 · D27 |
| [032](../tasks/032-provider-vocabulary-outside-the-runner.md) | D3 · D8 · D22 · D27 |
| [033](../tasks/033-record-the-commit-a-run-ended-on-and-a-review-bundle.md) | D2 · D4 · D5 · D8 · D10 · D18 · D20 · D28 · D29 · D31 · D32 · D33 · D34 |
| [034](../tasks/034-review-actions-on-every-door.md) | D3 · D4 · D5 · D6 · D8 · D9 · D12 · D16 · D18 · D20 · D28 · D29 · D30 · D32 · D33 |
| [035](../tasks/035-runs-have-a-kind-and-review-findings-have-a-home.md) | D2 · D4 · D5 · D6 · D8 · D10 · D12 · D16 · D17 · D18 · D19 · D23 · D27 · D28 · D29 · D30 · D31 · D32 · D33 |
| [036](../tasks/036-a-board-port-between-the-runner-and-the-board.md) | D5 · D8 · D10 · D14 · D17 · D19 · D23 · D27 · D28 · D29 · D30 · D31 · D32 · D34, and D4 and D6 as prohibitions |
| [037](../tasks/037-the-review-loop-in-the-interface.md) | D4 · D5 · D6 · D7 · D8 · D9 · D12 · D17 · D20 · D28 · D29 · D30 · D32 · D34 |
| [038](../tasks/038-team-mode-schema-and-a-scoped-service-context.md) | D2 · D3 · D4 · D5 · D6 · D8 · D10 · D11 · D17 · D28 · D29 · D31 · D32 · D33 · D34 |
| [039](../tasks/039-every-board-service-filters-by-team.md) | D3 · D4 · D5 · D6 · D8 · D10 · D12 · D13 · D16 · D17 · D20 · D21 · D23 · D24 · D28 · D29 · D30 · D31 · D32 · D33 · D34 |
| [040](../tasks/040-the-runner-store.md) | D3 · D4 · D5 · D6 · D8 · D10 · D11 · D28 · D33 · D34 |
| [041](../tasks/041-machine-state-moves-to-the-runner.md) | D3 · D4 · D5 · D6 · D7 · D8 · D10 · D11 · D12 · D13 · D15 · D16 · D17 · D19 · D20 · D21 · D22 · D23 · D24 · D25 · D26 · D28 · D29 · D30 · D31 · D32 · D33 · D34 |
| [042](../tasks/042-split-the-scheduler.md) | D3 · D15 · D19 · D21 · D22 · D23 · D24 · D27 · D28 · D29 · D31 · D32 · D33, and D4, D6 and D34 as prohibitions |
| [043](../tasks/043-runner-leases.md) | D4 · D6 · D8 · D9 · D10 · D11 · D14 · D15 · D17 · D19 · D21 · D23 · D27 · D28 · D29 · D30 · D31 · D32 · D33 |
| [044](../tasks/044-branch-from-the-dependencys-commit.md) | D4 · D5 · D8 · D18 · D20 · D28 · D29 · D31 · D32 · D33 |
| [045](../tasks/045-consent-and-eligibility.md) | D2 · D3 · D4 · D6 · D8 · D10 · D12 · D16 · D17 · D21 · D23 · D27 · D28 · D29 · D30 · D31 · D32 · D33 · D34 |
| [046](../tasks/046-the-server-crate-and-one-command-registry.md) | D2 · D4 · D6 · D7 · D8 · D10 · D11 · D16 · D20 · D26 · D28 · D29 · D31 · D32 · D33 · D34 |
| [047](../tasks/047-identity.md) | D4 · D6 · D8 · D10 · D11 · D28 · D31 · D32 · D33 · D34 |
| [048](../tasks/048-events-over-sse.md) | D2 · D4 · D6 · D7 · D8 · D10 · D14 · D24 · D28 · D31 · D32 · D33 · D34 |
| [049](../tasks/049-frontend-transport.md) | D4 · D6 · D7 · D8 · D14 · D20 · D28 · D32 · D33 · D34 |
| [050](../tasks/050-the-web-shell.md) | D4 · D6 · D7 · D8 · D10 · D11 · D12 · D14 · D22 · D28 · D29 · D32 · D33 · D34 |
| [051](../tasks/051-teams-invitations-and-roles.md) | D2 · D3 · D4 · D6 · D7 · D8 · D10 · D16 · D28 · D32 · D33 · D34, and the entries 045, 048 and 050 add |
| [052](../tasks/052-the-runner-protocol-and-an-http-board-adapter.md) | D4 · D6 · D8 · D10 · D14 · D19 · D28 · D29 · D31 · D32 · D33 · D34 |
| [053](../tasks/053-leases-across-the-network.md) | D4 · D6 · D8 · D9 · D11 · D12 · D14 · D17 · D19 · D21 · D28 · D29 · D31 · D32 · D33 |
| [054](../tasks/054-repositories-by-remote.md) | D2 · D4 · D6 · D7 · D8 · D10 · D11 · D20 · D22 · D25 · D26 · D28 · D30 · D31 · D32 · D33 · D34 |
| [055](../tasks/055-the-run-scoped-proxy-and-consent-laundering.md) | D4 · D6 · D8 · D17 · D27 · D28 · D29 · D30 · D31 · D32 · D33 · D34 |
| [056](../tasks/056-transcripts-leave-the-machine.md) | D3 · D4 · D5 · D6 · D8 · D9 · D10 · D14 · D17 · D18 · D19 · D20 · D23 · D25 · D28 · D29 · D31 · D32 · D33 · D34 |
| [057](../tasks/057-push-postcondition-and-run-elsewhere.md) | D4 · D6 · D8 · D9 · D12 · D17 · D20 · D23 · D25 · D28 · D29 · D30 · D31 · D32 · D33 · D34 |
| [058](../tasks/058-the-headless-runner.md) | D3 · D4 · D6 · D8 · D9 · D10 · D11 · D15 · D19 · D20 · D22 · D25 · D28 · D30 · D31 · D32 · D33 · D34 |
| [059](../tasks/059-desktop-connected-mode.md) | D3 · D4 · D6 · D7 · D8 · D10 · D11 · D15 · D16 · D19 · D20 · D25 · D27 · D28 · D29 · D30 · D31 · D32 · D33 · D34 · D35 |
| [060](../tasks/060-hosted-mcp.md) | D4 · D6 · D8 · D16 · D17 · D19 · D20 · D23 · D25 · D28 · D30 · D31 · D32 · D33 · D34 · D35, and the entries 045, 048, 050 and 051 add |
| [061](../tasks/061-assignment-consent-and-runners-in-the-interface.md) | D3 · D4 · D6 · D7 · D8 · D9 · D10 · D12 · D16 · D17 · D28 · D29 · D30 · D31 · D32 · D33 · D34 · D35, and the entries 045, 050, 051, 054, 058 and 060 add |
| [062](../tasks/062-hosting.md) | D4 · D5 · D6 · D8 · D11 · D19 · D28 · D29 · D31 · D32 · D33 · D34 |
| [063](../tasks/063-a-signed-desktop-updater.md) | D4 · D6 · D7 · D8 · D9 · D11 · D15 · D20 · D28 · D32 · D33 · D34 |
| [064](../tasks/064-docs-and-ci-final-pass.md) | D5 · D8 · D16 · D28 · D29 · D30 · D32 · D33 · D35, the entries 060 and 071 add, and D4, D6 and D34 as prohibitions |
| [065](../tasks/065-drop-retired-columns.md) | D3 · D4 · D6 · D8 · D11 · D28 · D31 · D33, and D34 as a prohibition |
| [066](../tasks/066-checkouts-and-worktree-records-move-to-the-runner.md) | D4 · D7 · D8 · D12 · D13 · D16 · D17 · D20 · D25 · D26 · D28 · D29 · D31 · D32 · D33 · D34 |
| [067](../tasks/067-the-model-rule-and-who-may-start-a-run.md) | D8 · D17 · D19 · D27 · D28 · D29 · D31 · D32 · D33 |
| [068](../tasks/068-transcript-retention-on-the-server.md) | D3 · D18 · D28 · D29 · D32 |
| [069](../tasks/069-runners-in-the-interface.md) | D4 · D7 · D8 · D10 · D12 · D28 · D31 · D32 · D33 · D34 · D35 |
| [070](../tasks/070-a-context-that-acts-for-nobody.md) | D8 · D10 · D32 · D33, and D4 and D6 as prohibitions |
| [071](../tasks/071-close-the-hosted-mcp-parity-gaps.md) | D16 · D25 · D30 · D32 · D33, and the entries 048, 050, 054 and 060 add |
| every task | D4 and D6 as prohibitions |

A reviewer treats any decision visible in a diff that is neither in an ADR nor here as a
finding, **even when the code looks right**. The objection is not that the choice was bad; it
is that the next agent has no way to inherit it.

Adding an entry: append it with the next free `D` number, in the same four-part shape. Never
renumber — same rule as ADRs and tasks. If an entry grows into an architectural decision, write
the ADR and replace the entry's body with a one-line pointer, as D2 shows.
