---
id: "041"
title: Machine state moves to the runner
milestone: v0.5
status: ready
depends_on: ["040"]
adrs: ["0028", "0021", "0027"]
size: L
---

# Machine state moves to the runner

## Goal

Every fact that is only true on one machine is read from `runner.db`, and the board stops
reading any of them. This covers:

- the ten runner keys of [seam-contract D28](../docs/seam-contract.md) part 4;
- the `schedules` table;
- the per-repository half of `repositories`: the clone path, `worktree_root`,
  `max_concurrency`, the unattended consent, `on_archive`, `on_archive_script` and the
  `credential_*` metadata;
- `tasks.worktree_path`;
- `runs.log_path`.

When this lands, no board DTO carries an absolute path, and a test proves it.

[ADR-0028](../docs/adr/0028-the-server-owns-the-board-and-each-runner-keeps-its-own-store.md)
point 2 places each of these facts, and task 040 built the store they go into. This task moves
the readers and writers. It also fixes the shape of the one thing
[ADR-0027](../docs/adr/0027-a-server-for-the-board-and-runners-for-the-work.md) point 6
makes hard: the code that owns these facts' rules lives in `rimaia-core`, the queries that
store them live in `rimaia-runner` (D33 point 2), and `rimaia-core` must never depend on
`rimaia-runner`. The answer is a port in core, implemented by the runner and injected by the
host. The MCP tools that reconfigure one machine
([ADR-0035](../docs/adr/0035-mcp-when-the-board-is-remote.md) point 6) are injected the same
way, so a server that has no machine serves no machine tools.

**Solo behaviour does not change.** A user on a migrated `rimaia.db` sees the same board, the
same settings, the same queue and the same MCP tools as before, with one exception: a
narrower rule for changing a task's repository (D13, below).

## Why now

Task 042 moves the runner loop into `rimaia-runner`. Everything that loop reads (queue state,
capacity, the run window, the usage-limit pause, the schedules) must already be runner state
by then. Otherwise 042 has to move storage and code together, and a mistake in either is
indistinguishable from a mistake in the other. D31's table assigns these rows to 041 for that
reason: `QueueHandle` verbs, `tick_schedules`, `capacity::resolve`, `pause::active_until`,
`settings::run_environment`, `write_worktree_columns`, and the runner's half of
`ensure_unattended_runs_allowed`. Its point 5 also says `run_task` keeps its
`ServiceContext` argument "until 041".

Two later tasks depend on the absolute-path rule being true before they start. 046 serves
board commands over HTTP, where a path is meaningless to the caller and leaks one member's
filesystem layout to the team. 054 lets a repository exist on the board with no clone on
this machine. Both are easier to build on a board that already has no paths than to
retrofit.

## Scope

The work splits into two parts along a line that also works as a cut (see Notes). Part A
is storage, settings, schedules and the MCP split. Part B is per-repository and per-task
state and every surface that shows it.

### Part A — the port, the store, the ten keys, the schedules, the MCP split

**The migration.** Create `crates/runner/migrations/20261003130100_machine_state.sql` with
exactly D28 part 6's DDL for it (`checkouts`, `worktrees`, `held_leases`, `schedules`),
under a header comment in the voice of the existing migrations. Its first line is its
title. No file begins with `-- no-transaction` (D28 part 1). `held_leases` is created here
and stays unused until 043. Do not add or edit any other migration in either set (D4, and
D28's D4 amendment).

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
- `context.rs` defines `MachineContext { store: Arc<dyn MachineStore>, clock: Arc<dyn Clock>,
  changes: broadcast::Sender<ChangeEvent> }`. It is the runner-side counterpart of
  `ServiceContext`. It has no pool, no team scope and no `MutationSource`, because a runner's
  own state has neither a team nor a door. In solo, `changes` is the same sender as the board
  context's, so the frontend receives exactly the events it receives today:
  - `Settings` for a runner key;
  - `Schedules(ids)` for a schedule;
  - `Repositories(ids)` for a checkout;
  - `Tasks(ids)` for a worktree record.

**No rule moves into the store.** The typed accessors stay where D3 put them, next to the
code that owns each key, and change only their first argument, from the board context
039 gave them to `&MachineContext`. These include:

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
still decided in exactly one place. 039's runner-placement accessor in `db::settings`, and
the doc comments it left naming 040 or 041, go away: the runner placement is read through
`MachineStore`. `RUNNER_KEYS` and `runner_placed` stay, used only by 040's adoption step.

**Two implementations, one contract.**

- `rimaia-runner` implements `MachineStore` on 040's `RunnerStore`, with checked queries
  against the runner schema. This is the production implementation, and the only one.
- `rimaia-core`'s `testing` module adds `testing::machine::MemoryMachine`, an in-memory
  implementation for core's own tests. Core's tests cannot reach the runner crate: a
  dev-dependency cycle would give the test binary two copies of every core type.
- A contract suite, `crates/core/src/testing/machine_contract.rs`, exports
  `machine_store_contract!(Harness)`. It is invoked from
  `crates/core/tests/machine_store_memory.rs` and `crates/runner/tests/machine_store_sqlite.rs`.
  This is D31 point 13's pattern. The suite is the reason the in-memory store can be
  trusted, so it covers every constraint the schema enforces:
  - the `worktrees → checkouts` foreign key in both directions (an unknown checkout is
    refused, and so is removing a checkout that still has worktrees);
  - the primary keys;
  - absent against empty settings;
  - every nullable schedule column round-tripping.

  CLAUDE.md's "fake the clock, never fake git or the filesystem" still holds. This fakes a
  store, the fake is bound to the real one by a shared suite, and production never builds it.

**Where the port is built.** Once in each place, like `BoardPort` (D31 point 8):

- `src-tauri/src/lib.rs` `setup()` puts 040's `RunnerStore` on `AppState` as
  `runner_store`, for runner-crate readers such as 049's `runner_identity` read. It also
  builds a `MachineContext` over the same store and puts it on `AppState` as `machine`;
- `testing::context` exposes `TestContext::machine()` over a `MemoryMachine`, sharing the
  test's clock and change channel.

`scheduler::build` takes the `MachineContext` as a parameter. `run_task`'s `ServiceContext`
argument is replaced by `&MachineContext`, which is what D31 point 5 said it was reserved for
from 036 on. If 036–040 left any other use of that `ServiceContext` in `run_task`, that use
goes through the `BoardPort` or is reported as a finding, not kept.

**The adoption step `machine_state`.** Append it to 040's step list in `rimaia_runner::adopt`
as one more `(name, fn)` entry, exactly as D28's "The runner set" describes:

- For each repository, the board's `path`, `worktree_root`, `max_concurrency`,
  `on_archive`, `on_archive_script`, `credential_*` and `created_at` go into `checkouts`,
  and `allow_unattended_runs` becomes `unattended_consent`.
- Every `tasks.worktree_path` becomes a `worktrees` row.
- `schedules` is copied whole.

It reads the board only through one `rimaia-core` function, `machine::adoption::read_board`.
This is the only board query left that names a retired column, and its SQL carries the
comment `-- machine_state adoption (task 041; task 065 deletes this read)`. It writes
`runner.db` in one transaction together with its `adoptions` row, and runs only while that
row is absent. It never writes the board.

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
- `apply_retry_policy`'s usage-limit pause;
- the runner's `run_environment` read;
- the MCP listener's port in `setup()` and in `set_mcp_port`;
- the doctor's dismissals and port check;
- `get_app_info`'s `onboardingDismissed`.

The board's copies of these rows stay in `settings` until 065 drops them, and nothing reads
them.

**The MCP split: board tools in core, machine tools injected by the host.**
`RimaiaServer`'s tools move into two `#[tool_router]` blocks, combined when the server is
built. The router name and the combination use rmcp's `#[tool_router(router = …)]` and `+`
on `ToolRouter`. Verify both against the locked rmcp 3.1.4 before relying on them.

- **The board router** needs only a `ServiceContext`. It holds every tool not listed below,
  including the ones 033–035 and 021 added.
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

  Part B adds a 23rd, `list_checkouts`.

The local router's handlers stay in `crates/core/src/mcp/server.rs`. They reach this machine
only through a `LocalTools { machine: MachineContext, doctor: doctor::Environment, planner:
PlannerAccess }` value. `doctor` and `planner` stop being `RimaiaServer`'s own fields, because
they only ever served local tools.

`mcp::build` and both constructors take `local: Option<LocalTools>`:

- **The shell passes `Some`.** It builds `LocalTools` from `AppState`.
- **`None` serves the board router alone.** This is what 046's server and 060's hosted `/mcp`
  will pass.

**"Injected by the host" means exactly that.** Core defines the tool handlers and the
`MachineStore` trait. The host decides whether a machine exists and supplies its store.
`rimaia-core` gains no dependency.

**`Tool::run_access` does not change.** Every one of the 22 is already refused to runs, and
the run door's local router carries the same table. The anti-drift test
`every_registered_tool_has_a_run_scope_decision` (`crates/core/tests/mcp_scope.rs`) iterates
the combined router, so a tool added to either block still needs a decision.

### Part B — checkouts, worktree records, log paths, and the surfaces that show them

**`Repository` splits in two.**

- **The board type keeps** `id`, `team_id`, `name`, `default_branch`, `created_at` and
  `review_config`. It keeps `allow_unattended_runs` in the struct, because it is the team
  ceiling 038 kept (ADR-0032 point 4). The ceiling is **not** serialized to the frontend or to
  MCP until 045 names it.
- **Everything machine-local moves to `machine::Checkout`.** This also carries out D31's note
  that "041 and 054 narrow" `RunContext::repository` to the board's pathless row.

Every function that needs a clone path, a worktree root, a per-repository cap, the consent,
the archive policy or the credential metadata takes the `Checkout` from `MachineContext`:

- `repo::{remote_info, gh_status, path_problem}`;
- `worktree::{locate, prepare, remove, reconcile}` and `worktree::cleanup`;
- `archive::{run_on_archive, set_repository_on_archive}`;
- `credentials::inject`, and `repo::{set,clear}_credential_metadata`;
- `scheduler::capacity`'s per-repository caps;
- `doctor`'s per-repository checks;
- `openers`;
- `startup::survey`.

A repository with no checkout on this machine is a legitimate state (ADR-0033 point 2):

- every reader answers it with `Error::invalid` naming the repository and saying it is not
  set up on this computer;
- nothing panics;
- a starter refuses before any run state is written.

**Registration and removal in solo.** `register_repository` stays a local command until 054
splits it (D32 appendix). In solo it:

- validates the path as it does today;
- checks for duplicates against `checkouts.path`, no longer against the board;
- writes the board row with `path` and `worktree_root` `NULL`;
- then writes the checkout.

`remove_repository` removes the checkout after the board row. Both are one core function
each, so the command and any later door share them.

`update_repository` loses `worktreeRoot`. It becomes the new local command
`set_repository_worktree_root`, per D32's binding for this task.

`set_repository_unattended_runs` writes the checkout's `unattended_consent` only.
`ensure_unattended_runs_allowed` reads the consent from the checkout. The board's ceiling is
not read here: D31's table gives the ceiling check on the claim to 045.

**Worktree records.** `write_worktree_columns` retires, and its two halves go separately:

- **The path** goes to `MachineStore::{record_worktree, forget_worktree}`.
- **The branch** goes through `BoardPort::record_branch` wherever a lease exists: from
  `worktree::prepare`, reached from `run_task` and `plan_claimed`. This gives `record_branch`
  its first production caller (D31 point 7).

Two branch writes have no lease: the branch clear in `worktree::remove` when
`delete_branch` is set, and `worktree::reconcile`'s retained branch at startup. Both keep a
board write through one named core function, `worktree::clear_branch`, which is not a query
of their own. Each carries a comment naming its later owner: 054's `report_runner` for the
first, 043's per-runner reconcile for the second.

No code writes `tasks.worktree_path` any more, and no board query reads it.

**D13 no longer reads a worktree.** A board rule cannot see a path.
`ensure_repository_is_reassignable` now refuses when the task has a recorded `branch` or
any run. The branch is the board-side fact that ADR-0005 ties to one repository, and the
same act that recorded it created the worktree. The refusal sentence becomes, exactly:

`cannot move "<title>" to another repository: it already has a branch, <branch>, in <repository name>`

This is a real narrowing. A task whose worktree was removed with its branch kept, and which
has no runs, used to be reassignable, and is not any more. That is intended: its branch is
still in the old repository. Amend D13 in the same commit, with that sentence as its Why.
`RepositorySelector.tsx` computes its disabled reason from `task.branch`.

**Log paths are derived, not stored.** Every reader of `runs.log_path` computes the path
with `runner::events::transcript_path(paths, task_id, run_id)`, which is ADR-0013's layout.
The readers are:

- `runs::{list, get, log_path_to_reveal, prune}`;
- `startup::survey`'s missing-transcript check;
- the three transcript reads.

`start_run` keeps writing the column as D31 point 4 says, until 056's `transcript_key`
replaces it, but nothing reads it. The two unchecked `SELECT log_path` queries in
`runs/mod.rs` go away.

**No board DTO carries an absolute path.**

- `Repository` (Rust, `src/types.ts`, MCP `RepositoryView`) loses `path`, `worktreeRoot`,
  `maxConcurrency`, `onArchive`, `onArchiveScript` and `allowUnattendedRuns`.
- `Task`, `TaskSummary` and `TaskDetail` lose `worktreePath`, and the MCP `TaskView` loses
  `worktree_path`. Amend D12's field list in the same commit.
- `Run` loses `logPath`.

If 033's `runs::get` still computes a diff from the live worktree, that computation leaves
the board read. `get_diff_summary` is the local command for it, and the board read answers
from the bundle.

**Four new local commands, and the frontend.**

| Command | Returns / does | Replaces |
| --- | --- | --- |
| `list_checkouts` | `CheckoutView[]`: `repositoryId`, `path`, `worktreeRoot`, `maxConcurrency`, `unattendedConsent`, `onArchive`, `onArchiveScript` | the per-machine fields `Repository` lost |
| `set_repository_worktree_root` | `{ repositoryId, worktreeRoot }` → `CheckoutView` | `update_repository`'s `worktreeRoot` |
| `list_local_worktrees` | `{ taskId, path }[]` from `worktrees` | `worktreePath` on task DTOs |
| `get_run_log_path` | `{ taskId, runId }` → the derived path | `Run.logPath` |

`list_checkouts` is also a local MCP tool, refused to runs. The operator's MCP client could
read a repository's path, cap and consent through `list_repositories` until now, and ADR-0021
does not allow that capability to disappear. The other three have no tool. Record them as
ADR-0021 point 1 gaps in their appendix rows, the way D32 point 9 records the existing ones.

Append a row for each of the four to D32's appendix. D32 point 8 requires this of every
task before 046 that adds a command. Add each wrapper to `src/lib/commands.ts`, still
through the private `call<T>` (046 splits it).

Two hooks, `src/hooks/useCheckouts.ts` and `src/hooks/useLocalWorktrees.ts`, re-read on the
events above, subscribing through `src/lib/events.ts` (D7). Components join by id:

- `RepositoriesSection`, `ConcurrencySection`, `OnArchiveFields` and `WelcomeView` read the
  per-machine fields from the checkout;
- `TaskCard`, `TaskDetailPanel`, `RunInfoSection` and `OpenInMenu` read `worktreePath` from
  the local worktrees;
- `RunOutcomeSection` and `RunDetailOverlay` fetch the path on the copy action.

Each has a Vitest test covering the joined state and the "not set up on this computer" state.

**Machine reactions to board actions stay synchronous in solo.** Two board services trigger
work on this machine today:

- archiving runs the repository's on-archive policy (ADR-0025);
- moving to `done` runs D20.3's auto-removal.

ADR-0033 point 8 and D32's appendix make both of them, in the end, the runner's reaction to a
change event, with the result reported back. That needs 054's `report_runner`, and until
then 030's archive report has nowhere to come from.

So in 041:

- `tasks::archive_task`, `archive_tasks` and `move_task` do board work only.
- One core function per operation, taking `(&ServiceContext, Option<&MachineContext>, …)`,
  performs the board write and then, given a machine, the reaction. The Tauri command and
  the board MCP tool both call it: `Some` from the shell, and on the MCP side
  `self.local.as_ref().map(|l| &l.machine)`.
- With `None` the reaction is skipped, and the report says `Nothing`. That is the server's
  behaviour until 054.

This keeps ADR-0006's one-function-two-doors rule without letting the board services read
runner state.

## Out of scope

- **Moving the runner loop, `InFlight`'s rename, and the stricter-of-team-and-runner values
  for `max_turns` and `disallowed_tools`** (042).
- **Anything that writes `held_leases`**, the lease row and per-runner reconciliation (043).
  `startup::survey` still runs globally in 041; only its reads move.
- **The unattended ceiling:** its board command, its check on the claim, and its value for a
  repository registered after this task (045). Until then `register_repository` leaves the
  board column at its default and nothing reads it.
- **The command registry, `board`/`local` wrappers, and routing local handlers' remaining
  board reads through a dispatcher** (046). D32 point 8's rule is followed here as far as it
  can be without a dispatcher. No handler this task writes or rewrites queries the board pool
  itself. Where one needs a board fact, it calls a named core read function, and the PR lists
  each such call for 046.
- **Mapping a checkout by remote, `normalized_remote`, re-keying keychain items per runner,
  asynchronous archive cleanup with reported results, and `report_runner`** (054). The
  keychain stays keyed by repository id.
- **`transcript_key`, the outbox and `transcript_uploads`** (056). `logAvailable` on run DTOs
  stays a boolean computed from the derived path.
- **The retired columns.** Nothing drops, relaxes or backfills them; 065 drops them.
- **Worktree records for deleted tasks.** A record whose task the board no longer has is
  invisible, as a deleted task's worktree directory is today. The inventory and the survey
  ignore it.

## Acceptance criteria

**Part A**

- `crates/runner/migrations/20261003130100_machine_state.sql` exists, with D28 part 6's DDL
  for it unchanged and a header comment. No other migration was added or edited in either
  set. 040's `no_runner_migration_opts_out_of_its_transaction` covers it and passes.
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
- `runner_state_is_read_from_the_runner_store` gives each of the ten keys one value in the
  board's `settings` and a different one in `runner_settings`. It asserts that:
  - every typed accessor returns the runner's value;
  - every setter changes `runner_settings` and leaves the board row byte-identical;
  - each setter publishes the same `ChangeEvent` it published before this task.
- `no_board_query_reads_a_retired_column` parses every `query` in `crates/core/.sqlx/*.json`.
  It fails on any match of `worktree_path`, `worktree_root`, `log_path`, `on_archive`,
  `on_archive_script` or `credential_(login|label|added_at)`, or of `path` or
  `max_concurrency` as a whole word naming a `repositories` column. The one exception is a
  query carrying the `machine_state adoption` marker.
- `a_usage_limit_pause_is_held_by_the_runner` drives a usage-limit fixture stream through a
  run under a `TestClock`. `usage_limit_pause_until` is then set in the runner store and
  absent from the board, and the queue does not start until the clock passes it. There is
  no `sleep`.
- `a_schedule_opens_its_window_from_the_runner_store` passes with a `TestClock`: the fire
  records `last_fired_at` in `runner.db`, and `active_run_window` is written there.
- No assertion in `crates/core/tests/scheduler.rs` changes. Only its fixture setup changes,
  where a setter's first argument did.
- `mcp::build(…, None)` serves exactly the board router.
  `a_server_without_a_machine_lists_no_machine_tool` asserts that the tool list contains
  none of the 22 names listed in Scope (23 with Part B's `list_checkouts`), and that
  calling one is an unknown-tool error.
  `every_registered_tool_has_a_run_scope_decision` iterates the combined router. The combined
  router lists every tool it listed before this task, plus `list_checkouts` once Part B lands.
- `run_task` takes no `ServiceContext`.

**Part B**

- `no_board_dto_carries_an_absolute_path` builds a board through real services:
  - a repository registered from a real git clone in a `TempDir`;
  - a task whose worktree was prepared by `worktree::prepare`;
  - a run finished from a recorded fixture stream;
  - a plan and titles that contain no `/` or `\`.

  It serializes the result of `list_repositories`, `update_repository`, `create_task`,
  `get_task`, `list_tasks`, `update_task`, `archive_task`, `list_runs_for_task`, `list_runs`
  and `get_run`, and of the MCP tools `list_repositories`, `get_task` and `list_tasks`. It
  walks every JSON string and asserts that none is an absolute path
  (`Path::is_absolute`, or a Windows drive prefix) and that none contains the `TempDir`'s
  path, raw or canonicalized.
- `a_new_worktree_is_recorded_on_the_runner_and_not_the_board` uses real git in a `TempDir`.
  After `worktree::prepare`:
  - `worktrees` holds the path;
  - `tasks.branch` holds the branch, written through `BoardPort::record_branch`;
  - `tasks.worktree_path` is `NULL`.
- `a_repository_not_set_up_on_this_computer_refuses_to_run` covers a board repository with
  no checkout. Run now refuses with the "not set up on this computer" sentence and leaves no
  claim, no worktree and no `runs` row. `get_worktree_status` returns the same refusal and
  does not panic.
- `an_unattended_run_needs_this_runners_consent` covers two cases:
  - ceiling `1` and consent `0` is refused before any run state is written;
  - ceiling `0` and consent `1` proceeds.

  The second case pins that 041 does not yet read the ceiling.
- `a_task_with_a_recorded_branch_cannot_change_repository` asserts the D13 refusal sentence
  exactly. A task with no branch and no runs still moves. D13 carries a dated amendment.
- Registering the same clone twice is refused from `checkouts`. A freshly registered
  repository has `NULL` `path` and `worktree_root` on the board and a complete checkout.
  `update_repository` rejects nothing it used to accept apart from the removed
  `worktreeRoot`, and `set_repository_worktree_root` sets that value.
- `a_run_log_is_found_by_its_ids_not_its_column` sets `runs.log_path` to a path that does
  not exist, then checks that the transcript page, search, summary, reveal and prune all
  still find the file at the derived path.
- Archiving with `remove_worktree` through the command removes the worktree and reports
  `WorktreeRemoved`. Through the board function with no machine, it reports `Nothing` and
  leaves the worktree. Moving to `done` with auto-cleanup on removes the worktree through
  both the command and the MCP tool. Both cases run against real git in a `TempDir`.
- The four local commands are defined, registered, wrapped in `commands.ts`, and have rows
  in D32's appendix. `./scripts/check-command-wiring.sh` passes.
- `src/types.ts` carries no `path`, `worktreeRoot` or `logPath` on `Repository`, `Task*` or
  `Run`. Vitest covers each changed component's joined state and its "not set up on this
  computer" state.

**Both parts**

- Every production reader of the ten runner keys and of the machine-local columns goes
  through `MachineContext`. D31's table rows assigned to 041 are all done, and a dated D31
  amendment describes the machine port: its module, `MachineContext`, the two
  implementations, the contract suite, and `LocalTools`.
- CLAUDE.md's Gotchas gain one line: machine state lives in `runner.db` behind
  `rimaia_core::machine`, and no board DTO carries an absolute path. No CI step changes.
  040 added the runner's test and clippy steps, and CLAUDE.md's command list still matches
  `ci.yml` line for line.
- Every CI check passes:
  - `npm run typecheck`, `npm run test`, `npm run build`;
  - `cargo test -p rimaia-core`, `cargo test -p rimaia-runner`;
  - `cargo fmt --all --check`;
  - `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
    `cargo clippy -p rimaia-runner --all-targets -- -D warnings`;
  - `cargo check --workspace --all-targets`;
  - `./scripts/check-command-wiring.sh`.
- **Needs a person (PR checklist):** on a copy of a real `rimaia.db`, launch with a scratch
  `RIMAIA_DATA_DIR`. Settings → Repositories, the queue controls, the schedules, a card's
  worktree badge, and "copy log path" all show what they showed before the upgrade.

## Notes

**Seam entries to read:**

- D3: typed accessors stay with their owners.
- D4, and D28's D4 amendment: the migration name, and that it freezes when this task lands.
- D12 and D13: both are amended here.
- D15 and D19–D22: the queue, capacity and the doctor.
- D24: schedules.
- D25: credentials.
- D20 and D26: cleanup and archive.
- D28: parts 4, 6 and 7, and "The runner set".
- D31: points 5, 7, 8, 13 and 14.
- D32: points 8 and 9, the appendix, and its Binds line for 041.
- D33: the recipe, and why runner queries cannot live in core.

**Files to start from:**

- Settings and state:
  - `crates/core/src/db/settings.rs`;
  - `crates/core/src/scheduler/{state,pause,capacity,queue,inflight}.rs`;
  - `crates/core/src/schedule/{mod,window,fire,preflight}.rs`;
  - `crates/core/src/mcp/{server,scope,mod,settings,responses}.rs`.
- Worktrees, repositories and runs:
  - `crates/core/src/worktree/{mod,cleanup,safety}.rs`;
  - `crates/core/src/repo/mod.rs`;
  - `crates/core/src/archive/mod.rs`;
  - `crates/core/src/credentials/inject.rs`;
  - `crates/core/src/doctor/{mod,checks}.rs`;
  - `crates/core/src/openers/mod.rs`;
  - `crates/core/src/runs/mod.rs`;
  - `crates/core/src/startup.rs`;
  - `crates/core/src/tasks/service.rs`: `ensure_repository_is_reassignable`, and the
    archive and `move_task` paths that call `run_on_archive` and `auto_remove_on_done`;
  - `crates/core/src/runner/{process,events}.rs`: `transcript_path`.
- Core types and tests:
  - `crates/core/src/db/models.rs`: `Repository`, `Task`, `Run`;
  - `crates/core/src/testing/context.rs`;
  - `crates/core/tests/{mcp_scope,scheduler}.rs`.
- The shell:
  - `src-tauri/src/{lib,state}.rs`;
  - `src-tauri/src/commands/{repositories,worktree,queue,schedules,settings,doctor,mcp,runs,app}.rs`.
- The frontend:
  - `src/types.ts`, `src/lib/commands.ts`;
  - the components named in Part B.

**What 040 provides.**

- The `rimaia-runner` crate at `crates/runner/`.
- `RunnerStore::open`, and `runner.db` at `AppPaths::runner_db_file()`, migrated through
  `db::apply_migrations`.
- `runner_identity`, `runner_settings` and `adoptions`, and `adopt::adopt_board`'s step
  list, which the `machine_state` step joins.
- The `settings` step, which already copied the ten keys, and `db::settings::RUNNER_KEYS`.
- The store is not on `AppState` yet. This task puts it there.
- The second offline cache, and the CLAUDE.md and CI lines for both.

040 switched no reader, so on this branch the ten keys are still read from the board until
this task. The two tasks ship in one release, so no user runs a build where the copy has
happened but the readers have not switched. A development data directory from between the
two is disposable, so use a fresh `RIMAIA_DATA_DIR`.

**What the next tasks expect.**

- **042** moves the loop into `rimaia-runner` without touching storage. It reads only
  through `MachineContext`, and adds the runner's `max_turns` and `disallowed_tools`
  overrides as two more `runner_settings` keys through the same accessors.
- **043** adds `held_leases` methods to `MachineStore` and its contract cases, and replaces
  `worktree::reconcile`'s leaseless branch write.
- **045** reads the board's `allow_unattended_runs` as the ceiling and ANDs it with the
  checkout's consent on the claim.
- **046** marks the four new commands `local` and extends
  `no_board_dto_carries_an_absolute_path` to iterate every board row of the registry. Its
  server passes `None` for `LocalTools`.
- **054** adds `normalized_remote` to `checkouts` (`20261003130200_checkout_mapping.sql`),
  splits registration, and turns the two synchronous machine reactions into runner reactions
  with reported results.
- **056** replaces the derived log path with `transcript_uploads.path`.
- **060** decides what the hosted `/mcp` serves for the two planning tools.
- **065** drops the retired columns. It must delete `machine::adoption::read_board` in the
  same change, because that read names them and would stop compiling.

**Size, and where to cut.** Honestly estimated, the whole task is about 5,000 changed lines
before the regenerated caches:

- the port, both implementations and the contract suite: about 1,300;
- adoption: 400;
- reader and signature churn across core and the shell: 1,500;
- the MCP split: 300;
- the frontend and its tests: 900;
- test fixture updates: 600.

That is above what one agent should carry in one session. **Recommended: cut before launch at
the Part A / Part B line.** Part B becomes a new task, "Checkouts and worktree records move to
the runner", with the next free number. It is placed directly after 041 in `tasks/README.md`
and depends on `"041"`. It takes Part B's Scope and acceptance criteria unchanged, and the
DTO test with them.

If the cut is made:

- 041 still lands the whole migration file and the whole adoption step. The file freezes
  when 041 lands (D28's D4 amendment), and both tasks ship in one release.
- The cut task adds no migration.
- Its row goes into D32's appendix and D28's Binds list by amendment.

If the cut is not made, implement Part A first and commit it as its own series. Part B's
tests assume Part A's port.
