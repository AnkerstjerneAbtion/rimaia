---
id: "041"
title: Machine state moves to the runner
milestone: v0.5
status: ready
depends_on: ["040"]
adrs: ["0028", "0033", "0035", "0032", "0031", "0021", "0027"]
size: L
landed: "#34"
---

# Machine state moves to the runner

## Goal

The runner's settings and schedules are read from `runner.db`, through a port that the rest
of the machine-local state then moves behind. This task delivers:

- the ten runner keys of [seam-contract D28](../docs/seam-contract.md) part 4, every reader
  and writer switched;
- the `schedules` table, every reader and writer switched;
- the whole `20261003130100_machine_state.sql` file and the whole `machine_state` adoption
  step, so `checkouts` and `worktrees` hold every existing install's values;
- the port, `rimaia_core::machine`, with all four groups of storage the file creates;
- the MCP split into a board router and a host-injected local router.

[ADR-0028](../docs/adr/0028-the-server-owns-the-board-and-each-runner-keeps-its-own-store.md)
point 2 places each of these facts, and task 040 built the store they go into. This task
fixes the shape of the one thing
[ADR-0027](../docs/adr/0027-a-server-for-the-board-and-runners-for-the-work.md) point 6
makes hard: the code that owns these facts' rules lives in `rimaia-core`, the queries that
store them live in `rimaia-runner` (D33 point 2), and `rimaia-core` must never depend on
`rimaia-runner`. The answer is a port in core, implemented by the runner and injected by the
host. The MCP tools that reconfigure one machine
([ADR-0035](../docs/adr/0035-mcp-when-the-board-is-remote.md) point 6) are injected the same
way, so a server that has no machine serves no machine tools.

Task [066](066-checkouts-and-worktree-records-move-to-the-runner.md) then moves the readers
of the per-repository and per-task half (checkouts, worktree records and log paths) and
makes "no board DTO carries an absolute path" true. Until 066 lands, those readers stay on
the board columns.

**Solo behaviour does not change.** A user on a migrated `rimaia.db` sees the same board, the
same settings, the same queue and the same MCP tools as before.

## Why now

Task 042 moves the runner loop into `rimaia-runner`. Everything that loop reads (queue state,
capacity, the run window, the usage-limit pause, the schedules) must already be runner state
by then. Otherwise 042 has to move storage and code together, and a mistake in either is
indistinguishable from a mistake in the other. D31's table assigns these rows to 041 for that
reason: `QueueHandle` verbs, `tick_schedules`, `capacity::resolve`, `pause::active_until` and
`settings::run_environment`. Its point 5 also says `run_task` keeps its `ServiceContext`
argument "until 041".

## Scope

**The migration.** Create `crates/runner/migrations/20261003130100_machine_state.sql` with
exactly D28 part 6's DDL for it (`checkouts`, `worktrees`, `held_leases`, `schedules`),
under a header comment in the voice of the existing migrations. Its first line is its
title. No file begins with `-- no-transaction` (D28 part 1). `held_leases` stays unused
until 043. Do not add or edit any other migration in either set (D4, and D28's D4
amendment). The file freezes when this task lands, which is why all of it lands here and
066 adds none.

**The port: `rimaia_core::machine`.** A new module, `crates/core/src/machine/`:

- `port.rs` defines `MachineStore`, the storage half of every runner-owned fact. It is
  object-safe and uses boxed futures, for the same reason `BoardPort` does (D31 point 2):
  no `async-trait`, which D6 forbids and D34 does not approve. It is held as
  `Arc<dyn MachineStore>`. It has four groups of methods, each doing storage only:
  - **settings:** get, set and clear a `runner_settings` key;
  - **checkouts:** list, get by repository id, insert, patch and remove;
  - **worktrees:** get by task id, list, record and forget;
  - **schedules:** one method per storage operation today's `schedule::` functions perform
    (list, get, insert, update, set enabled, record a fire, delete).
- `types.rs` defines `Checkout` (one row of `checkouts`, with `OnArchive` reused from
  `db::models`) and `WorktreeRecord` (`task_id`, `repository_id`, `path`, `fenced_at`).
  `Schedule` keeps its type in `schedule`.
- `context.rs` defines:

  ```rust
  pub struct MachineContext {
      pub store: Arc<dyn MachineStore>,
      pub clock: Arc<dyn Clock>,
      pub changes: broadcast::Sender<ChangeEvent>,
      /// The solo team, from `solo_identity`. Used only to build `ChangeEvent`s, which
      /// carry a team from 038 on, until 048 moves machine events to `LocalEvents`.
      pub event_team: TeamId,
  }
  ```

  It is the runner-side counterpart of `ServiceContext`. It has no pool, no team scope and
  no `MutationSource`, because a runner's own state has neither a team nor a door.
  `event_team` is not a scope: nothing reads it except the publish, and every publish site
  carries a comment naming 048. In solo, `changes` is the board context's sender, so the
  frontend receives exactly the events it receives today: `Settings` for a runner key and
  `Schedules(ids)` for a schedule.

**The rules stay in `rimaia-core`; only the storage moves.** The typed accessors stay where
D3 put them, next to the code that owns each key, and change only their first argument, from
the board context 039 gave them to `&MachineContext`:

- `scheduler::state::{queue_state, set_queue_state}`;
- `scheduler::pause::{active_until, note_usage_limit, clear}`;
- `scheduler::capacity::{configured, resolve, schedule_mode, set_schedule_mode,
  max_concurrency, set_max_concurrency}`;
- `schedule::window::{active, open, close}`;
- the `schedule::` CRUD functions and `enabled`;
- `db::settings::{run_environment, set_run_environment, onboarding_dismissed,
  set_onboarding_dismissed, doctor_dismissals, set_doctor_dismissals}`;
- `mcp::settings::{configured_port, set_configured_port}`;
- `worktree::cleanup::{auto_cleanup, set_auto_cleanup}`.

Parsing, defaults, validation and the publish after a write all stay in these functions. A
key's absent-means-default rule (for example, `run_environment` absent means `inherit`) is
still decided in exactly one place. They must stay in core, because the local MCP handlers
that call them are core code. `rimaia-runner` holds the `MachineStore` implementation and
nothing that decides a value. 042 moves the loop, not these rules (042 point 1).

039's runner-placement accessor in `db::settings`, and the doc comments it left naming 040 or
041, go away. `RUNNER_KEYS` and `runner_placed` stay, used only by 040's adoption step.

**Two implementations, one contract.**

- `rimaia-runner` implements `MachineStore` on 040's `RunnerStore`, with checked queries
  against the runner schema. This is the production implementation, and the only one.
- `rimaia-core`'s `testing` module adds `testing::machine::MemoryMachine`, an in-memory
  implementation for core's own tests. Core's tests cannot reach the runner crate: a
  dev-dependency cycle would give the test binary two copies of every core type.
- A contract suite, `crates/core/src/testing/machine_contract.rs`, exports
  `machine_store_contract!(Harness)`. It is invoked from
  `crates/core/tests/machine_store_memory.rs` and `crates/runner/tests/machine_store_sqlite.rs`
  (D31 point 13's pattern). The suite is the reason the in-memory store can be trusted, so
  it covers every constraint the schema enforces:
  - the `worktrees → checkouts` foreign key in both directions (an unknown checkout is
    refused, and so is removing a checkout that still has worktrees);
  - the primary keys;
  - absent against empty settings;
  - every nullable schedule column round-tripping.

  CLAUDE.md's "fake the clock, never fake git or the filesystem" still holds. This fakes a
  store, the fake is bound to the real one by a shared suite, and production never builds it.

**Which tests run where.** A test that asserts on `runner.db` lives in
`crates/runner/tests/`, over a real `RunnerStore` in a `TempDir` and a board built with
`rimaia_core::testing`. A test that asserts on behaviour (the queue waits, a window opens)
may run in core against `MemoryMachine`, and says "the machine store", not `runner.db`.

**Where the port is built.** Once in each place, like `BoardPort` (D31 point 8):

- `src-tauri/src/lib.rs` `setup()` puts 040's `RunnerStore` on `AppState` as
  `runner_store`, for runner-crate readers such as 049's `runner_identity` read. It also
  builds a `MachineContext` over the same store, the board's sender and the solo team, and
  puts it on `AppState` as `machine`;
- `testing::context` exposes `TestContext::machine()` over a `MemoryMachine`, sharing the
  test's clock, change channel and solo team.

`scheduler::build` takes the `MachineContext` as a parameter. `run_task` takes
`&MachineContext`, which is what D31 point 5 reserved its `ServiceContext` argument for.

One board use stays until task 044. `worktree::prepare` reads `fetch_task`, `repo::get` and
`base_ref::resolve` through a board `ServiceContext`, and it cannot give them up before
044's `RunContext::base` exists. So `run_task` and `plan_claimed` keep a board context for
that one call and nothing else, and 044 removes it. Any other use goes through the
`BoardPort` or is reported as a finding, not kept.

**The adoption step `machine_state`.** Append it to 040's step list in `rimaia_runner::adopt`
as one more `(name, fn)` entry, exactly as D28's "The runner set" describes:

- For each repository, the board's `path`, `worktree_root`, `max_concurrency`,
  `on_archive`, `on_archive_script`, `credential_*` and `created_at` go into `checkouts`,
  and `allow_unattended_runs` becomes `unattended_consent`.
- Every `tasks.worktree_path` becomes a `worktrees` row.
- `schedules` is copied whole.

It reads the board only through one `rimaia-core` function,
`machine::adoption::read_board(ctx: &ServiceContext)`. Like 040's `runner_placed`, it takes
the board context and ignores its scope, so 039's `no_service_takes_a_pool_without_a_scope`
needs no exception for it. Its doc comment names 065, which deletes it. Once 066 lands it is
the only board query that names a retired column, and its SQL carries the comment
`-- machine_state adoption (task 041; task 065 deletes this read)`. The step writes
`runner.db` in one transaction together with its `adoptions` row, runs only while that row
is absent, and never writes the board.

Two cases have no clone to map, and both are skipped, not guessed at:

- a repository whose board `path` is `NULL`;
- a worktree whose repository was skipped.

A repository whose `worktree_root` is `NULL` but whose `path` is set gets the default
`register` would have derived (`AppPaths::worktrees_dir()` joined with the slug).

**The ten keys switch readers.** They are `run_environment`, `mcp_port`, `max_concurrency`,
`schedule_mode`, `queue_state`, `active_run_window`, `usage_limit_pause_until`,
`worktree_auto_cleanup`, `doctor_dismissals` and `onboarding_dismissed`. Every production
reader and writer now goes through `MachineContext`:

- the queue and its handle (`scheduler/queue.rs`);
- D15's quit path;
- the schedule tick and the preflight (`schedule/fire.rs`, `schedule/preflight.rs`);
- `apply_retry_policy`'s usage-limit pause (D23);
- the runner's `run_environment` read;
- the MCP listener's port in `setup()` and in `set_mcp_port`;
- the doctor's dismissals and port check;
- `get_app_info`'s `onboardingDismissed`.

The board's copies of these rows stay in `settings` until 065 drops them, and nothing reads
them. A failing adoption step fails startup as D11 says, through 040's adoption path.

**Machine reactions to board actions.** Two board services trigger work on this machine:
archiving runs the repository's on-archive policy (ADR-0025, D26), and moving to `done` runs
D20.3's auto-removal. The second reads `worktree_auto_cleanup`, a runner key, so from this
task a board service cannot perform it on its own. ADR-0033 point 8 and D32's appendix make
both, in the end, the runner's reaction to a change event, which needs 054's follow-up. So
here:

- `tasks::archive_task`, `archive_tasks` and `move_task` do board work only.
- One core function per operation, taking `(&ServiceContext, Option<&MachineContext>, …)`,
  performs the board write and then, given a machine, the reaction. The Tauri command and
  the board MCP tool both call it: `Some` from the shell, and on the MCP side
  `self.local.as_ref().map(|l| &l.machine)`.
- With `None` the reaction is skipped and the archive report says `Nothing`. That is the
  server's behaviour until 054's follow-up.
- 034's `review::approve`, which runs the same auto-removal after its commit, and
  `review::reject`, whose worktree removal comes before its transaction and can refuse, take
  the same `Option<&MachineContext>` (046 point 3a relies on all five).

The archive reaction still reads its policy and paths from the board row here; 066 moves
those reads to the checkout and the worktree record. This keeps ADR-0006's
one-function-two-doors rule without letting the board services read runner state.

**The MCP split: board tools in core, machine tools injected by the host.**
`RimaiaServer`'s tools move into two `#[tool_router]` blocks, combined when the server is
built. The router name and the combination use rmcp's `#[tool_router(router = …)]` and `+`
on `ToolRouter`. Verify both against the locked rmcp 3.1.4 before relying on them.

- **The board router** needs only a `ServiceContext`, and five of its tools,
  `archive_task`, `archive_tasks`, `move_task` and 034's `approve_task` and `reject_task`,
  also use `self.local` when present. It
  holds every tool not listed below, including the ones 033–035 and 021 added.
- **The local router** holds the 22 tools that inspect, reconfigure or spawn on this machine:
  - `run_doctor`, `dismiss_onboarding`, `dismiss_doctor_warning`, `restore_doctor_warning`;
  - `get_repository_credential_status`;
  - `list_worktrees`, `get_worktree_auto_cleanup`, `set_worktree_auto_cleanup`;
  - `set_repository_on_archive`;
  - `get_run_capacity`, `set_schedule_mode`, `set_max_concurrency`,
    `set_repository_max_concurrency`;
  - `list_schedules`, `create_schedule`, `update_schedule`, `set_schedule_enabled`,
    `delete_schedule`, `preview_schedule_preflight`, `list_timezones`;
  - `plan_task_strategy` and `plan_tasks_strategy`. They are here because they spawn, which
    is the first of ADR-0035 point 6's two categories. 060 gives the hosted server their
    request form.

The local router's handlers stay in `crates/core/src/mcp/server.rs`. They reach this machine
only through a `LocalTools { machine: MachineContext, doctor: doctor::Environment, planner:
PlannerAccess }` value. `doctor` and `planner` stop being `RimaiaServer`'s own fields, because
they only ever served local tools. Core defines the handlers and the `MachineStore` trait;
the host decides whether a machine exists and supplies its store. `rimaia-core` gains no
dependency.

`mcp::build` and both constructors, the operator's and the run-scoped one D30 serves as
`rimaia-run`, take `local: Option<LocalTools>`:

- **The shell passes `Some`, to both.** `tools/list` is deliberately not filtered by scope
  (`mcp/scope.rs`'s header), and 055's proxy relays that same list. So a run is still
  offered all 22, and calling one is refused by `RunScope::authorize` with today's
  sentence, not reported as an unknown tool. `Tool::run_access` does not change: every one
  of the 22 is already `Refused` to runs.
- **`None` serves the board router alone.** This is what 046's server and 060's hosted `/mcp`
  pass. There, calling a local tool is an unknown-tool error.

The anti-drift test `every_registered_tool_has_a_run_scope_decision`
(`crates/core/tests/mcp_scope.rs`) iterates the combined router, so a tool added to either
block still needs a decision.

**The local-handler rule (D32 point 8, as amended 2026-10-04).** No handler this task adds
or rewrites, Tauri command or MCP tool, issues a board query of its own. Where one needs a
board fact, it calls a named core read function over `AppState.context`, and the PR lists
each such call for 059, which converts them.

## Out of scope

- **Every reader of the per-repository and per-task half:** the clone path, the worktree
  root, the per-repository cap, the consent, the archive policy, the credential metadata,
  `tasks.worktree_path`, `runs.log_path`, the DTO changes, the four new local commands and
  the frontend (066). This task copies those values; it switches no reader of them.
- **Moving the runner loop, `InFlight`'s rename, and the stricter-of-team-and-runner values
  for `max_turns` and `disallowed_tools`** (042).
- **Anything that writes `held_leases`**, the lease row and per-runner reconciliation (043).
- **The command registry and the `board`/`local` wrappers** (046), and **converting local
  handlers' remaining board reads** (059).
- **`LocalEvents`** (048). Machine events keep riding the board's channel under `event_team`.
- **The retired columns.** Nothing drops, relaxes or backfills them; 065 drops them.

## Acceptance criteria

- `crates/runner/migrations/20261003130100_machine_state.sql` exists, with D28 part 6's DDL
  for it unchanged and a header comment. No other migration was added or edited in either
  set. The runner set's copy of `no_migration_opts_out_of_its_transaction` — 040's
  `#[cfg(test)]` unit test in `crates/runner/src/store.rs`, under the core name D28 part 7
  gives it — covers the new file without being edited, and passes.
- `crates/core/.sqlx/` and `crates/runner/.sqlx/` were regenerated with D33 point 3's recipe
  and committed. No `.sqlx/` exists at the workspace root.
- `rimaia_core::machine::MachineStore` is object-safe and uses boxed futures. 040's
  `rimaia_core_does_not_depend_on_rimaia_runner` still passes, unchanged.
- `machine_store_contract!` runs green against both `MemoryMachine` and the runner's SQLite
  store. Its cases include:
  - `a_worktree_for_an_unknown_checkout_is_refused`;
  - `a_checkout_with_worktrees_cannot_be_removed`;
  - `an_absent_setting_reads_as_none`;
  - `every_schedule_column_round_trips`, with every nullable column exercised both set and
    `NULL`.
- `the_runner_adopts_the_board_once`, extended, passes against real files in a `TempDir`. It
  builds a board where:
  - one repository has every per-machine column set;
  - one repository has a `NULL` `worktree_root`;
  - one repository has a `NULL` `path`;
  - tasks have worktree paths;
  - there are two schedules.

  It then asserts that:
  - every copied value equals its source column by column;
  - `allow_unattended_runs` arrived as `unattended_consent`;
  - the `NULL`-path repository and its tasks' worktrees were skipped;
  - the `NULL` root got the derived default;
  - a `machine_state` row is in `adoptions`;
  - a second launch copies nothing;
  - the board is unchanged.
- `runner_state_is_read_from_the_runner_store`, in `crates/runner/tests/`, gives each of the
  ten keys one value in the board's `settings` and a different one in `runner_settings`. It
  asserts that:
  - every typed accessor returns the runner's value;
  - every setter changes `runner_settings` and leaves the board row byte-identical;
  - each setter publishes the same `ChangeEvent` it published before this task, carrying
    the solo team.
- `a_usage_limit_pause_is_held_by_the_runner` drives a usage-limit fixture stream through a
  run under a `TestClock`, in core against `MemoryMachine`. `usage_limit_pause_until` is then
  set in the machine store, the board's row is unchanged, and the queue does not start until
  the clock passes it. There is no `sleep`.
- `a_schedule_opens_its_window_from_the_runner_store`, in `crates/runner/tests/` with a
  `TestClock`: the fire records `last_fired_at` in `runner.db`, and `active_run_window` is
  written there.
- No assertion in `crates/core/tests/scheduler.rs` changes. Its fixture setup changes only
  where an accessor's first argument did.
- `mcp::build(…, None)` serves exactly the board router.
  `a_server_without_a_machine_lists_no_machine_tool` asserts that the tool list contains
  none of the 22 names listed in Scope, and that calling one is an unknown-tool error. With
  `Some`, the operator and the run-scoped server list every tool they listed before this
  task, and a run calling `run_doctor` gets `RunScope`'s refusal.
  `every_registered_tool_has_a_run_scope_decision` iterates the combined router.
- `move_task` to `done` and `approve`, each with auto-cleanup on, remove the worktree
  through both the command and the MCP tool, against real git in a `TempDir`. Through the
  core functions with no machine, they leave it.
- `run_task` takes `&MachineContext`. It holds a board `ServiceContext` only to pass to
  `worktree::prepare`, with a comment naming 044.
- Every production reader and writer of the ten runner keys and of `schedules` goes through
  `MachineContext`. D31's table rows assigned to 041 are done, and a dated D31 amendment
  describes the machine port: its module, `MachineContext` with `event_team`, the two
  implementations, the contract suite, and `LocalTools`.
- CLAUDE.md's Gotchas gain one line: machine state lives in `runner.db` behind
  `rimaia_core::machine`, and its rules stay in core. No CI step changes. 040 added the
  runner's test and clippy steps, and CLAUDE.md's command list still matches `ci.yml` line
  for line.
- Every CI check passes:
  - `npm run typecheck`, `npm run test`, `npm run build`;
  - `cargo test -p rimaia-core`, `cargo test -p rimaia-runner`;
  - `cargo fmt --all --check`;
  - `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
    `cargo clippy -p rimaia-runner --all-targets -- -D warnings`;
  - `cargo check --workspace --all-targets`;
  - `./scripts/check-command-wiring.sh`.
- **Needs a person (PR checklist):** on a copy of a real `rimaia.db`, launch with a scratch
  `RIMAIA_DATA_DIR`. The queue controls, capacity, the schedules, the doctor's dismissals
  and the MCP port show what they showed before the upgrade.

## Notes

**Seam entries to read:**

- D3: typed accessors stay with their owners.
- D4, and D28's D4 amendment: the migration name, and that it freezes when this task lands.
- D8: errors. D11: an adoption step failing at startup.
- D15 and D19–D22: the queue, capacity and the doctor. D23: the usage-limit pause.
- D24: schedules.
- D16: MCP casing, for the split routers. D30: the run-scoped constructor.
- D20 and D26: cleanup and archive, for the machine reactions.
- D28: parts 4, 6 and 7, and "The runner set".
- D31: points 5, 7, 8, 13 and 14.
- D32: point 8 and its 2026-10-04 amendment, and its Binds line for 041.
- D33: the recipe, and why runner queries cannot live in core. D34: no new dependency.

**Files to start from:**

- `crates/core/src/db/settings.rs`;
- `crates/core/src/scheduler/{state,pause,capacity,queue,inflight}.rs`;
- `crates/core/src/schedule/{mod,window,fire,preflight}.rs`;
- `crates/core/src/mcp/{server,scope,mod,settings,responses}.rs`;
- `crates/core/src/worktree/cleanup.rs`, and `crates/core/src/tasks/service.rs`'s archive and
  `move_task` paths that call `run_on_archive` and `auto_remove_on_done`;
- `crates/core/src/runner/{process,strategy}.rs`, `crates/core/src/doctor/{mod,checks}.rs`;
- `crates/core/src/testing/context.rs`, `crates/core/tests/{mcp_scope,scheduler}.rs`;
- `src-tauri/src/{lib,state}.rs`, and
  `src-tauri/src/commands/{queue,schedules,settings,doctor,mcp,app,tasks}.rs`.

**What 040 provides.**

- The `rimaia-runner` crate at `crates/runner/`.
- `RunnerStore::open`, and `runner.db` at `AppPaths::runner_db_file()`, migrated through
  `db::apply_migrations`.
- `runner_identity`, `runner_settings` and `adoptions`, and `adopt::adopt_board`'s step
  list, which the `machine_state` step joins.
- The `settings` step, which already copied the ten keys, and `db::settings::RUNNER_KEYS`.
- The second offline cache, and the CLAUDE.md and CI lines for both.

040 switched no reader, so on this branch the ten keys are still read from the board until
this task. 040, 041 and 066 ship in one release, so no user runs a build where a copy has
happened but its readers have not switched. A development data directory from between them
is disposable, so use a fresh `RIMAIA_DATA_DIR`.

**What the next tasks expect.**

- **066** reads checkouts and worktree records through this task's `MachineStore` methods,
  adds `list_checkouts` to the local router, and moves the archive reaction's inputs.
- **042** moves the loop into `rimaia-runner` without touching storage or these rules. It
  reads only through `MachineContext`, and adds the runner's `max_turns` and
  `disallowed_tools` overrides as two more `runner_settings` keys through the same accessors.
- **043** adds `held_leases` methods to `MachineStore` and its contract cases.
- **044** removes `run_task`'s and `plan_claimed`'s remaining board context.
- **046** carries the five machine-reaction functions on `BoardRequest.machine` (its point
  3a), so solo keeps the reactions after they become dispatcher handlers. Its server passes
  `None` for `LocalTools`.
- **048** replaces `MachineContext.changes` and `event_team` with `LocalEvents`.
- **059** converts the core read functions the local handlers call (D32's 2026-10-04
  amendment).
- **060** decides what the hosted `/mcp` serves for the two planning tools.
- **065** drops the retired columns. It must delete `machine::adoption::read_board` in the
  same change, because that read names them and would stop compiling.

**Size.** About 3,000 changed lines before the regenerated caches: the port, both
implementations and the contract suite about 1,300; adoption 400; the ten keys' reader and
signature churn 700; the MCP split 300; tests and fixtures 300. The per-repository and
per-task half was cut into 066 for this reason.
