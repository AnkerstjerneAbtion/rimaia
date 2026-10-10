---
id: "040"
title: The runner store
milestone: v0.5
status: ready
depends_on: ["039"]
adrs: ["0028", "0023", "0027", "0003"]
size: L
landed: "#34"
---

# The runner store

## Goal

Give the runner a store of its own. A new library crate, `crates/runner` (`rimaia-runner`),
owns a second SQLite file, `runner.db`, beside `rimaia.db` in the same data directory. It
has its own migration set under `crates/runner/migrations/` and its own offline query cache
under `crates/runner/.sqlx/` (seam-contract D33). On the first launch of this version in
solo mode, the runner copies the settings ADR-0028 point 2 assigns to a runner out of
`rimaia.db` and records which runner it is. After that it never copies them again.

**This task copies and moves nothing else.** Every reader of those settings still reads the
board's `settings` table when this task lands. Task 041 moves the readers, the
per-repository machine state and the worktree paths. `runner.db` is written once in 040 and
read by no production path.

## Why now

ADR-0028 point 3 puts everything that describes one machine into a store the board never
sees. Tasks 041 to 058 all write into that store: checkouts and worktrees (041), the leases
a runner holds (043), the checkout mapping (054), the outbox and transcript uploads (056),
and the headless binary that runs with no board file at all (058). None of them can start
until the crate, the file, the migrator and the second cache exist. Landing those on their
own, with the one copy that needs nothing but 038's identity, keeps 041's diff about moving
readers and not about build plumbing.

The cache split cannot wait for the first runner query either. D33's "Why" explains that one
`cargo sqlx prepare --workspace` describes every crate against one database, so the first
query in `rimaia-runner` breaks the existing recipe. The move, the new recipe, CLAUDE.md and
CI change together, in the task that adds the crate, as the plan requires of every task that
adds a crate.

It comes after 039 and not straight after 038 because it reads the board through a
`ServiceContext`, and 039 is the task that makes that the only way in: its
`no_service_takes_a_pool_without_a_scope` test forbids a new core function over a bare pool,
and its shell rule forbids `src-tauri/` handing one to core outside three named setup calls.

## Scope

**The crate.** `crates/runner/` with `Cargo.toml` (`name = "rimaia-runner"`, a library only;
the headless binary is 058's), `build.rs`, `src/lib.rs`, a store module and an adoption
module. The root `Cargo.toml` `members` becomes
`["crates/core", "crates/runner", "src-tauri"]` (D33 point 5).

- Dependencies are `rimaia-core` by path plus workspace entries that already exist (`sqlx`,
  `tokio`, `chrono`, `tracing`). Dev-dependencies are `rimaia-core` with
  `features = ["testing"]`, `tempfile = { workspace = true }` and `pretty_assertions = "1"`.
  The last is not a workspace entry: `crates/core/Cargo.toml` pins it directly, and the
  same requirement resolves to the version `Cargo.lock` already holds. No new crate enters
  `Cargo.lock` (D6, D34). The manifest names no Tauri crate: the headless runner (058) links
  this library with no display server.
- Errors are `rimaia_core::Error`, with `Error::invalid` or `Error::internal` and a message
  (D8). There is no second error type, so the shell keeps one type across its boundary.
- `build.rs` prints `cargo:rerun-if-changed=migrations`, for the reason
  `crates/core/build.rs` gives (D33 point 2, D4 amendment).

**The store.** A newtype `RunnerStore` wraps the runner's `SqlitePool` and the path it was
opened from. The newtype exists because the shell now holds two pools of the same Rust type,
and passing the board's pool where the runner's is expected must not compile. The path is
kept for the two messages that must name the file: the refusal below and the shell's log
line (D11). `RunnerStore::path()` returns it.

- `RunnerStore::open(path)` opens through `rimaia_core::db::connect`, so the pragmas are the
  board's by construction and not restated: WAL, `synchronous = NORMAL`, `foreign_keys`,
  `busy_timeout`. It then migrates through `rimaia_core::db::apply_migrations` (task 038,
  D28 part 1) with the runner's own
  `static MIGRATOR: Migrator = sqlx::migrate!("migrations")`, in
  `crates/runner/src/store.rs`. The migrator is private, and `open` is the only way to get a migrated store, which is the
  same argument `db::MIGRATOR`'s doc makes.
- The file is `AppPaths::runner_db_file()`, which is new in `crates/core/src/paths.rs` and
  returns `data_dir.join("runner.db")`. It sits beside `db_file()`, so `RIMAIA_DATA_DIR`
  relocates both files through the one `AppPaths::resolve` that already exists (ADR-0023,
  ADR-0028 point 4). The layout stays in one place. Core deriving a file name is not core
  depending on the runner crate.

**The migration.** `crates/runner/migrations/20261003130000_runner_store.sql`, with exactly
the three tables of D28 part 6: `runner_identity`, `runner_settings` and `adoptions`. The
header comment is written in the voice of the existing migrations. Its first line is its
title and never begins with `-- no-transaction` (D28 part 1). This is the only runner file
this task writes. `20261003130100_machine_state.sql` is 041's.

**Adoption.** `rimaia_runner::adopt::adopt_board(board: &ServiceContext, store:
&RunnerStore, solo: &SoloIdentity)`, run once per launch in solo mode. `board` is the
shell's one context, and its clock stamps every time adoption writes. `solo` is the
`SoloIdentity` that 038's `identity::ensure_solo` already returned to the shell. Adoption
never calls `ensure_solo` itself, because that function creates rows on a board that has
none, and adoption never writes the board.

It walks an ordered list of steps. Each step runs only while its `adoptions` row is absent,
writes `runner.db` in one transaction that also inserts that row, and never writes the board
(D28, "The runner set"). It publishes no `ChangeEvent`, because nothing on the board
changed. 040 ships the list with one step, `settings`. 041 appends `machine_state` and 054
appends `credential_keys`, each as one more entry, not a new mechanism.

The `settings` step:

- reads the runner-placed rows through one new `rimaia-core` function,
  `db::settings::runner_placed(ctx: &ServiceContext) -> Result<Vec<Setting>>`. It selects
  the `settings` rows and keeps those whose 038 `placement(key)` is `Placement::Runner`,
  which today are the ten keys in 038's `db::settings::RUNNER_KEYS`. **040 adds no key list
  of its own**; `placement` and `RUNNER_KEYS` are 038's, and a second list could drift from
  them. The function takes the context and ignores its scope, exactly as 039's machine-state
  readers do: these rows belong to no team (038 deliberately left them out of
  `team_settings`). Its doc comment says so and names 065 as the task that deletes it with
  the table. It is one query of its own, not a loop over 039's per-key runner accessor,
  because 041 deletes that accessor and this function outlives it;
- writes each row into `runner_settings` with its key and value byte for byte. A key absent
  on the board stays absent, because an absent key already means "the default" in every
  accessor (`run_environment`'s doc);
- writes `runner_identity` with `runner_id = solo.runner_id`, `server_url` NULL, and
  `created_at` from the board context's clock;
- inserts `adoptions ('settings', <now>)`.

The runner crate holds no query that names a board table (D33 point 2). Its own test code
included, see "Tests in the runner crate" in Notes.

**One refusal, on every solo launch, before any step runs.** D28's "The runner set" states
it, and this task implements it without restating the rule: if `runner_identity` exists and
its `runner_id` differs from `solo.runner_id`, `adopt_board` returns `Error::invalid` and
writes neither file. The message names both runner ids, the `runner.db` path
(`store.path()`), and the remedy D28 gives: move `runner.db` aside to start this machine
over, or restore the `rimaia.db` it was adopted against.

**The shell.** In `src-tauri/src/lib.rs`'s setup hook, after `db::migrate`, 038's
`identity::ensure_solo` and the construction of the `ServiceContext` (038 and 039 place it
there), and before `startup::survey`:

1. `RunnerStore::open(&paths.runner_db_file())`;
2. `adopt::adopt_board(&context, &store, &solo)`.

Each step fails like its neighbours: `log_startup_failure` and `report_startup_failure`,
under its own step name ("open the runner store" and "adopt this machine's state into the
runner store"), with `runner.db`'s path in the log line (D11). The second name is neutral on
purpose: the refusal copies nothing, and the name must still be true when it is the
refusal the user sees. Neither step touches `context.pool`, so 039's rule that `.pool`
appears in `src-tauri/src/` only beside `connect`, `migrate` and `ensure_solo` still holds.
`src-tauri/Cargo.toml` gains `rimaia-runner` by path. The store is **not** added to
`AppState`: nothing reads it yet, and 041, its first reader, adds it.

**The two caches (D33 points 1 to 6, all of them).**

- `git mv .sqlx crates/core/.sqlx`, then regenerate both caches with the D33 point 3
  recipe, verbatim. No `.sqlx/` is left at the workspace root.
- CLAUDE.md: the two command lines, the two layout rows and the rewritten `SQLX_OFFLINE`
  paragraph followed by the recipe, exactly as D33 point 4 words them. The
  `RIMAIA_DATA_DIR` gotcha gains that it relocates `runner.db` as well as `rimaia.db`.
- `.github/workflows/ci.yml`: `Clippy (runner)` after `Clippy` (Linux only) and
  `Test (runner)` after `Test` (all three operating systems), with job names unchanged
  (D33 point 5). CLAUDE.md's commands block and the workflow's steps agree line for line.
- ADR-0003 gains a short amendment under "The offline cache lives at the workspace root"
  saying that ADR-0028 and D33 supersede it (D33 point 6). The section itself is not
  rewritten.

## Out of scope

- **Moving any reader.** `run_environment`, `queue_state`, `max_concurrency` and the rest
  are still read from and written to `rimaia.db`'s `settings` table after this task. The
  scheduler, the doctor, the MCP port and Settings are untouched. That is 041's work, and
  040 and 041 ship in the same release (one branch, one PR), so no build exists that copies
  the settings and never reads the copy.
- `checkouts`, `worktrees`, `held_leases`, `schedules` and the `machine_state` step (041).
  The `credential_keys` step (054). `outbox` and `transcript_uploads` (056).
- The board. No board migration, no board query change beyond `runner_placed`, and no
  board row written or deleted. The retired rows stay until task 065.
- A key list, a placement rule or a settings accessor of 040's own. 038 owns placement; 039
  owns the accessors.
- The runner loop, `LocalSlot`, and the scheduler split (042). The headless binary and
  pairing (058). Writing `server_url`, which happens at pairing or connection (052, 059).
- Connected and headless modes. `adopt_board` is solo-only. A store opened without a board
  has no adoption to run, and 040 has no such caller.
- `rimaia-server`, and the check that it does not depend on `rimaia-runner` (046).
- A doctor row for `runner.db`. `checks::data_directory` already probes the directory both
  files live in.
- Anything under `src/`. No command is added, so `check-command-wiring.sh` is unaffected.

## Acceptance criteria

- `crates/runner` exists as the library crate `rimaia-runner`, is a workspace member, and
  its manifest names no Tauri crate. `Cargo.lock` gains no package other than
  `rimaia-runner` itself.
- **`rimaia-core` does not depend on `rimaia-runner`, and a test says so.**
  `rimaia_core_does_not_depend_on_rimaia_runner` in `crates/core/tests/` reads
  `crates/core/Cargo.toml` with `include_str!` and fails if it names `rimaia-runner` or
  `crates/runner` anywhere, in any dependency table. Cargo already refuses a cycle through
  `[dependencies]`, but it allows one through `[dev-dependencies]`, which is how
  `rimaia-core` depends on itself today, so the compiler alone does not enforce this.
- `crates/runner/migrations/20261003130000_runner_store.sql` exists with exactly D28 part
  6's three tables, columns, types, `CHECK`s and keys. No other file is in that directory.
  No file is added to `src-tauri/migrations/`.
- **`no_migration_opts_out_of_its_transaction`, the runner set's copy.** A `#[cfg(test)]`
  unit test beside the private `MIGRATOR` in `crates/runner/src/store.rs`, under the name
  D28 part 7 gives it. It lives there because core's test cannot reach the runner migrator,
  and making the migrator public to let it would undo the "`open` is the only way in"
  argument. This is how D28 part 7's "extended by 040 to the runner set" is realised. It
  iterates the runner migrator and asserts that `no_tx` is false for every migration, and
  that each file's first line is a `--` title that does not contain `no-transaction`.
- In `crates/runner/tests/store.rs`:
  - `the_runner_store_opens_with_the_boards_pragmas`: a `RunnerStore` opened on a file in
    a `TempDir` reads `journal_mode = wal` and `foreign_keys = 1`, and the file exists.
  - `a_second_launch_applies_no_further_runner_migrations`: opening the same file twice
    leaves `_sqlx_migrations` identical, and it lists exactly the runner set's versions and
    no board version.
- `the_runner_store_sits_beside_the_board_wherever_the_data_directory_resolves`
  (`paths.rs`): with no override, `runner_db_file()` is `<fallback>/runner.db`. With an
  absolute `RIMAIA_DATA_DIR`, both `db_file()` and `runner_db_file()` are under the
  override. A refused override refuses before either file is named.
- In `crates/runner/tests/adoption.rs`, each against real files in one `TempDir`, a board
  context built the way the shell builds it, and a `TestClock`:
  - **`the_runner_adopts_the_board_once`** (D28 part 7). The board has every one of the ten
    runner keys set, plus team keys.
    - After the first `adopt_board`, `runner_settings` holds exactly those ten rows with
      byte-identical values. `runner_identity` holds `(1, <solo.runner_id>, NULL, <clock
      now>)`. `adoptions` holds exactly `('settings', <clock now>)`.
    - A full dump of every board table (`settings`, `team_settings`, `user_settings`,
      `solo_identity`, `runners`) is equal before and after.
    - The test then changes a runner key on the board, advances the clock, and adopts
      again. Every `runner.db` table is unchanged, including `adoptions.adopted_at`, and so
      is every board table.
  - `a_fresh_install_adopts_its_identity_and_no_settings`: a board built by `db::migrate`
    and `identity::ensure_solo` alone adopts to an empty `runner_settings`, a
    `runner_identity` naming its solo runner, and one `adoptions` row. A key the board does
    not hold is absent from `runner_settings`, not stored as an empty string.
  - **`every_settings_key_lands_in_exactly_one_store`.** A board at the pre-038 schema,
    holding every key D28 part 4 lists (including a `strategy_default.<repository_id>`), is
    migrated by `db::migrate` and then adopted. Every key appears in exactly one of
    `team_settings`, `user_settings` and `runner_settings`, with its original value. 038's
    rebuild test and `every_settings_key_has_the_placement_the_migration_gave_it` already
    tie the migration's SQL lists to `placement`. This test extends that guarantee to the
    runner leg: every key `placement` calls `Runner` reaches `runner_settings`, and no key
    lands in two stores or in none.
  - `a_runner_store_from_another_board_is_refused`: a `runner.db` adopted against one
    board and then adopted against a second, freshly created board fails with an `Invalid`
    error whose message names both runner ids, the `runner.db` path and D28's remedy.
    Neither file changes.
- `db::settings::runner_placed` has a unit test in `rimaia-core`, on a `TestContext`,
  asserting that it returns exactly the rows whose `placement` is `Runner`, and none for an
  absent key.
- **039's structural tests pass unchanged.** `no_service_takes_a_pool_without_a_scope` gains
  no exception: `runner_placed` and every function in `crates/core/src/` this task adds take
  a context. `grep -rn "\.pool" src-tauri/src` still matches only `lib.rs`'s calls to
  `connect`, `migrate` and `ensure_solo`.
- **The runner crate's query macros name only runner tables.** No `sqlx::query!`,
  `query_as!` or `query_scalar!` in `crates/runner/`, in `src/` or `tests/`, names
  `settings`, `team_settings`, `user_settings`, `solo_identity`, `runners` or any other
  board table. `crates/runner/.sqlx/` therefore prepares against a database holding only
  the runner migrations, as D33 point 3 runs it.
- `runner_settings`, `runner_identity` and `adoptions` are named by no query outside
  `crates/runner/src/adopt*` and tests. No production path reads `runner.db` after
  adoption.
- **Needs a person, and the PR body carries it as a checklist:** `npm run tauri dev` with
  `RIMAIA_DATA_DIR=/tmp/rimaia-040` creates both `rimaia.db` and `runner.db` in that
  directory and nowhere else. A second launch copies nothing. Deleting `rimaia.db` and
  launching again shows the startup-failure dialog under "adopt this machine's state into
  the runner store", naming the `runner.db` path and the remedy.
- **The caches.** `crates/core/.sqlx/` and `crates/runner/.sqlx/` exist and are current.
  No `.sqlx/` exists at the workspace root, and
  `no_offline_query_cache_at_the_workspace_root` (D33 point 1) passes in
  `cargo test -p rimaia-core`.
- CLAUDE.md carries D33 point 4's edits word for word, and ADR-0003 carries point 6's
  amendment. `ci.yml` carries point 5's two steps, with no `sqlx-cli`, no
  `SQLX_OFFLINE_DIR` and no `prepare --check`.
- Every CI check passes, run with `SQLX_OFFLINE=true` exported: `npm run typecheck`,
  `npm run test`, `npm run build`, `cargo test -p rimaia-core`,
  `cargo test -p rimaia-runner`, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo clippy -p rimaia-runner --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`.

## Notes

**Read first.** ADR-0028 (all of it, and especially points 2 to 5). ADR-0023 (points 1, 2
and 5: the shell reads the variable, core decides, no default changes). ADR-0027 point 6
(the crate layout, and why "runner" names both `rimaia_core::runner` and this crate).
ADR-0003 (the pragmas `connect` sets and why, and the offline-cache section this task
amends). Seam entries: **D28** (part 1's `apply_migrations`, part 4's placement table, part
6's runner DDL, "The runner set" and its refusal, and part 7's tests), **D33** (all six
points; this task lands every one), the **D4 amendment** under D28 (the four runner file
names), D3, D5 and its amendment, D8, D10 and D11. Read D4 and D6 as prohibitions, and
**D34** as D6's team-mode amendment: none of its approvals is needed here, and no crate
enters `Cargo.lock`.

**Files to start from.**

- `crates/core/src/db/mod.rs`: `connect`, `migrate`, the private `MIGRATOR` and its doc,
  and after 038 `apply_migrations`.
- `crates/core/build.rs`: the `rerun-if-changed` argument to copy.
- `crates/core/src/paths.rs`: `AppPaths::resolve`, `db_file` and their tests.
- `crates/core/src/db/settings.rs`: after 038, `RUNNER_KEYS`, `placement` and `Placement`;
  after 039, the private per-placement accessors and the doc comments naming 040 or 041.
- `crates/core/src/identity/mod.rs` (038): `SoloIdentity` and `ensure_solo`.
- `crates/core/src/testing/context.rs` (`TestContext`), `crates/core/src/testing/db.rs` and
  `crates/core/src/testing/clock.rs` (`TestClock`).
- `src-tauri/src/lib.rs`: the setup hook from `AppPaths::resolve` to `startup::survey`,
  `log_startup_failure` and `report_startup_failure`.
- `src-tauri/Cargo.toml`, the root `Cargo.toml`, `crates/core/Cargo.toml` (the
  self-referencing dev-dependency and its comment, and the directly pinned
  `pretty_assertions`), `.github/workflows/ci.yml`, `CLAUDE.md` and
  `docs/adr/0003-sqlite-as-the-local-store.md`.

**What 038 and 039 provide, and what this task assumes about them.**
From 038: `db::apply_migrations(&Migrator, &SqlitePool)` as a public function, written for
this crate's use (D28 part 1: "which the runner store also calls");
`db::settings::RUNNER_KEYS` and `placement(key) -> Placement`; `SoloIdentity` and
`identity::ensure_solo`, whose result the shell keeps; and the SQL placement lists in
`20261003120000_team_mode_board.sql`. From 039: a context on every core function over the
board, the private runner-placement accessor `runner_placed` sits beside, and the
structural test that keeps it that way. **If `apply_migrations` is not public, stop and
ask. Do not add a second migration path.**

`every_settings_key_lands_in_exactly_one_store` needs a board at the pre-038 schema. Use the
builder that 038's `the_team_mode_rebuild_keeps_every_row` uses. It lives in a different
crate from this test, so lift it into `crates/core/src/testing/` behind the `testing`
feature rather than copying it, and have 038's test call the lifted one. The two tests
then cannot build two different old schemas.

**Tests in the runner crate.** They read board tables and runner tables side by side, which
is exactly where an implementer reaches for `query!`. Do not. D33 point 3 prepares
`rimaia-runner` with `--all-targets` against a database that holds only the runner
migrations, so a checked macro over `settings`, `team_settings`, `user_settings`,
`solo_identity` or `runners` anywhere in `crates/runner/`, a `#[cfg(test)]` module
included, fails the prepare with "no such table" and breaks the runner cache. Tests read
board tables through unchecked `sqlx::query` or `sqlx::query_as` (runtime strings, never
cached) or through `rimaia_core` functions and `rimaia_core::testing` helpers. The only
query macros in the crate name `runner_identity`, `runner_settings`, `adoptions` or
`_sqlx_migrations`. The adoption tests build their board context as the shell does:
`db::connect` on a file in the `TempDir`, `db::migrate`, `identity::ensure_solo`, then the
context with the solo scope and a `TestClock`. Put that in one helper in
`crates/runner/tests/common/mod.rs`, so the four adoption tests cannot drift from the shell
or from each other.

**The regeneration order matters.** Run D33 point 3's recipe exactly, including `rm -f
target/sqlx-prepare-*.db*` and the offline `cargo check -p rimaia-runner` between the two
prepares. Do not reach for `cargo sqlx prepare --workspace` from habit: it creates a root
`.sqlx/` even when it fails (D33, "Why the crate directory"), and the new root-cache test
will then fail. The board cache should come out as a pure rename, because no board query
changes except the one `runner_placed` adds. If `git diff -M --stat` on `crates/core/.sqlx/`
shows rewritten files beyond that one, a stale scratch database was used.

**The adoption is written for 041 to extend.** Its step list is data, not branches: `(name,
fn)` pairs in order, where the name is the `adoptions.step` string. 041 adds `machine_state`
and extends `the_runner_adopts_the_board_once` to cover it. Its board reader,
`machine::adoption::read_board`, takes the context for the same reason `runner_placed`
does. 054 adds `credential_keys`. A later launch of an install that already ran `settings`
runs only the new step. Write `adopt_board` so that case needs no code of its own.

**What 041 expects from this task.** The crate, `RunnerStore::open`, a migrated `runner.db`
at `AppPaths::runner_db_file()`, `runner_settings` populated on every existing install,
`runner_identity` set, the adoption step list, both caches, and the recipe in CLAUDE.md.
041 then adds `20261003130100_machine_state.sql`, keeps the store in `AppState`, and moves
every reader of `RUNNER_KEYS` from the board to `runner_settings`. At that point
`RUNNER_KEYS` and `runner_placed` become adoption-only.

**Size.** L, and comfortably one session. The crate and its tests are a few hundred lines.
The shell change is two setup steps. The `.sqlx` move is a rename. The runner cache is a
handful of JSON files. If the diff grows past about 1,500 lines, something from 041 has
crept in (a reader moved, or a `checkouts` table), and it should come out.
