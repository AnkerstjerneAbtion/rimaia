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
| [008](../tasks/008-claude-code-runner.md) | D2 · D3 · D5 · D7 · D8 · D9 · D10 · D14 · D15 |
| [009](../tasks/009-sequential-run-queue.md) | D2 · D5 · D7 · D8 · D9 · D10 · D14 · D15 |
| [010](../tasks/010-local-mcp-server.md) | D2 · D3 · D4 · D5 · D6 · D8 · D10 · D12 · D13 · D16 |
| [011](../tasks/011-task-dependencies-and-blocking.md) | D12 · D16 |
| [013](../tasks/013-run-scheduling.md) | D15 |
| [015](../tasks/015-run-history-and-log-viewer.md) | D14 |
| [016](../tasks/016-worktree-lifecycle-and-cleanup.md) | D17 |
| [018](../tasks/018-preflight-doctor-and-packaging.md) | D11 · D16 |
| [020](../tasks/020-per-task-execution-strategy.md) | D2 · D3 · D4 · D5 · D8 · D10 · D12 · D16 · D17 |
| [021](../tasks/021-review-and-fix-loop.md) | D17 |
| every task | D4 and D6 as prohibitions |

A reviewer treats any decision visible in a diff that is neither in an ADR nor here as a
finding, **even when the code looks right**. The objection is not that the choice was bad; it
is that the next agent has no way to inherit it.

Adding an entry: append it with the next free `D` number, in the same four-part shape. Never
renumber — same rule as ADRs and tasks. If an entry grows into an architectural decision, write
the ADR and replace the entry's body with a one-line pointer, as D2 shows.
