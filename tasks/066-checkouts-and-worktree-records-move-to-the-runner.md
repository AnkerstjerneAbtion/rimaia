---
id: "066"
title: Checkouts and worktree records move to the runner
milestone: v0.5
status: ready
depends_on: ["041"]
adrs: ["0028", "0033", "0032", "0013", "0025", "0005", "0021"]
size: L
---

# Checkouts and worktree records move to the runner

## Goal

Every fact about one repository or one task that is only true on one machine is read from
`runner.db`, and the board stops reading any of them:

- the per-repository half of `repositories`: the clone path, `worktree_root`,
  `max_concurrency`, the unattended consent, `on_archive`, `on_archive_script` and the
  `credential_*` metadata, now read from `checkouts`;
- `tasks.worktree_path`, now read from `worktrees`;
- `runs.log_path`, now derived from the run's ids.

When this lands, no board DTO carries an absolute path, and a test proves it.

Task 041 built everything this needs: the `checkouts` and `worktrees` tables, the adoption
that filled them on every existing install, and the `MachineStore` methods that read and
write them. This task moves the readers and writers, and every surface that shows them.

**Solo behaviour does not change**, with one exception: a narrower rule for changing a
task's repository (D13, below).

## Why now

This task was cut from 041, whose whole was about 5,000 lines, and sits directly after it.

042 sends `ClaimTarget::Next.repositories`, the repositories this runner has consented to,
and counts per-repository caps on the runner. Both must already be read from `checkouts`
when it starts. 046 serves board commands over HTTP, where a path is meaningless to the
caller and leaks one member's filesystem layout to the team. 054 lets a repository exist on
the board with no clone on this machine. All three are easier to build on a board that
already has no paths than to retrofit.

## Scope

**`Repository` splits in two.**

- **The board type keeps** `id`, `team_id`, `name`, `default_branch`, `created_at` and
  `review_config`. It keeps `allow_unattended_runs` in the struct, because it is the team
  ceiling 038 kept (ADR-0032 point 4). The ceiling is **not** serialized to the frontend or to
  MCP until 045 names it.
- **Everything machine-local moves to 041's `machine::Checkout`.** This also carries out
  D31's note that `RunContext::repository` narrows to the board's pathless row.

Every function that needs a clone path, a worktree root, a per-repository cap, the consent,
the archive policy or the credential metadata takes the `Checkout` from `MachineContext`:

- `repo::{remote_info, gh_status, path_problem}`;
- `worktree::{locate, prepare, remove, reconcile}` and `worktree::cleanup`;
- `archive::{run_on_archive, set_repository_on_archive}`;
- `credentials::inject`, and `repo::{set,clear}_credential_metadata` (D25);
- `scheduler::capacity`'s per-repository caps;
- `scheduler::selection::{plan, skip_reason}` and `get_queue_status` (below);
- `doctor`'s per-repository checks;
- `openers`;
- `startup::survey`.

A repository with no checkout on this machine is a legitimate state (ADR-0033 point 2):

- every reader answers it with `Error::invalid` naming the repository and saying it is not
  set up on this computer;
- nothing panics;
- a starter refuses before any run state is written.

**The queue offers only what this runner consented to.** `selection::plan` filters with
`repo::allows_unattended_runs` on the board row today (`scheduler/selection.rs:183`). After
this task that field is the team ceiling, so leaving the filter would skip every repository
registered from now on (board default `0`) and keep offering one whose consent was
withdrawn, which the pre-spawn check then refuses and the release lands in `failed`.
`plan` and `get_queue_status` take the set of repository ids whose checkout has
`unattended_consent`, read once per pass from `MachineContext` by one core function,
`machine::consented_repositories`. `skip_reason` keeps its signature and its
`UnattendedRunsNotAllowed` reason, fed from that set. 042 sends the same set as
`ClaimTarget::Next.repositories`.

`ensure_unattended_runs_allowed` reads the consent from the checkout.
`set_repository_unattended_runs` writes the checkout's `unattended_consent` only. Neither
reads the ceiling: D31's table gives the ceiling check on the claim to 045.

**Registration and removal in solo.** `register_repository` stays a local command until 054
splits it (D32 appendix). In solo it:

- validates the path as it does today;
- checks for duplicates against `checkouts.path`, no longer against the board;
- writes the board row without naming a retired column, so `path` and `worktree_root` are
  `NULL` and the rest take their defaults;
- then writes the checkout.

`remove_repository` takes the archive functions' shape, `repo::remove(&ServiceContext,
Option<&MachineContext>, id)`:

1. the board removal, unchanged: refused with today's sentence, before any write in either
   store, while any task still references the repository;
2. given a machine, it forgets every worktree record of that repository. Each belongs to a
   task the board has already deleted, because step 1 passed. `delete_task` does not remove
   a worktree, so these exist on real installs, and the checkout's `RESTRICT` would
   otherwise refuse step 3 and orphan it;
3. it removes the checkout.

The Tauri command passes `Some`. The worktree directories of deleted tasks stay on disk, as
they do today.

**The per-repository setters.** `update_repository` loses `worktreeRoot`, which becomes the
new local command `set_repository_worktree_root` (D32's Binds). The three per-repository
setters, `set_repository_unattended_runs`,
`set_repository_on_archive` and `set_repository_max_concurrency`, return `CheckoutView`
instead of `Repository`. The MCP tool `set_repository_max_concurrency` returns
`CheckoutView`'s snake_case view (D16.1) instead of `RepositoryView`.
`set_repository_on_archive`'s tool keeps `RepositoryOnArchiveView`, which carries no
removed field.

**Worktree records.** `write_worktree_columns` retires, and its two halves go separately:

- **The path** goes to `MachineStore::{record_worktree, forget_worktree}`.
- **The branch** goes through `BoardPort::record_branch` wherever a lease exists: from
  `worktree::prepare`, reached from `run_task` and `plan_claimed` (D17). This gives
  `record_branch` its first production caller (D31 point 7).

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
The readers are `runs::{list, get, log_path_to_reveal, prune}`, `startup::survey`'s
missing-transcript check, and the three transcript reads. `prune` and the survey keep the
kind rules D29 gives them. `start_run` keeps writing the column as D31 point 4 says, until
056's `transcript_key` replaces it, but nothing reads it. The two unchecked
`SELECT log_path` queries in `runs/mod.rs` go away.

**No board DTO carries an absolute path.**

- `Repository` (Rust, `src/types.ts`, MCP `RepositoryView`) loses `path`, `worktreeRoot`,
  `maxConcurrency`, `onArchive`, `onArchiveScript` and `allowUnattendedRuns`.
- `Task`, `TaskSummary` and `TaskDetail` lose `worktreePath`, and the MCP `TaskView` loses
  `worktree_path`. Amend D12's field list in the same commit.
- `Run` loses `logPath`.

`get_run` already runs no git: 033's `RunReview` is `Recorded` or `NotRecorded`, and for
`NotRecorded` the overlay calls the local `get_diff_summary`. That command now finds the
worktree through the local record, and answers a repository with no checkout with the "not
set up on this computer" refusal, which the overlay's existing failed-fallback line shows.

**Four new local commands, and the frontend.**

| Command | Returns / does | Replaces |
| --- | --- | --- |
| `list_checkouts` | `CheckoutView[]`: `repositoryId`, `path`, `worktreeRoot`, `maxConcurrency`, `unattendedConsent`, `onArchive`, `onArchiveScript` | the per-machine fields `Repository` lost |
| `set_repository_worktree_root` | `{ repositoryId, worktreeRoot }` → `CheckoutView` | `update_repository`'s `worktreeRoot` |
| `list_local_worktrees` | `{ taskId, path }[]` from `worktrees` | `worktreePath` on task DTOs |
| `get_run_log_path` | `{ taskId, runId }` → the derived path | `Run.logPath` |

`list_checkouts` is also a local MCP tool, the local router's 23rd, refused to runs, with the
fields `repository_id`, `path`, `worktree_root`, `max_concurrency`, `unattended_consent`,
`on_archive` and `on_archive_script` (D16.1). The operator's MCP client could read a
repository's path, cap and consent through `list_repositories` until now, and ADR-0021 does
not allow that capability to disappear. For the other three, the appendix rows say:

- `list_local_worktrees` is paired with the existing `list_worktrees` tool, which already
  serves this machine's worktree paths;
- `set_repository_worktree_root` inherits `update_repository`'s existing missing tool. The
  worktree root had no MCP surface before this task, so nothing is lost;
- `get_run_log_path` is a new ADR-0021 point 1 gap, recorded as D32 point 9 records the
  existing ones.

Append a row for each of the four to D32's appendix (point 8), and change
`update_repository`'s note to name this task. Add each wrapper to `src/lib/commands.ts`,
still through the private `call<T>` (046 splits it), and a row for each to 028's
fixture transport in `src/dev/fixtures/`, whose seed `Repository`, `Task` and `Run` records
lose the removed fields.

Two hooks, `src/hooks/useCheckouts.ts` and `src/hooks/useLocalWorktrees.ts`, re-read on the
events below, subscribing through `src/lib/events.ts` (D7). Components join by id:

- `RepositoriesSection`, `ConcurrencySection`, `OnArchiveFields` and `WelcomeView` read the
  per-machine fields from the checkout;
- `TaskCard` reads the consent for Run now's disabled state from the checkout, and
  `worktreePath` from the local worktrees, as do `TaskDetailPanel`, `RunInfoSection` and
  `OpenInMenu`;
- `src/lib/archive.ts` reads `onArchive` and `onArchiveScript` from the checkout, for its
  callers `ArchiveTaskSection` and `Board`;
- `RunOutcomeSection` and `RunDetailOverlay` fetch the path on the copy action.

Each has a Vitest test covering the joined state and the "not set up on this computer"
state. `RunDetailOverlay`'s also covers `NotRecorded` when `get_diff_summary` refuses.

**Events.** Checkout writes publish `Repositories(ids)` and worktree-record writes publish
`Tasks(ids)`, through 041's `MachineContext` under its `event_team`, each site with a comment
naming 048. 048 turns them into `LocalChange::Checkouts` and `LocalChange::Worktrees`,
which keep these wire names, so the two hooks need no change then.

**Machine reactions.** 041 gave `archive_task`, `archive_tasks`, `move_task` and 034's
`approve` and `reject` their `Option<&MachineContext>` functions. Their reactions now read
the archive policy and script from the checkout and the worktree from its record. Reject's
machine half (034's "What the next tasks expect") is the dirty-tree check,
`worktree::remove` and `forget_worktree`; the note, `branch = NULL`, the move and the marker
stay board writes. `remove_repository` (above) joins them.

**The local-handler rule (D32 point 8, as amended 2026-10-04)** applies here as in 041: no
handler this task adds or rewrites issues a board query of its own, and the PR lists each
named core read function a local handler calls, for 059.

## Out of scope

- **The unattended ceiling:** its board command, its check on the claim, and its value for a
  repository registered after this task (045). Until then `register_repository` leaves the
  board column at its default and nothing reads it.
- **Mapping a checkout by remote, `normalized_remote`, re-keying keychain items per runner,
  asynchronous archive cleanup with reported results, and `report_runner`** (054). The
  keychain stays keyed by repository id.
- **`transcript_key`, the outbox and `transcript_uploads`** (056). `logAvailable` on run DTOs
  stays a boolean computed from the derived path.
- **The command registry and its DTO test over every board row** (046).
- **Free-form text.** The rule covers structured fields. `runs.error_message` can quote a
  local path, for example `process.rs`'s "could not start it in {workspace}", and so can
  git's stderr inside it. No task scrubs these yet; the PR records it as a known gap.
- **Worktree records for deleted tasks**, beyond `remove_repository`'s step 2. A record whose
  task the board no longer has is invisible, as a deleted task's worktree directory is
  today. The inventory and the survey ignore it.
- **The retired columns.** Nothing drops, relaxes or backfills them; 065 drops them.

## Acceptance criteria

- `no_board_query_reads_a_retired_column` parses every entry in `crates/core/.sqlx/*.json`.
  It checks the `query` text, with string literals and comments stripped, and every name in
  `describe.columns`, so neither `SELECT *` nor an alias hides a read. It fails on
  `worktree_path`, `worktree_root`, `log_path`, `on_archive`, `on_archive_script`,
  `credential_(login|label|added_at)`, or `path` or `max_concurrency` as a whole word. Two
  markers exempt a query:
  - `-- machine_state adoption`, which only 041's `read_board` carries;
  - `-- runs.log_path written until 056`, which only `start_run`'s `INSERT` carries. For
    that query the test still fails on any other name, and on `log_path` in
    `describe.columns`.

  No unchecked query in `crates/core/src/` names a retired column outside
  `machine::adoption`, and the PR shows the `grep` that says so.
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
- `an_unattended_run_needs_this_runners_consent` covers the run and the queue:
  - ceiling `1` and consent `0`: Run now is refused before any run state is written, and
    the queue skips the task with `UnattendedRunsNotAllowed` and never claims it;
  - ceiling `0` and consent `1`: the run proceeds, and the queue offers the task.

  The second case pins that this task does not read the ceiling.
- `a_task_with_a_recorded_branch_cannot_change_repository` asserts the D13 refusal sentence
  exactly. A task with no branch and no runs still moves. D13 carries a dated amendment.
- Registering the same clone twice is refused from `checkouts`. A freshly registered
  repository has `NULL` `path` and `worktree_root` on the board and a complete checkout.
  `update_repository` rejects nothing it used to accept apart from the removed
  `worktreeRoot`, and `set_repository_worktree_root` sets that value.
- `a_repository_whose_deleted_tasks_left_worktrees_can_be_removed_and_registered_again`,
  against real git in a `TempDir`: prepare a task's worktree, delete the task, remove the
  repository, and register the same clone again. Both steps succeed, and no checkout or
  record of the removed repository remains.
- The three per-repository setters return `CheckoutView`, and the MCP tool
  `set_repository_max_concurrency` returns its snake_case view.
- `a_run_log_is_found_by_its_ids_not_its_column` sets `runs.log_path` to a path that does
  not exist, then checks that the transcript page, search, summary, reveal and prune all
  still find the file at the derived path.
- Archiving with `remove_worktree` set on the checkout, through the command, removes the
  worktree and reports `WorktreeRemoved`. Through the core function with no machine, it
  reports `Nothing` and leaves the worktree. Both run against real git in a `TempDir`.
- No assertion in `crates/core/tests/scheduler.rs` changes. Its fixture setup changes only
  where an accessor's first argument changed or a per-repository setter moved to the
  checkout.
- The combined MCP router lists every tool it listed at 041, plus `list_checkouts`, and
  `a_server_without_a_machine_lists_no_machine_tool` covers all 23.
- The four local commands are defined, registered, wrapped in `commands.ts`, served by 028's
  fixture transport, and have rows in D32's appendix. `./scripts/check-command-wiring.sh`
  passes.
- `src/types.ts`'s `Repository` carries none of `path`, `worktreeRoot`, `maxConcurrency`,
  `onArchive`, `onArchiveScript` and `allowUnattendedRuns`; no `Task*` type carries
  `worktreePath`; `Run` carries no `logPath`. Vitest covers each changed component's joined
  state and its "not set up on this computer" state.
- `npm run screenshot` shows Settings → Repositories, a card with a worktree, and the run
  overlay rendering from the seeded checkouts, and the images were inspected.
- Every production reader of the machine-local columns goes through `MachineContext`. D31's
  table rows for `write_worktree_columns`, the runner half of `ensure_unattended_runs_allowed`
  and `record_branch`'s first caller are done, and 041's D31 amendment gains the checkout
  and worktree half.
- `.sqlx` caches regenerated as in 041. CLAUDE.md's Gotchas line from 041 gains "and no
  board DTO carries an absolute path". No CI step changes.
- Every CI check passes, as listed in 041.
- **Needs a person (PR checklist):** on a copy of a real `rimaia.db`, launch with a scratch
  `RIMAIA_DATA_DIR`. Settings → Repositories, a card's worktree badge, Run now on an
  opted-in repository, and "copy log path" all show what they showed before the upgrade.

## Notes

**Seam entries to read:**

- D4: no migration here; 041's file is frozen.
- D7: hooks subscribe through `events.ts`. D8: errors.
- D12 and D13: both are amended here.
- D16: MCP casing for `list_checkouts` and the setter's view. D17: `plan_claimed` records the
  branch.
- D20 and D26: cleanup and archive. D25: credentials.
- D28: part 6's `checkouts` and `worktrees`. D29: prune and the survey as `runs` readers.
- D31: points 4, 5, 7 and 14.
- D32: points 8 and 9, its 2026-10-04 amendment, the appendix, and its Binds line for 066.
- D33: the recipe. D34: no new dependency.

**Files to start from:**

- `crates/core/src/machine/` (041);
- `crates/core/src/worktree/{mod,cleanup,safety}.rs`, `crates/core/src/repo/mod.rs`,
  `crates/core/src/archive/mod.rs`, `crates/core/src/credentials/inject.rs`;
- `crates/core/src/scheduler/{selection,capacity,queue}.rs`;
- `crates/core/src/doctor/{mod,checks}.rs`, `crates/core/src/openers/mod.rs`;
- `crates/core/src/runs/mod.rs`, `crates/core/src/startup.rs`,
  `crates/core/src/runner/{process,events,strategy}.rs`;
- `crates/core/src/tasks/service.rs`: `ensure_repository_is_reassignable`, and the archive
  and `move_task` paths;
- `crates/core/src/db/models.rs` (`Repository`, `Task`, `Run`) and
  `crates/core/src/mcp/responses.rs`;
- `src-tauri/src/commands/{repositories,worktree,queue,runs}.rs`;
- `src/types.ts`, `src/lib/commands.ts`, `src/lib/archive.ts`, `src/dev/fixtures/`, and the
  components named in Scope.

**What 041 provides.** `MachineStore`'s checkout and worktree methods, `MachineContext` on
`AppState.machine` and `TestContext::machine()`, `checkouts` and `worktrees` filled by
adoption on every existing install, `LocalTools` and the local router, and the five
machine-reaction functions.

**What the next tasks expect.**

- **042** sends `machine::consented_repositories` as `ClaimTarget::Next.repositories`, and
  counts per-repository caps from `checkouts`.
- **043** replaces `worktree::reconcile`'s leaseless branch write.
- **045** reads the board's `allow_unattended_runs` as the ceiling and ANDs it with the
  checkout's consent on the claim.
- **046** marks the four new commands `local`, carries `remove_repository` on
  `BoardRequest.machine` with its point 3a rows, and extends
  `no_board_dto_carries_an_absolute_path` to iterate every board row of the registry.
- **048** turns the checkout and worktree publications into `LocalChange::Checkouts` and
  `LocalChange::Worktrees`, and adds these writers to its machine-state test.
- **054** adds `normalized_remote` to `checkouts` (`20261003130200_checkout_mapping.sql`),
  splits registration, and turns the machine reactions into runner reactions with reported
  results.
- **056** replaces the derived log path with `transcript_uploads.path`.

**Size.** About 2,500 changed lines before the regenerated caches: reader and signature churn
across core and the shell 900, the frontend and its tests 900, fixture updates 600, and the
commands 100.
