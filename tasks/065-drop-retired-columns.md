---
id: "065"
title: Drop retired columns
milestone: v0.5
status: not-ready
depends_on: ["064"]
adrs: ["0028", "0031", "0032", "0037"]
size: M
---

# Drop retired columns

## Goal

Take the second step of [ADR-0028](../docs/adr/0028-the-server-owns-the-board-and-each-runner-keeps-its-own-store.md)
point 5. One board migration, in the release **after** the one that ships team mode, drops
what that release copied out of `rimaia.db` and stopped reading:

- on `repositories`: `path`, `worktree_root`, `max_concurrency`, `credential_login`,
  `credential_label`, `credential_added_at`, `on_archive` and `on_archive_script`;
- on `tasks`: `worktree_path`;
- on `runs`: `log_path`;
- the `schedules` table and the `settings` table, whole.

The columns are the ones seam-contract D28 part 6's comments on
`20261003120000_team_mode_board.sql` mark as retired. The two tables are retired by D28
part 4 (`settings` split three ways) and part 5 ("dropped whole"). Nothing else is dropped.
The code that still names them goes in the same change, because after the drop it no
longer compiles: task 040's `db::settings::runner_placed`, task 041's
`machine::adoption::read_board`, the `machine_state` adoption step, and the copy half of the
`settings` step.

**The drop cannot be undone, so it refuses to run on a board whose copy never happened.**
The one function that migrates a board first checks that the board went through the
team-mode release and, on a solo install, that the runner store recorded both copies. A
board that fails the check is left untouched, `runner.db` too, and the app says which
release to open first.

## Why now

It is not now, and the `status` says so. ADR-0028 point 5 splits the change across two
releases, and "the release in between is what makes a rollback possible". While that release
is the newest, every value 040, 041 and 054 copied into `runner.db` still exists at its
source. A fix release can redo a copy that turned out wrong, and a user who deletes or loses
`runner.db` gets it back by opening the app again. Shipping this file in the same release
removes that window on the one launch where it matters most. D28's D4 amendment leaves the
file unnamed for the same reason: naming it now bets on a merge order nobody knows yet.

The task file exists now so that the earlier tasks which say "065 drops it" point at
something, and so the backlog, imported into Rimaia by task 010's MCP tools, carries the
second half of the ADR as a `not-ready` card rather than forgetting it. The Phase 1 workflow
skips it.

### Before this task becomes ready

Each item is a decision or a fact that does not exist yet. The person who flips `status` to
`ready` records all four, in the commit that flips it:

1. **The team-mode release has shipped**, and its version is known. It appears in both
   refusal messages below as `<team-mode release>`.
2. **The migration is named.** A new D4 amendment in `docs/seam-contract.md` names one board
   file whose version sorts after every file then in `src-tauri/migrations/`. This task adds
   no runner-store file.
3. **The skip-release rule below is confirmed or replaced.** ADR-0028 says nothing about an
   install that jumps from a pre-team-mode build straight to this one. This task proposes to
   refuse it (Scope, "The precondition"). Record the decision as an amendment to D28, since
   the precondition is the other half of D28 part 2's "retired columns stay". Replacing it
   with "migrate through 038, adopt, then drop" is the alternative in Notes.
4. **The inventory is re-run** on `main` at that time. Grep `crates/*/src`, `crates/*/tests`
   (excluding `tests/fixtures`), `src-tauri/src` and `src` for the column list of 066's
   `no_board_query_reads_a_retired_column`, for `settings` and `schedules` as board tables,
   and for comments that name 041, 056, 065 or 066 as the task that retires something (the
   `"path!"` overrides of D28 part 2 name 066, not 065). Every hit not covered by Scope
   below, production code or test, is added to Scope first.

## Scope

**The migration.** One file in `src-tauri/migrations/`, under the name item 2 recorded. Its
first line is its title and does not begin with `-- no-transaction` (D28 part 1). Its header
comment, in the voice of the existing migrations, says what is dropped, cites ADR-0028 point
5, and says that the data went to `runner.db` (040, 041) or `transcript_key` (056). The body
is exactly:

```sql
ALTER TABLE repositories DROP COLUMN path;
ALTER TABLE repositories DROP COLUMN worktree_root;
ALTER TABLE repositories DROP COLUMN max_concurrency;
ALTER TABLE repositories DROP COLUMN credential_login;
ALTER TABLE repositories DROP COLUMN credential_label;
ALTER TABLE repositories DROP COLUMN credential_added_at;
ALTER TABLE repositories DROP COLUMN on_archive;
ALTER TABLE repositories DROP COLUMN on_archive_script;
ALTER TABLE tasks DROP COLUMN worktree_path;
ALTER TABLE runs DROP COLUMN log_path;
DROP TABLE schedules;
DROP TABLE settings;
```

No rebuild. D28 part 5 established that each column can go with `DROP COLUMN` (SQLite 3.35
and later; the crate bundles 3.46): none is indexed, part of a key, or named by a
table-level constraint, and `on_archive`'s own column `CHECK` does not block it. That was
measured against the schema as 038 leaves it. If a later file added an index, a trigger, a
view or a partial-index `WHERE` that names one of these columns, the statement fails. Then
stop and ask; do not reach for a rebuild. Nothing references `schedules` or `settings`, so
dropping them cascades into nothing, and D28 part 1's `foreign_keys` handling in
`db::apply_migrations` applies to this file unchanged.

`allow_unattended_runs` stays. It kept its name in 038 and now means the team ceiling
(ADR-0032 point 4). `runs.run_environment`, `runs.base_ref` and `tasks.branch` stay: they are
facts about a run or a task that mean the same on every machine.

**The precondition.** A new module, `rimaia_core::db::retired`:

- `BoardHistory { fresh, team_mode_applied, drop_applied, has_solo_identity }`, read by a
  private `read_history(&mut SqliteConnection)`. `fresh` is "`_sqlx_migrations` does not
  exist or has no row". `team_mode_applied` and `drop_applied` are whether that table holds
  a successful row for `20261003120000` and for this task's version. The versions are
  constants, and a unit test asserts that each equals a version in the embedded `MIGRATOR`,
  so a rename cannot leave the check reading a version that no longer exists.
  `has_solo_identity` is whether `solo_identity` exists and holds its row. These are
  unchecked `sqlx::query_scalar` calls, because the tables they read may not exist yet when
  they run. Each one carries a comment saying so, and each first looks the table up in
  `sqlite_master`.
- `may_drop_retired(&BoardHistory, adopted: Option<&BTreeSet<String>>) -> Result<()>`, a pure
  function:
  - `fresh` or `drop_applied`: `Ok`. There is nothing left to lose.
  - not `team_mode_applied`: refuse with the **skipped-release** message.
  - `has_solo_identity`, and `adopted` is `None` or lacks either `"settings"` or
    `"machine_state"`: refuse with the **not-copied** message.
  - otherwise `Ok`.

  `credential_keys` (054) is deliberately not required. It reads no board column, it can
  legitimately stay pending while the keychain is locked (054's `Unavailable` rule), and it
  keeps running after this task.
- `migrate_board(pool: &SqlitePool, adopted: Option<&BTreeSet<String>>) -> Result<()>`:
  `read_history` on a connection from the pool, then `may_drop_retired`, then
  `db::migrate`. **The check and the migration are one function**, so the order is tested
  where it is written, and no caller can migrate a board without the check. Outside
  `db::retired` and `testing/`, no production code calls `db::migrate`.
- The refusals are `Error::invalid` (D8: no new `ErrorCode`), with exactly these messages:
  - skipped release: `This board was last opened by a version of Rimaia older than
    <team-mode release>. Open Rimaia <team-mode release> once so it can copy this
    computer's settings, checkouts and worktrees out of the board, then install this
    version again. Nothing has been changed.`
  - not copied: `This computer's settings, checkouts and worktrees have not been copied out
    of the board into runner.db yet. Open Rimaia <team-mode release> once to copy them, then
    install this version again. Nothing has been changed.`

  The strings are fixed by the tests, and a test failure is the only way to change them.
  "Nothing has been changed" covers `runner.db` as well as the board.

**Reading `adoptions` without changing `runner.db`.** `RunnerStore::open` creates and
migrates `runner.db`. On a skipped-release install that would leave a store at this
release's runner schema, which the team-mode release the message sends the user to may
refuse as a missing migration (ADR-0023's collision). So the desktop reads `adoptions`
through a new `rimaia_runner::adopt::recorded_steps(runner_db: &Path) ->
Result<BTreeSet<String>>`. It opens the file read-only, with `create_if_missing(false)` and
not through `db::connect`, whose pragmas write. An absent file or an absent `adoptions`
table yields an empty set, by the same `sqlite_master` lookup and unchecked query as
`read_history`, with the same comment. It lives in `adopt` because 040 confines every query
naming `adoptions` there.

**Who calls it.**

- **The desktop**, through one function in `rimaia-runner`,
  `adopt::migrate_local_board(&SqlitePool, runner_db: &Path)`: `recorded_steps`, then
  `migrate_board` with `Some`. It replaces `db::migrate` at every site where the desktop
  migrates `rimaia.db`: the setup hook's solo path in `src-tauri/src/lib.rs`, under the D11
  step name `db::migrate` had, and task 059's `choose_solo` and disconnect step 4. The two
  059 sites only ever see a board file that did not exist before they ran, so the check
  passes there; they still go through it so that no desktop path migrates unchecked.
  `RunnerStore::open` stays where 040 put it, after the board is migrated. A refusal goes
  through `report_startup_failure` like every other step, so the window never opens and
  the message reaches the user by D11 as amended by task 025.
- **The server's startup** in `crates/server/src/main.rs` (046; 062 fixed the order in
  `main`) calls `migrate_board(&pool, None)` in place of `db::migrate`. A server's own board
  never adopts a solo identity (D28 part 3), so the check passes on every server board, and
  the `None` means a server pointed at a desktop's board is refused rather than allowed to
  drop that board's machine state.
- **The headless runner (058)** has no board and does not call it.

039's structural rules take two sanctioned edits. `no_service_takes_a_pool_without_a_scope`
gains one `STORE_HANDLE_EXCEPTIONS` entry, `db::retired::migrate_board`, with the reason
`db::migrate`'s entry gives: it runs before any context exists. `read_history` takes a
connection and is private, so it needs none. The shell's bare `pool` local is used by
`db::connect`, `migrate_local_board` (in place of `db::migrate`) and `identity::ensure_solo`;
039's text and any comment stating that list say so.

**What goes with the columns.**

- `machine::adoption::read_board` (041), and the `machine_state` step in
  `rimaia_runner::adopt`, together with that step's runner-side writes. 041's Notes require
  this to happen in the same change as the drop.
- `db::settings::runner_placed` and `db::settings::RUNNER_KEYS` (040). The `settings` step
  keeps its name, its `adoptions` row and its `runner_identity` write, and loses its copy. A
  fresh install still needs `runner_identity`, and that step is where 040 writes it.
- Every other function in `crates/core/src`, `src-tauri/src` or `crates/server/src` whose
  query names `settings` or `schedules` as a board table, including any private accessor 039
  left for the runner placement. `crates/runner/` is excluded: its own `schedules` and
  `runner_settings` are the runner store's tables and stay.
- Any field for a retired column left on `db::models::{Repository, Task, Run}` or on a type
  built from them, and any `"col!"` override or comment that names 041, 066 or 065 as the
  task that retires it.
- **The second set in `scheduler::reconcile::reconcile_unrecorded`** (043): tasks in
  `running` or `queued` with no lease row. 043 says 065 is where that query can go. Only a
  build older than 043 could leave such a task. The precondition refuses every board such a
  build could have left behind, and the team-mode release reconciled the rest at its first
  launch. The first set, leases the board records and `held_leases` does not, stays
  (ADR-0031). The function's doc is rewritten to say what is true now.
- 066's `no_board_query_reads_a_retired_column` loses its `machine_state adoption`
  exception (056 already removed the `log_path` one) and otherwise stays. The schema now
  enforces what it checks, and the test costs nothing.
- **The pre-054 keychain accounts.** 054's `credential_keys` copied each item to
  `<repository_id>/<runner_id>` and left the original under the bare repository id, so the
  release before this one could still be rolled back (ADR-0028 point 5). This release ends
  that window. A new adoption step, `credential_keys_retired`, appended after
  `credential_keys` in `rimaia_runner::adopt`, runs only once `credential_keys` has its
  `adoptions` row. For each checkout it deletes the item under the bare repository id. An
  item already gone is not an error, so an interrupted step runs again, and an
  `Unavailable` keychain leaves the step pending exactly as 054's rule does for
  `credential_keys`.

**Tests that used the legacy schema.** Each existing test that writes to or asserts on
something this task drops is handled by one rule, and the PR body lists every test touched,
under its rule:

1. **A test whose subject is a deleted function is deleted with it.** This covers 040's and
   041's copy cases in `the_runner_adopts_the_board_once` (054's `credential_keys` cases
   stay), `every_settings_key_lands_in_exactly_one_store`'s runner half, and 043's
   `a_task_left_running_by_a_build_without_leases_is_still_offered_for_resume` together
   with its `queued` → `cancelled` case.
2. **A test that builds an old board to test an earlier migration migrates only as far as
   that migration.** 038's `the_team_mode_rebuild_keeps_every_row` asserts retired values
   column by column, so it now applies board files through `20261003120000` with
   `db::apply_migrations` over a bounded `Migrator`, instead of `db::migrate`. That is the
   same helper `db::migrate` calls, so the test still exercises D28 part 1's `foreign_keys`
   handling. Its assertions do not change.
3. **A fixture that seeds a legacy row to prove it is ignored loses that row.** Examples are
   042's "a third value in the legacy `settings` table, which must appear nowhere", 041's
   board-side values in `runner_state_is_read_from_the_runner_store`, and 039's
   `a_team_setting_written_after_the_split_never_touches_the_legacy_table`. That last test's
   whole subject is gone, so it is deleted.
4. **An assertion that a retired column is NULL or unwritten, or a setup step that writes
   one, is deleted**, and the rest of its test stays. `the_board_has_no_retired_column`
   below now enforces the same thing at the schema level. Known cases, all 066's:
   `a_new_worktree_is_recorded_on_the_runner_and_not_the_board` (`tasks.worktree_path` is
   `NULL`); the registration case's "a freshly registered repository has `NULL` `path` and
   `worktree_root` on the board"; and `a_run_log_is_found_by_its_ids_not_its_column`, whose
   setup writes `runs.log_path`.

No other assertion changes, apart from the two 039 edits above.

**The caches.** Both offline caches are regenerated with D33 point 3's recipe and committed:
`crates/core/.sqlx/` because board queries were deleted, and `crates/runner/.sqlx/` because
the `machine_state` step's queries were.

**The docs.** The CLAUDE.md sentence 064 added, that retired columns exist until 065 and must
not be read, is removed. D28 gains a dated amendment (item 3's, extended, or a second one)
recording that the retired columns and both tables are dropped by the file item 2 named,
that part 6's "dropped by 065" comments are now history, and which of part 7's
`the_runner_adopts_the_board_once` cases rule 1 deleted. `docs/hosting.md`'s "deploying and
upgrading" section (062) gains one line: this release drops data irreversibly, so before
deploying it, confirm that a Litestream restore point exists from after the previous
release's deploy (ADR-0037 point 2). Where a later task moved either file, follow it.

## Out of scope

- **The runner store's schema.** No runner migration is added or edited. `adoptions` stays,
  because it is what the precondition reads. `checkouts`, `worktrees` and `schedules` in
  `runner.db` are where the data lives now.
- **The `credential_keys` step.** It stays, unchanged, for the reason given under the
  precondition.
- **Renaming `allow_unattended_runs`** to say "ceiling". A rename in SQLite is a drop and an
  add for every query that names it, and it buys nothing a comment does not.
- **Editing any earlier migration**, including 038's adoption SQL, which reads `settings`.
  On a fresh board it still runs before this file drops that table. Migrations are
  append-only (CLAUDE.md, D4).
- **Any rebuild.** D28 part 5 says none is needed. If one turns out to be needed, stop and ask.
- **Any change to the runner protocol, `/api/v1`, `Rimaia-Protocol`'s version, an MCP tool or
  a file under `src/`.** No board DTO has carried a retired column since 066 (its
  `no_board_dto_carries_an_absolute_path`). A diff that needs one of these changes has found
  a reader that 066 or 056 missed. Report it as a finding against that task.
- **Exporting a solo board into a team**, which ADR-0028's consequences leave undecided.
- **Migrating a pre-team-mode board in one launch.** This is the alternative in Notes, and
  it is taken only if item 3 above chooses it.

## Acceptance criteria

- The four items under "Before this task becomes ready" are recorded, and the D4 amendment
  names the one file this task adds. No other file is added to or edited in either migration
  set.
- The migration's body is exactly the twelve statements in Scope, and its first line is its
  title. 038's `no_migration_opts_out_of_its_transaction` covers it and passes.
- `the_retired_columns_drop_and_every_other_value_stays` runs against a real file in a
  `TempDir`. It builds a board at the schema immediately before this task's version. It uses
  the builder that 038's rebuild test uses, lifted into `crates/core/src/testing/` if 040 did
  not already lift it, and given the cut-off version as an argument, so both tests build old
  boards the same way. It fills every table, with every retired column set and every other
  nullable column set, and runs `migrate_board(&pool, None)` on the same file. It asserts
  that:
  - row counts are equal in every surviving table, and every surviving value is unchanged,
    column by column;
  - each table's column set equals its column set before, minus exactly the retired columns
    (`pragma_table_info`);
  - `schedules` and `settings` are absent from `sqlite_master`;
  - every index on `repositories`, `tasks` and `runs` is present with its `sql` in
    `sqlite_master` unchanged;
  - `pragma_foreign_key_check` is empty, and `PRAGMA foreign_keys` reads 1 on the pool's
    connections.
- `the_board_has_no_retired_column`, on `testing::db::test_pool`: none of the ten retired
  columns exists on its table, and neither dropped table exists.
- `may_drop_retired`'s pure cases, one test each:
  - `a_fresh_board_may_drop`;
  - `a_board_that_already_dropped_is_not_checked_again`;
  - `a_board_that_skipped_the_team_mode_release_is_refused`;
  - `a_solo_board_whose_machine_state_was_never_copied_is_refused`, for `settings` missing,
    `machine_state` missing, and `adopted` = `None`;
  - `a_pending_credential_keys_step_does_not_block_the_drop`;
  - `a_server_board_may_drop_with_no_runner_store`.

  Each refusal asserts the exact message from Scope, with `<team-mode release>` substituted.
- `the_pre_054_accounts_are_deleted_only_after_credential_keys_has_run` (`MemoryStore`):
  with `credential_keys` pending nothing is deleted; after it, every old account is empty
  and every new key still holds its item.
- `a_refused_board_is_left_exactly_as_it_was`, in `crates/runner/tests/`, against real files
  in a `TempDir`: a solo board at the pre-drop schema, and a `runner.db` whose `adoptions`
  lacks `machine_state`. `migrate_local_board` returns the not-copied refusal. Afterwards
  `_sqlx_migrations`, every retired column's values, `settings` and `schedules` are
  unchanged, and `runner.db` is byte-identical. A second case with no `runner.db` gets the
  same refusal, and `runner.db` is still absent. After the missing row is inserted, the
  same call returns `Ok` and applies the drop. Then `migrate_board(&pool,
  Some(&BTreeSet::new()))` also returns `Ok`, because the board has already dropped.
- `a_board_from_before_team_mode_is_refused_before_anything_migrates`: `migrate_board` on a
  board at the schema just before `20261003120000` returns the skipped-release message, and
  `_sqlx_migrations` holds no version newer than it held before.
- `a_fresh_install_still_records_which_runner_it_is`: in a fresh `TempDir` data directory,
  `migrate_local_board`, `identity::ensure_solo`, `RunnerStore::open` and `adopt_board` run
  in the setup hook's order. `runner_identity.runner_id` equals `solo_identity`'s
  `runner_id`, and `adoptions` holds `settings`.
- The version constants in `db::retired` each equal a version in the embedded `MIGRATOR`
  (unit test).
- `machine::adoption::read_board`, `db::settings::runner_placed`, `db::settings::RUNNER_KEYS`
  and the `machine_state` adoption step no longer exist. `reconcile_unrecorded` has no query
  for tasks without a lease row.
- `db::migrate` is called from no production code outside `db::retired`: not in
  `src-tauri/src`, `crates/server/src` or `crates/runner/src`. 039's structural tests pass
  with exactly the two edits Scope names.
- `grep -rnwE "(FROM|INTO|UPDATE|JOIN|TABLE)[[:space:]]+(settings|schedules)"
  crates/core/src src-tauri/src crates/server/src --exclude-dir=testing` finds nothing
  outside `#[cfg(test)]` modules, and the PR body shows its output. `testing/` and test
  modules are excepted because the old-board builder legitimately fills both tables before
  migrating. `crates/runner/` is not searched, because its `schedules` is the runner
  store's own.
- `grep -rnE "(//|--).*\b065\b" crates/*/src crates/*/tests src-tauri/src src
  --exclude-dir=fixtures` finds nothing. Migration files are not searched: they are
  append-only.
- Every test deleted or changed under "Tests that used the legacy schema" is listed in the PR
  body with the rule that applied, and no other assertion in the workspace changed.
  `crates/core/tests/scheduler.rs` passes, and apart from the tests rule 1 deleted its
  assertions are unchanged.
- `crates/core/.sqlx/` and `crates/runner/.sqlx/` were regenerated with D33 point 3's recipe
  and committed. No `.sqlx/` exists at the workspace root.
- No file under `src/` changed. The runner-protocol contract suite (052) passes against both
  adapters, unchanged.
- CLAUDE.md no longer says retired columns exist, D28 carries the amendment from Scope, and
  `docs/hosting.md` carries the deploy line.
- Every CI command in CLAUDE.md passes, as 064 left the list. This task adds none.
- **Needs a person, and the PR body carries it as a checklist.** Take a copy of a real
  `rimaia.db` and `runner.db` that the team-mode release has opened, and point
  `RIMAIA_DATA_DIR` at a scratch directory holding them. Launch. Repositories open their
  clones, worktrees are found, schedules fire, and `sqlite3 rimaia.db 'PRAGMA
  foreign_key_check'` prints nothing. Then launch against a copy of a pre-team-mode
  `rimaia.db` with no `runner.db`: the window does not open, the skipped-release message is
  shown, and no `runner.db` was created.

## Notes

**Read first.** ADR-0028 points 2, 3 and 5. Seam-contract D28: part 2's "Retired columns
stay", part 4's placement table (why `settings` can go whole), part 5's last paragraph (why
no rebuild), part 6's comments on the retired columns and "The runner set", and the D4
amendment, whose "Task 065's drop is deliberately left unnamed" this task's readiness
resolves. D33 point 3 is the cache recipe. D11 and 025's amendment to it describe the
refusal path. D8 means no new error code. Then the Notes of 040 (`runner_placed` "exists only
until 065"), 041 ("065 … must delete `machine::adoption::read_board` in the same change"),
043 (the second set of `reconcile_unrecorded`), 054 (`credential_keys` and its
`Unavailable` rule) and 059 (`choose_solo`, disconnect step 4).

**Files to start from.** These exist on `main` today: `crates/core/src/db/mod.rs`
(`MIGRATOR`, `migrate`; 038 adds `apply_migrations`), `crates/core/src/db/settings.rs`,
`crates/core/src/db/models.rs`, `crates/core/src/scheduler/reconcile.rs`,
`crates/core/src/startup.rs`, `crates/core/src/testing/db.rs`, `crates/core/tests/store.rs`,
`src-tauri/src/lib.rs` (the setup hook's D11 steps and `report_startup_failure`) and
`src-tauri/migrations/`. These arrive with the chain: `crates/core/tests/team_mode_board.rs`
(038's suggested home for its migration tests), `crates/runner/src/` with its `adopt` module
and `RunnerStore` (040), `machine::adoption` in `rimaia-core` (041),
`crates/server/src/main.rs` (046), 059's connection commands, and `docs/hosting.md` (062).
Where a later task put one of these somewhere else, follow that task.

**What the chain provides.** 038 relaxed the three `NOT NULL`s, copied team and user keys out
of `settings`, and left every retired column in place. 040 and 041 copied the runner keys,
the per-repository machine state, the worktree paths and `schedules` into `runner.db`, and
recorded each copy in `adoptions`. 039, 041 and 066 moved every reader, so after the
team-mode release only the two adoption reads name a retired column or a dropped table. 056
replaced `log_path` with `transcript_key` and `transcript_uploads`. 064 left CLAUDE.md and CI
describing the shipped team-mode release. That release, shipped and in users' hands, is the
real dependency. `depends_on` can only name the task that finishes it.

**What the next task expects.** Nothing in this backlog follows 065. After it, the board
holds no local path and no machine state, which is the state ADR-0028 point 2 describes.

**The alternative to refusing.** A pre-team-mode board could be carried through in one
launch. Build a `Migrator` bounded at `20261003120000`, apply it, open the runner store and
run the full adoption, and only then apply the rest. That keeps `read_board` and
`runner_placed` alive in this release, reading columns that the same launch then drops. They
would have to become unchecked queries, because the compile-time schema no longer has those
columns. They would also be tested only against old boards, indefinitely. Refusing costs a
user who skipped a release one extra install. The alternative costs permanent code with no
compile-time checking. This task proposes refusing. Item 3 is where a person decides.

**Residual edge.** The precondition proves that the team-mode release copied the machine
state. It does not prove that the same launch finished `reconcile_unrecorded`'s second set.
A launch that copied, then failed startup at reconcile, and was never opened again before
this release was installed, could leave a pre-043 `running` task. The next reconcile's first
set does not see it, because it has no lease. It stays `running` until someone moves it. The
precondition could also require a marker of a clean startup, but no such marker exists and
adding one is a runner migration this task does not have. Accept the edge, and name it in
the PR body.

**Size.** Medium. It is mostly deletion, but it spans `rimaia-core`, `rimaia-runner`,
`src-tauri` and the server crate. About 40 lines of SQL and header, 200 of `db::retired`,
`recorded_steps` and the call sites, and 350 of new tests. On top of that come the deletions
of the two adoption reads, their steps and their tests, fixture edits in five earlier tasks'
tests, two cache regenerations and the doc edits. If it runs over, the second set of
`reconcile_unrecorded` is the piece to cut. It is dead code rather than a wrong answer, and
it can go in a follow-up with its own number.
