---
id: "038"
title: Team-mode schema, solo identity, and a scoped service context
milestone: v0.5
status: ready
depends_on: ["021", "036"]
adrs: ["0028", "0029", "0030", "0031", "0034", "0035", "0019", "0018"]
size: L
---

# Team-mode schema, solo identity, and a scoped service context

## Goal

Give the board an owner. After this task every repository and task belongs to a team, every
run names the runner it ran on, and the existing installation has become what ADR-0029 point
2 says solo is: one team, one user and one runner, with generated ids, recorded as the
installation's solo identity. Every `ServiceContext` carries the team scope and the acting
user it was built for, and every change event carries the team it belongs to.

Three things land together because none is testable without the others:

- **the one table rebuild**, `20261003120000_team_mode_board.sql`, exactly as seam-contract
  D28 part 6 writes it, applied through a `db::migrate` that turns foreign keys off around
  the migrator (D28 part 1);
- **the solo identity**, adopted by that file on a board that has something to adopt and
  created by `identity::ensure_solo` on one that does not (D28 part 3);
- **the scope and actor on `ServiceContext`** (ADR-0029 point 5, ADR-0030 point 8) and **the
  team on `ChangeEvent`** (ADR-0034 point 3).

**This task filters nothing.** A service that reads a team's rows still reads everyone's.
Making every service honour the scope is [task 039](README.md), and 039 ships in the same
release. What this task guarantees is that no context can be built without a scope, so 039
converts functions and never has to invent where a scope comes from.

## Why now

038 is where `team_id` arrives, and it is the last chance to put a table-level constraint on
`tasks`. D28 part 1 measured what a careless rebuild does on this schema: with enforcement
on, `DROP TABLE tasks` cascades into `runs` and `task_links` and reports success. So the
rebuild happens once, here, behind three guards and a row-for-row test, before anything in
M2–M4 writes a column that depends on it. 043's leases, 045's consent columns and 054's
remotes are all additive files precisely because this one did the rebuild.

The context fields land now for the reason ADR-0019 gave `source`: a required field is a
compile error at every construction site, and on main there are four. From 046 on there is a
fifth for every door, and each would otherwise have to guess.

## Scope

**1. `db::migrate` owns the foreign-key pragma (D28 part 1).** A new
`db::apply_migrations(&Migrator, &SqlitePool)` in `crates/core/src/db/mod.rs` does D28's six
steps, and `db::migrate` keeps its signature and calls it with `MIGRATOR`. Task 040 calls
the same helper for the runner store, so it takes the migrator as a parameter and names no
board table. A `foreign_key_check` row after a run that applied something is an
`Error::internal` naming the table, rowid and parent, which the shell's existing migration
failure path reports (D11). Nothing starts a migration file with `-- no-transaction`.

**2. The migration.** `src-tauri/migrations/20261003120000_team_mode_board.sql`, with D28
part 6's DDL: the two opening guards, the seven new tables (`users`, `teams`,
`team_memberships`, `runners`, `solo_identity`, `team_settings`, `user_settings`), the
adoption, the rebuilds of `repositories`, `tasks` and `runs` in that order, and the closing
guard. Three rules the DDL cannot show:

- **The rebuilt tables follow the files on disk.** If `20261001120000_…` (033) or
  `20261001120100_…` (035) as they landed differ from D28 part 6, the rebuild redeclares
  what the files say. Every column those tables have when this task starts is redeclared
  with its type, default and `CHECK`, except the three relaxed `NOT NULL`s.
- **The settings split is placed by D28 part 4 as it stands when 038 lands**, including
  its dated amendment from task 034. Tasks 028–037 and 021 may have added keys, and each
  such key's accessor states its placement. Two are known: 034's
  `review_digest_seen_through` is **User**, and 021's review-loop keys are Team. So the
  user-settings copy is an `IN` list, not D28 part 6's `=`:
  `WHERE s.key IN ('subscription_monthly_usd', 'review_digest_seen_through')`, and the
  team copy's `NOT IN` names both user keys beside the runner keys. A key nobody listed goes
  to `team_settings` by exclusion, which is correct for 021's keys and wrong for 034's; that
  is why 034's is spelled out.
- **The header comment is this task's**, in the voice of the existing migrations: why the
  file rebuilds, why it never renames the old table, why it guards, and why it adopts rather
  than always creating.

**3. The placement lists in Rust.** `db::settings` gains `RUNNER_KEYS` and `USER_KEYS`, the
two lists the migration spells in SQL (`USER_KEYS` has two entries,
`subscription_monthly_usd` and 034's `review_digest_seen_through`), and
`placement(key) -> Placement { Team, User, Runner }`, where `Team` is the exclusion. The
rebuild test drives its expectations from them, so the SQL and the Rust cannot drift
silently. Task 039 moves readers by this function, and task 040 copies `RUNNER_KEYS` into
`runner.db`. `Placement` is an enum, not a string (CLAUDE.md).

**4. The identity module.** New: `crates/core/src/identity/mod.rs`.

- `SoloIdentity { team_id, user_id, runner_id, created_at }`, read from `solo_identity`.
- `Role { Owner, Member }`, with `as_str` and parsing in `team_memberships.role`'s `CHECK`
  spelling. D32 point 7's `TeamGrant.role` is this enum.
- `create_personal_team(conn, clock, login) -> PersonalTeam { user_id, team_id }`: one user,
  their personal team, an owner membership, and `team_settings.base_instructions` seeded
  from `DEFAULT_BASE_INSTRUCTIONS`. It writes inside the caller's transaction, because
  `ensure_solo` adds a runner and a `solo_identity` row to the same one, and task 047's
  sign-up adds a `users` identity. This is the team-creation service D28 part 3 names.
- `ensure_solo(pool, clock) -> Result<SoloIdentity>`, D28 part 3's three cases: load the row
  if it exists; create the five rows in one transaction with `Uuid::new_v4()` if there is
  none and `teams` is empty; refuse with `Error::invalid` if there is none and `teams` is
  not empty, saying the file belongs to a server and cannot be opened as a solo board. Both
  paths write D28's placeholders: login `solo`, team `Personal`, runner `This computer`,
  provider `ProviderId::ClaudeCode.as_str()`.

It takes a pool, not a `ServiceContext`, because it runs before any context can exist: the
context's scope is what it returns.

**5. The scope and the actor on `ServiceContext`.** In `crates/core/src/context.rs`:

```rust
pub struct ServiceContext {
    pub pool: SqlitePool,
    pub clock: Arc<dyn Clock>,
    pub changes: broadcast::Sender<ChangeEvent>,
    pub tail: broadcast::Sender<RunTail>,
    pub source: MutationSource,
    pub scope: TeamScope, // ADR-0029 point 5
    pub actor: UserId,    // ADR-0030 point 8
}

impl ServiceContext {
    pub fn new(pool: SqlitePool, clock: Arc<dyn Clock>, source: MutationSource,
               scope: TeamScope, actor: UserId) -> Self;
    pub fn with_source(&self, source: MutationSource) -> Self; // unchanged
    pub fn with_scope(&self, scope: TeamScope) -> Self;        // keeps the same senders
}
```

- **`TeamScope` is the set of teams a request may touch**, not one team, because ADR-0035
  point 2 lets an entity's id determine its team and ADR-0029 point 5 resolves the set once,
  at the edge. It is a non-empty, deduplicated, sorted `Arc<[TeamId]>`.
  `TeamScope::one(id)`, `TeamScope::of(ids) -> Result<Self>` (empty is `Error::invalid`),
  `contains(&str)`, `teams() -> &[TeamId]`, and `sole() -> Result<&TeamId>`, which is
  `Error::invalid` when the scope names more than one team. `sole` is for a write that
  creates a team-owned row with no parent to take the team from: registering a repository,
  and a settings write until 039.
- **`actor` is a plain user id (D10), not an enum.** Every writer that exists through M4
  acts for a user: the solo user, a signed-in caller, a runner's owner. A context acting for
  nobody, such as 053's expiry sweep on the server, is 053's to add, by widening this field.
  Nothing in this task writes the actor to a column; 045's `tasks.created_by` is the first.
- **No `Default`, and no constructor that defaults either field.** Every construction site
  states both. On main @728a049 there are four: `src-tauri/src/lib.rs` (from `ensure_solo`),
  `testing/context.rs` (from `ensure_solo`, so service tests exercise the app's path),
  `context.rs`'s own tests, and `crates/core/tests/repo_service.rs`. Tasks 033–037 may
  have added more, and each is converted the same way;
  `grep -rn 'ServiceContext::new\|ServiceContext {' crates src-tauri/src` finds them.
  `mcp::build` and `scheduler::build` re-source a clone and inherit both, which is why task
  039 lists them among the places a context is built. `TestContext` also keeps the
  `SoloIdentity` it got, as `TestContext::solo`, so a test reads the solo team, user and
  runner ids instead of querying for them.
- **The hand-written `impl fmt::Debug for ServiceContext`** (`context.rs`, beside the
  struct) prints `scope` and `actor` next to `source`. It lists fields by hand, so without
  this edit it would silently leave them out.
- **The tracing span records the actor.** Every `#[tracing::instrument]` that records
  `source = ctx.source.as_str()` also records `user_id = ctx.actor.as_str()` (ADR-0030 point
  8). There are fifteen today.
- `TeamId`, `UserId` and `RunnerId` are `String` aliases in `events.rs`, beside `TaskId`
  (D10).

**6. The team on `ChangeEvent` (ADR-0034 point 3).**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEvent { pub team_id: TeamId, pub change: Change }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change { Tasks(Arc<[TaskId]>), Repositories(Arc<[RepositoryId]>),
                  Runs(Arc<[RunId]>), Schedules(Arc<[ScheduleId]>), Settings }
```

The constructors take the team first: `ChangeEvent::tasks(team_id, ids)`,
`ChangeEvent::settings(team_id)`, and so on. `is_empty` moves to `Change`. ADR-0018's rules
are unchanged: ids, never rows; publish after commit; never publish an empty list.

- **An event never names an id outside its `team_id`.** A publish whose rows can span
  teams groups its ids by `team_id` and publishes one `ChangeEvent` per team. The sweeps
  that gather ids across repositories are the ones this binds: worktree cleanup, reconcile,
  archive cleanup and dependency unblocking. In solo every sweep is one team, so each still
  publishes one event, but 048's per-team fan-out relies on the rule, not on solo. One
  helper in `events.rs` does the grouping, from `(TeamId, id)` pairs the sweep's query
  returns once it selects `team_id` beside each id, so no sweep writes its own loop.
- **The team is the written row's `team_id`, read in the same transaction** (a `RETURNING
  team_id`, or the row already in hand). It is never assumed from the scope, which may name
  several teams. A run's team is its task's.
- **Where the written state has no team column yet**, which is `settings` and `schedules`
  until 039 and 041, the event names `ctx.scope.sole()?`. Each such site carries a comment
  naming the task that changes it. Task 048 splits machine-local events off the team
  channel.
- **The shell drops the team.** `emit_change_event` in `src-tauri/src/lib.rs` and
  `src-tauri/src/notify.rs` match on `event.change`. Every Tauri event name and payload is
  unchanged, and the lagged-receiver recovery emits a `Change` per variant, not a
  `ChangeEvent`, because "re-read everything" is not about one team. No file under `src/`
  changes.

**7. Writes that the new `NOT NULL`s require.**

- `repo::register` writes `team_id = ctx.scope.sole()?`.
- `tasks::create_task` writes the repository's `team_id`, read in the same transaction. The
  composite foreign key `(repository_id, team_id)` is the backstop.
- `board::service::start_run` writes `runs.runner_id`. The in-process adapter
  (`crates/core/src/board/in_process.rs`, task 036) gains the solo runner's id at
  construction (D31 point 9) and passes it; `StartRun` gains no field, because the runner is
  the adapter's scope, never a request field (D31 point 3).
- `LeaseRef` gains `team_id` (D31 point 2), filled at claim from the task's row. Narrowing
  the context to it and refusing another team's lease as `NotFound` is 039's.
- Every raw `INSERT INTO repositories`, `tasks` or `runs` in a test supplies the team and,
  where it matters, the runner. `grep -rn 'INSERT INTO \(repositories\|tasks\|runs\)'
  crates` lists them: 42 in 15 files on main, unit-test modules included, with
  `crates/core/tests/store.rs` holding most. The ids come from one place. A test holding a
  `TestContext` reads `ctx.solo`. A test holding only a pool calls
  `identity::ensure_solo(&pool, &clock)` once, with any test clock, and uses the ids it
  returns. No test inserts into `teams`, `users`, `team_memberships` or `solo_identity` by
  hand, except the refusal and rebuild tests this task adds, which need a board in a state
  `ensure_solo` would not produce. A second team is `create_personal_team`'s.
- **The contract harness gets two runners.** `crates/core/tests/board_port_in_process.rs`
  (036, D31 point 13) builds two `InProcessBoard`s over one `TestContext`, and each now
  needs a runner id, which `runs.runner_id` references. Runner A is the solo runner.
  Runner B is a second `runners` row for the solo user, written by a new
  `testing::db::insert_runner(conn, clock, user_id, label) -> RunnerId`, behind the
  `testing` feature. It is a fixture,
  not a pairing service: pairing is 047's and 052's, and nothing outside `testing` creates a
  runner except `ensure_solo`.

**8. The three relaxed columns.** `repositories.path`, `repositories.worktree_root` and
`runs.log_path` now infer `Option` in every `query!`. Each reader either handles `None` or
uses a `"path!"`-style override whose comment names the task that retires the reader: 066,
for all three. `Repository` and `Run` keep their `String` fields, so no DTO the frontend
sees changes shape. `.sqlx/` is regenerated with D5's recipe,
which D33 keeps for every task before 040.

**9. Startup.** `src-tauri/src/lib.rs`'s `setup()` calls `identity::ensure_solo` after
`db::migrate` and before the `ServiceContext` is built, through the same
`log_startup_failure` / `report_startup_failure` path the migration uses (D11), and builds
the context with `TeamScope::one(solo.team_id)` and `solo.user_id`. `AppState` keeps the
`SoloIdentity`, which 046's `Caller::solo` reads (D32 point 7).

**10. Documentation.**

- `context.rs`'s module and field docs say what `scope` and `actor` are, and that ADR-0029
  and ADR-0030 are the "later record" ADR-0019 asked for.
- ADR-0029 point 2 gains the one-line pointer to D28 part 3 that D28 promises, if Phase 0
  did not add it. No decision in any ADR is edited.
- CLAUDE.md's Gotchas gains one bullet: board migrations are applied only through
  `db::migrate`, which turns foreign keys off around the migrator; never apply one to a real
  `rimaia.db` with `cargo sqlx migrate run` or the sqlite3 CLI, and no migration file begins
  with `-- no-transaction`. The prepare recipe is unchanged and still works, because 038's
  guard passes on an empty `tasks`.

## Out of scope

- **Filtering by team.** No read gains a `WHERE team_id`, no lookup of another team's id
  returns `NotFound`, and the ~34 functions that take a raw `&SqlitePool` (`startup`,
  `capacity`, `analytics`, settings accessors) keep their signatures. All of it is 039's,
  with the two-team refusal tests.
- **Reading `team_settings` or `user_settings`.** This task copies into them once. Every
  settings reader still reads `settings` until 039 moves it. The copy is a snapshot, so on a
  development board a setting changed between 038 and 039 exists only in `settings`. 038 and
  039 ship in one release, so no user sees that.
- **`runner.db`**, `RUNNER_KEYS`' copy, and every retired column's reader: 040 and 041.
- **Any other migration.** 043, 045, 047, 051, 054 and 056 own theirs (D28's D4 amendment).
  If a column this task needs is missing from D28 part 6, stop and ask; do not add a file.
- **Dropping anything.** Retired columns and `settings` stay until 065 (ADR-0028 point 5).
- **`Caller`, `for_caller`, the registry and every door's own context.** 046 (D32).
- **Anything the user sees.** Solo shows no team, user or runner (ADR-0030 point 7). No DTO
  gains `team_id` in this task.
- **Deleting a team or a user.** 051 writes the ordered service D28's Why describes.

## Acceptance criteria

- `src-tauri/migrations/20261003120000_team_mode_board.sql` exists under exactly that name,
  its first line is its title and not `-- no-transaction`, and its DDL is D28 part 6's, with
  the 033/035 columns as those files landed and the settings lists matching
  `db::settings::RUNNER_KEYS` and `USER_KEYS`. It is the only new migration.
- `db::apply_migrations(&Migrator, &SqlitePool)` exists and `db::migrate` goes through it.
  After it returns, `PRAGMA foreign_keys` reads 1 on every connection the pool hands out,
  including after a failed run.
- D28 part 7's 038 tests exist under these names and pass, each against real files in a
  `TempDir` or the in-memory test pool, never a mocked store:
  - `the_team_mode_rebuild_keeps_every_row`, which builds the pre-038 board with
    `Migrator::new` over a `TempDir` holding copies of every board file older than
    `20261003120000`, fills every table as D28 part 7 lists, runs `db::migrate` on the same
    file, and asserts every value column by column, equal row counts, the adopted team and
    runner everywhere, each settings key where `placement` says, an empty
    `pragma_foreign_key_check`, and `foreign_keys` reading 1;
  - `a_fresh_board_adopts_nothing`;
  - `the_rebuild_refuses_to_run_over_a_cascade`, which fails with the guard's constraint
    name and leaves the task and run in place;
  - `a_dangling_reference_stops_the_rebuild_and_keeps_the_board`;
  - `ensure_solo_and_adoption_build_the_same_identity`, equal on everything except ids and
    timestamps, including the `team_settings` row for `base_instructions`, with every id
    parsing as a version-4 UUID;
  - `no_migration_opts_out_of_its_transaction`.
- These further tests exist and pass:
  - `a_failed_migration_leaves_no_connection_with_enforcement_off`: a `Migrator` over a
    `TempDir` whose last file fails, applied to an unmigrated one-connection in-memory pool
    built as `test_pool` builds its own; afterwards `PRAGMA foreign_keys` reads 1;
  - `a_second_launch_finds_the_same_solo_identity_and_creates_nothing`;
  - `a_board_that_belongs_to_a_server_refuses_to_open_as_solo`: `teams` has a row and
    `solo_identity` none; `ensure_solo` returns `Invalid` and writes nothing;
  - `every_settings_key_has_the_placement_the_migration_gave_it`: every key constant in the
    crate, `BASE_INSTRUCTIONS` through 034's `review_digest_seen_through` and 021's keys, is
    placed as D28 part 4 and its 034 amendment say, with `review_digest_seen_through` and
    `subscription_monthly_usd` as `User`;
  - `a_scope_names_at_least_one_team` and `a_scope_of_two_teams_has_no_sole_team`;
  - `with_scope_publishes_to_the_original_subscribers` and
    `with_scope_changes_only_the_scope`, beside their `with_source` twins in `context.rs`;
  - `a_change_event_names_the_team_of_the_row_it_announces`: a second team made with
    `create_personal_team`, a repository registered in it through a context scoped to that
    team alone, then a task created in that repository through a context scoped to both
    teams; each published `ChangeEvent`'s `team_id` is the second team's;
  - `ids_from_two_teams_become_one_event_per_team`, on the `events.rs` grouping helper:
    pairs from two teams give two events, each naming only its own team's ids, and no
    team with no ids gets an event. The sweeps that use it are exercised across two teams
    by 039's refusal tests;
  - `a_task_takes_its_repositorys_team`, and a raw insert of a task whose `team_id` differs
    from its repository's is refused by the store;
  - `a_run_records_the_runner_that_started_it`, through the in-process board;
  - `a_claims_lease_names_the_tasks_team`.
- `ServiceContext::new` takes `scope` and `actor`, there is no `Default` impl and no
  constructor that supplies either, and every construction site names both (four on main:
  the shell's `setup()`, `testing/context.rs`, `context.rs`'s tests and
  `crates/core/tests/repo_service.rs`, plus any 033–037 added). The `Debug` impl prints
  `scope` and `actor`.
- Every `#[tracing::instrument]` that records `source` also records `user_id`.
- `ChangeEvent` is the struct above, and no service derives a published team from
  `ctx.scope` except through `sole()` at the sites Scope 6 names, each with its comment.
  No published event names an id whose row belongs to a different team than its `team_id`.
- `repositories.path`, `repositories.worktree_root` and `runs.log_path` are nullable in the
  schema, and every override that keeps a reader on `String` carries a comment naming 066.
- **Behaviour is unchanged.** No existing test's assertion about behaviour changes. An
  existing test's diff is confined to:
  - fixture setup: a team or runner in a raw insert, the identity in the harness, and the
    second runner in the contract harness (Scope 7);
  - building and matching `ChangeEvent`s with a team;
  - the schema inventory: `a_fresh_database_gets_every_table_the_schema_declares` in
    `crates/core/tests/store.rs` gains exactly the seven new tables, `runners`,
    `solo_identity`, `team_memberships`, `team_settings`, `teams`, `user_settings` and
    `users`, in its sorted list, and loses none;
  - reads of the three relaxed columns in test helpers (`store.rs`'s `fetch_repository` and
    `fetch_run` among them), which gain the Scope 8 overrides or handle `None`.

  No other existing assertion changes. No file under `src/` changes, and the 31 frontend
  test files that mock `@tauri-apps/api/core` pass untouched.
- `.sqlx/` is regenerated with D5's recipe and committed, and `SQLX_OFFLINE=true cargo check
  --workspace --all-targets` passes with it.
- CLAUDE.md carries the Gotchas bullet from Scope 10, and ADR-0029 point 2 points at D28
  part 3.
- Every CI check passes: `npm run typecheck`, `npm run test`, `npm run build`, `cargo test
  -p rimaia-core`, `cargo fmt --all --check`, `cargo clippy -p rimaia-core --all-targets --
  -D warnings`, `cargo check --workspace --all-targets`,
  `./scripts/check-command-wiring.sh`.
- **Needs a person, and the PR body carries it as a checklist:** launch with
  `RIMAIA_DATA_DIR` pointing at a scratch directory holding a copy of a real, long-used
  `rimaia.db`. The board, run history, settings and repositories look exactly as they did,
  and `sqlite3 rimaia.db 'PRAGMA foreign_key_check'` prints nothing. This is M2's manual
  check, taken early.

## Notes

**Entry points.** Phase 0 adds 038's row to [`tasks/README.md`](README.md) (milestone
v0.5, depends on 021 and 036) and a row to `docs/seam-contract.md`'s "How to use this"
table: D4 · D5 · D6 · D8 · D10 · D11 · D28 · D29 · D31 · D32 · D33. If either is missing
when this task starts, add it as written here in the task's first commit.

**Read first.** Seam-contract D28 in full, with 034's dated amendment: parts 1–3 and 7 are
this task's, part 4 is its settings split, part 6 is the DDL to type out, and the D4
amendment beside it names the file. Then D31 points 2, 3, 9 and 13 (`LeaseRef.team_id`, the
adapter's runner id, the two-runner contract harness), D32 point 7 (what 046 will build on
`Role` and `SoloIdentity`), D29 (the rebuild recreates `idx_runs_task_kind`), D33's last
paragraph (D5's recipe still applies), D10, D11, D8 (no new `ErrorCode`: every refusal here
is `Invalid` or `Internal`), and D4 and D6 as prohibitions. Of the ADRs: 0028 points 2 and
5, 0029 points 2 and 5, 0030 points 7 and 8, 0034 point 3 (the team on `ChangeEvent`, an
amendment of ADR-0018), 0035 point 2 (why a scope is a set of teams), and 0031 for the
runner a run names. ADR-0019 and ADR-0018 for the struct and the event this task widens.

**Files to start from.** `crates/core/src/context.rs`, `crates/core/src/events.rs`,
`crates/core/src/db/mod.rs` (`MIGRATOR`, `migrate`, `connect`),
`crates/core/src/db/settings.rs` (`DEFAULT_BASE_INSTRUCTIONS` and the key constants),
`crates/core/src/db/models.rs` (`Repository`, `Run`), `crates/core/src/testing/context.rs`,
`crates/core/src/testing/db.rs` (`test_pool`), `crates/core/src/repo/mod.rs`,
`crates/core/src/tasks/service.rs`, `crates/core/src/runs/mod.rs`, `crates/core/src/board/`
(task 036), `src-tauri/src/lib.rs` (`setup`, `forward_change_events`, `emit_change_event`),
`src-tauri/src/notify.rs`, `crates/core/tests/store.rs`,
`crates/core/tests/repo_service.rs`. About a hundred `ChangeEvent::` sites span 26 files,
tests included; `grep -rn 'ChangeEvent::' crates src-tauri/src` lists them. The key
constants are scattered: `db/settings.rs`, `scheduler/{capacity,state,pause}.rs`,
`schedule/window.rs`, `worktree/cleanup.rs`, `mcp/settings.rs`, `runner/process.rs`,
`strategy/{catalogue,settings}.rs`, plus whatever 021 added. A good home for the migration
tests is a new `crates/core/tests/team_mode_board.rs`, reaching the migration directory
through `env!("CARGO_MANIFEST_DIR")` as `crates/core/src/testing/fixtures.rs` does, joined
with `../../src-tauri/migrations`, the relative path `crates/core/build.rs` watches.

**Migration.** `src-tauri/migrations/20261003120000_team_mode_board.sql`. Once this task
lands on the branch the file is frozen (D28's D4 amendment): every later task's scratch
database has applied it, and sqlx rejects a changed checksum.

**The traps D28 already found, so nobody finds them twice.** sqlx 0.8.6 ignores `--
no-transaction` on SQLite, and `PRAGMA foreign_keys` does nothing inside a transaction. A
`BEGIN … COMMIT` in the file breaks sqlx's bookkeeping. Renaming the old table first
repoints every child's `REFERENCES`. Copying with `*` misorders the columns that `ADD
COLUMN` appended. `abs(random())` can overflow. Each is a reason D28 gives, not a style
choice.

**What the chain provides.** 033 and 035 wrote the columns this rebuild redeclares
(`head_sha`, `base_sha`, `review_bundles`, `kind`, `review_findings`, and 021's
`review_instructions`/`review_config`). 036 put the runner's board writes behind
`BoardPort`, so `start_run` and the claim are each one function to change here, and
`LeaseRef` already exists without its team.

**What the next tasks expect.** 039: a scope on every context, `TeamScope::contains` and
`sole`, `team_id` on every repository and task, `team_settings`/`user_settings` filled,
`placement` to move readers by, and `create_personal_team` for its two-team fixture. 040:
`solo_identity` to copy into `runner_identity`, and `RUNNER_KEYS`. 043: `runners` and
`tasks.team_id`. 045: `users` and `team_settings`. 046: `Role`, `SoloIdentity` in
`AppState`, and a `ServiceContext::new` that `for_caller` can fill. 047:
`create_personal_team` and the `users` identity columns. 048: `ChangeEvent.team_id`.

**Size.** Roughly 2.5–3.5k lines before `.sqlx/`: about 350 of SQL, 400 of migration tests,
the identity module and its tests, and a long tail of one-line changes at the hundred
publish sites, the 15 spans and the readers of three columns. **Nothing here may be cut
into 039.** 039's "What 038 provides" lists `ChangeEvent` carrying its team among the
things it expects and stops if one is missing, and its registry test checks every event's
team. The schema, the identity and the context fields cannot be separated either: the
context needs the identity, and the identity needs the schema. If the task cannot be
finished whole, stop and ask; do not land part of it.

