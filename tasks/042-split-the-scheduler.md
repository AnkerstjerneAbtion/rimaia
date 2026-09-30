---
id: "042"
title: Split the scheduler into board selection and a runner loop
milestone: v0.5
status: ready
depends_on: ["041"]
adrs: ["0010", "0031", "0028"]
size: L
---

# Split the scheduler into board selection and a runner loop

## Goal

Cut `rimaia_core::scheduler` along the line
[ADR-0031](../docs/adr/0031-runners-claim-work-with-leases.md) draws. **The board decides
what runs next. The runner decides whether it has room, and when to ask.**

- **Board side, in `rimaia-core`.** Selection and the claim become one board operation,
  `BoardPort::claim(ClaimTarget::Next { .. })`. Seam-contract D31 names the variant and this
  task adds it, with its in-process body and its contract cases. ADR-0010's rules (board
  order, dependencies satisfied, the per-repository cap) and ADR-0012's opt-in are applied
  there, against what the runner says it can take.
- **Runner side, in `crates/runner`.** The queue loop, the go signal, capacity, run windows
  and schedules, and the usage-limit pause. All of them are per runner, and the loop reaches
  the board only through the port.

Three smaller changes ride with the split, because each is about which half owns what:

- **`InFlight`'s `Lease` becomes `LocalSlot`.** `LeaseOwner` and `LeaseRefused` become
  `SlotOwner` and `SlotRefused`. The rename reaches every user, `src-tauri/src/commands/
  strategy.rs` and `commands/runs.rs` included. From here on, "lease" in this codebase
  means only the board's lease (`LeaseRef`, `LeasePurpose`, and 043's `runner_leases`).
- **`max_turns` and `disallowed_tools` get their runner half.** The effective value is the
  stricter of the team's and the runner's (ADR-0028 point 2): the lower turn budget, and
  the team's blocklist plus whatever the runner adds.
- **The runner reads team and user settings from nowhere but the board.** Task 039 moved the
  readers to `team_settings` and `user_settings` (D28 part 4). This task adds no second
  reader. The team half reaches a run only as `RunContext::limits`, and a runner never
  reads `subscription_monthly_usd` at all.

**Solo behaves exactly as it does today.** There is one runner, one team and one queue. Every
existing scheduler test passes with its assertions unchanged. No file under `src/` changes,
and no Tauri command, MCP tool or event payload changes shape.

## Why now

Task 043 replaces the claim's body with one transaction that also writes a `runner_leases`
row (ADR-0031 point 1). That only works if the claim is the board's decision. Today the loop
picks the task itself, with `selection::plan` and `next_batch` over a board read, and then
claims the id it picked through `claim(Run)`. A remote runner cannot do that. It has no board
to read, and ADR-0031 point 1 has the server choose, applying ADR-0032's eligibility rules
that a runner must not be trusted to apply. If 043 lands first, the lease claim is written
for a caller that chooses its own task, and 052 then has to take the choice back across the
network. D31 point 7's table assigns this row to 042 for that reason:
`queue::try_step → selection::plan, next_batch | reads the board | claim(Next)`.

Per-runner queue control has to exist before a second runner does (052, 053, 058). Seam
entries D15, D19 and D21 were written for one process. ADR-0031 point 6 says they hold per
runner: quitting stops *that* machine's queue, a usage limit holds *that* runner, and the
per-repository cap counts *that* runner's runs. 041 moved where those values are stored.
This task moves the loop that obeys them, and proves "per runner" with two runners over one
board while it is still one process and still testable with a `TestClock`.

The rename is cheapest now. D31 point 2 named the board type `LeaseRef` rather than `Lease`
because the in-process slot had that name "until 042 renames it `LocalSlot`". From 043 on,
both kinds of lease are live in one function (`try_step` holds a slot and a `LeaseRef` at
once), and a diff that confuses them compiles.

## Scope

Read D31 in full, and D19, D21 and D15 with their amendments, before starting. Everything
below refines them and contradicts none of them.

**1. Where each piece lives when this task lands.** 041 may already have moved some of the
runner-side modules into `crates/runner` to reach `runner.db`. This table is the end state
either way.

| Piece | Lives in | Reads the board through |
| --- | --- | --- |
| selection: `plan`, `skip_reason`, `next_deadline`, the pick | `rimaia_core::scheduler::selection` | `ServiceContext` (it *is* the board) |
| the claim edges: `claim`, `claim_retry`, `release`, `give_up` | `rimaia_core::scheduler::claim` | `ServiceContext`. 043 replaces the bodies |
| attempts, the retry policy, reconcile | `rimaia_core::scheduler::{attempts, retry, reconcile}` | `ServiceContext`. Per-runner reconcile is 043's |
| `InFlight`, `LocalSlot`, `SlotOwner`, the preparation lock | `rimaia_core::scheduler::inflight` | nothing. It is in memory |
| `ClaimTarget::Next`'s body | `rimaia_core::board::service` | `ServiceContext` |
| the loop, `QueueHandle`, `QueueTask`, `supervise` | `crates/runner/src/queue/` | `BoardPort` only |
| the go signal, capacity, windows and schedules, the usage-limit pause | `crates/runner`, over `runner.db` | never |

`InFlight` stays in `rimaia-core` because `run_task`'s preparation lock, the planner's
`claim_for_planning` and the MCP server's Plan now all hold it, and all three are core code.
ADR-0031 point 6 calls it "the runner's local registry". That describes what it means, not
which crate compiles it. `rimaia_core::scheduler::queue` is deleted, and nothing re-exports
it under its old path. A second path to a loop is how two loops happen.

`rimaia_core::scheduler`'s module header is rewritten to describe what is left: the board
half of the scheduler, and the slot registry. Its "The six pieces" section currently
describes a loop that no longer lives there.

**2. `ClaimTarget::Next` (D31 points 2 and 4).** 036 left it out on purpose ("a variant whose
only body is a refusal cannot be told apart from a bug"). This task adds it and gives it a
real body:

```rust
ClaimTarget::Next { capacity: FreeCapacity, repositories: Vec<String>, wait: Duration }
pub struct FreeCapacity { pub total: usize, pub per_repository: BTreeMap<String, usize> }
```

The shape is D31's, exactly. What each field means, as this task pins it (and records in a
dated D31 amendment):

- **`repositories`** lists the repositories this runner has a checkout for *and* has given
  unattended consent for (041's `checkouts.unattended_consent`). ADR-0032 point 4 has the
  claim check the team ceiling, and has the runner re-check its own consent before spawning.
  Listing only consented repositories keeps the two checks from disagreeing on every pass:
  otherwise the board offers a task the runner then refuses, and a refused claim is a
  `release`, which lands the task in `failed`. The runner still re-checks before it spawns,
  because a consent withdrawn between the claim and the spawn is exactly the case point 4
  exists for.
- **`capacity`** is what the runner has *free*, already net of its own in-flight runs. The
  runner computes it from its resolved caps (`capacity::resolve`: the window, the mode,
  `max_concurrency`, each checkout's `max_concurrency`) minus `InFlight::counts()`.
  **A repository that appears in `repositories` and is missing from `per_repository` has no
  free slot.** This is the same conservative reading `next_batch` gives a missing key today
  ("never unbounded"), applied to a value that has already had the in-flight runs taken off.
- **`wait`** is honoured in process as D31 point 4 says: by waiting on
  `ServiceContext::subscribe` and `Clock::sleep_until`, never `tokio::time::sleep`. The
  body tries once, then waits until a change event or the deadline, then tries again, and
  returns `None` when the deadline passes with nothing claimed. **The solo loop always
  passes `Duration::ZERO`** (point 4 below says why). The waiting form has contract cases
  now, so 053 inherits a tested body rather than a stub.

The body, in `board::service`, is one function that the in-process adapter calls and 052's
HTTP handler will call:

1. `selection::plan(ctx, &view)`, where `view` is built from `repositories`. `plan` gains
   this parameter. A repository is opted in when its team ceiling
   (`repositories.allow_unattended_runs`, which 038 redefined as the ceiling) is on **and**
   the runner listed it. Either failing gives `SkipReason::UnattendedRunsNotAllowed`,
   exactly as today's single opt-in does. `skip_reason` stays a pure function and learns
   nothing new.
2. The first entry, in board order, with no skip reason and a free slot in its repository,
   and while `capacity.total > 0`. This is a pure function beside `next_batch`,
   `selection::first_startable(&plan, &FreeCapacity)`, with its own unit tests. Capacity
   is still not a `SkipReason` (D21 point 3).
3. The claim, by today's two routes: `claim::claim` for a fresh start, and `claim_retry`
   plus 035's `attempts::resume_point` for an entry whose `resume_after` is set. A lost
   race moves to the next entry rather than returning. A `Next` claim returns at most one
   `Claim`, with `purpose: Implementation`, `trigger: RunTrigger::Queued` and the context
   `claim(Run)` already builds.

This is the only place in the workspace that runs selection for a claim. Task 045 adds the
assignee, consent and trust predicates here, next to the ceiling, and 043 replaces step 3
with the lease transaction. Neither should have to touch the runner.

**3. The loop moves to `crates/runner/src/queue/`, and reaches the board only through the
port.** It keeps its shape: the same arms, the same order (shutdown check, drain,
`tick_schedules`, step), the same `DEADLINE_CAP`, and the `JoinSet` drain as the last
statement of `run`. D21 point 5 and the module header's arguments move with it, verbatim
where they still hold. What changes is `try_step`:

- the switch, the usage-limit pause and the capacity are read from this runner's store;
- if the switch is off, the pause is active, or `FreeCapacity::total` is zero, the pass does
  nothing more: no probe and no claim;
- otherwise it runs `probe_cli` once, then calls `claim(Next { wait: ZERO })` until it
  returns `None` or the free capacity is spent. Each call re-derives `FreeCapacity`, so the
  second call already counts the first claim's slot;
- for each `Claim`: it re-checks the switch, the cancel signal and shutdown (today's
  `interrupted_since`), and calls `release` if any of them changed. It re-checks the
  runner's own consent for the claim's repository, and releases if that has been withdrawn.
  It takes the `LocalSlot` with `acquire` under the resolved caps, and releases if that is
  refused. Then it spawns `supervise` with the claim.

`build` takes an `Arc<dyn BoardPort>` and a board-change receiver in place of a board
`ServiceContext`'s pool. It also takes the runner store, the `InFlight`, `AppPaths` and the
`RunnerConfig`. `run_task` and `doctor::run` are called with what they take after 041.
This task changes neither signature, apart from point 6's limits.

**4. Why the solo loop claims with `wait: ZERO`, and what wakes it.** A `Next` claim that is
waiting was issued with the free capacity of the moment it started. Take a runner with one
free slot in repository A and none in B. A run in B finishes and frees its slot, but the
waiting claim still says B has none, so it sleeps through the ready task in B until its
`wait` runs out. With a `TestClock` it never runs out. The fix would be to drop the waiting
claim and ask again. Before 043, though, the claim's two edges are two separately committed
transactions, so a claim dropped between them strands its task at `queued`. A claim dropped
after it committed leaves a task `running` that nothing supervises.

So the solo loop keeps today's wake sources and makes each claim a non-blocking try. The
board-change arm is the in-process context's `subscribe()`, handed to `build` by the shell,
and it replaces today's `changes` arm one for one. `releases`, the signals, the join arm and
the deadline arm are unchanged. The long poll, and cancelling a claim that has not yet
committed, are 053's. They need a one-transaction claim (043) and lease expiry to recover a
reply that never arrived (D31 point 4, `heartbeat`). Record this in the D31 amendment.

**5. The slot is taken after the claim when the board picks the task.** D19 and the queue's
header say the queue takes its slot *before* the claim, "so a Pause pressed mid-claim has
something to act on". Under `Next` the runner does not know which task it is getting until
the claim returns, so it cannot take a slot for it first. What closed the mid-claim window
was never the slot. It was the re-check of the switch and the shutdown signal after each
await, and point 3 keeps both. A Stop that lands during the probe writes `paused` and
cancels nothing, because nothing is held yet. The re-check after the probe then stops the
pass before any claim, and the task stays `idle`, which is what the three mid-claim tests
assert. Record this as a dated D19 amendment, together with the rename.

Manual starts keep 036's order (`preview`, slot, `claim(Run)`), because a person named the
task.

**6. The stricter of team and runner (ADR-0028 point 2).**

- **The runner half.** Two `runner_settings` keys, `max_turns` and `disallowed_tools`, with
  typed readers beside 041's other runner settings. Both are absent by default, and absent
  means no override (D28 part 4: "the runner's stricter override starts out absent"). Reads
  are tolerant, like every other key here. A `max_turns` that is unparseable or `0` warns
  and reads as absent. It never reads as `0`, and it never reads as the team's value. The
  blocklist is one pattern per line, blank lines ignored, which is today's `disallowed_tools`
  format. There is no command, no MCP tool and no UI: like the team values today, they are
  set in the sqlite3 CLI (ADR-0003). A control for them is 061's.
- **The rule.** One pure function in `rimaia-core`, `runner::limits::effective(team:
  &TeamLimits, runner: &RunnerLimits, provider: ProviderId) -> EffectiveLimits`. It takes
  `TeamLimits` from D31, and a new `RunnerLimits { max_turns: Option<u32>,
  disallowed_tools: Vec<String> }`.
  - `max_turns` is `min(team, runner)` when the runner set one, and the team's otherwise.
  - The forbidden operations are the team's first: `claude::DEFAULT_FORBIDDEN` when
    `TeamLimits::disallowed_tools` is `None`, and one `ProviderRule` per stored rule
    otherwise, an explicitly empty list included. Then come the runner's rules not already
    present, as `ProviderRule`s tagged with `provider`. Then the caller's extras
    (`RIMAIA_TOOL_SURFACE`, the planner's own). The order is fixed, so the argv is
    deterministic.
  - The runner can only add. Nothing in `RunnerLimits` can remove a team rule or raise
    the team's budget, and the type makes no room for it.
- **Every process a runner starts goes through it** (ADR-0032 point 5): a queued run, Run
  now, Retry now, and the planner. The planner's turn budget is `min(catalogue.planner.
  max_turns, runner)`. The team's `max_turns` keeps capping only the runs it caps today, so
  a solo planner is unchanged. The runner half reaches `run_task` and the planner by the
  route 041 gave `run_environment`: read from `runner.db` when each run starts, never
  cached across runs. `runner::process::max_turns`, `disallowed_tools` and
  `forbidden_operations` in their `&SqlitePool` forms no longer exist. Their only remaining
  callers were the paths this rule replaces.

**7. The rename.** `Lease` → `LocalSlot`, `LeaseOwner` → `SlotOwner`, `LeaseRefused` →
`SlotRefused`, across `crates/core/src/scheduler/inflight.rs`, `runner/strategy.rs`,
`mcp/server.rs`, the moved queue, `src-tauri/src/commands/runs.rs`,
`src-tauri/src/commands/strategy.rs` and every test. Method names (`acquire`,
`acquire_unbounded`, `cancel_owned_by`, `releases`) do not change, and neither does
`PlannerClaim`'s shape. Doc comments that call the slot a "lease" say "slot". The
`releases` arm's comments keep their argument and change their noun.

**8. The shell and the two status reads.** `src-tauri/src/lib.rs` builds the queue with
`rimaia_runner::queue::build`, passing 036's `board_port`, `context.subscribe()`, 041's
runner store and the one `InFlight`. `AppState::queue`, `notify.rs` and `commands/queue.rs`
change their import paths.

`QueueStatus` keeps its wire shape. Its `plan` is a board read and everything else in it is
the runner's, so one solo-only function joins them, `rimaia_runner::queue::status_with_plan`:
the runner half from the handle, plus `selection::plan(&board_ctx, &view)` built from the
same runner facts the loop sends with `Next`. The `get_queue_status` command calls it. So
does `preview_schedule_preflight`, through whichever door 041 gave it, so the plan a card
shows and the plan a claim acts on come from one view. Task 059 replaces the board half
with an HTTP read. This function is the one place that has to change for it.

**9. Tests move with their code.** Loop tests leave `crates/core/tests/scheduler.rs` for
`crates/runner/tests/queue.rs`, keeping `#![cfg(unix)]` and the file's header argument. The
claim, selection and reconcile tests stay in core. `rimaia-core`'s tests cannot name
`rimaia-runner` (040's `rimaia_core_does_not_depend_on_rimaia_runner`), so a test that
builds a loop has to live in the runner crate. Update the two references to
`crates/core/tests/scheduler.rs` in `tasks/044-branch-from-the-dependencys-commit.md`, in
the same commit.

**10. Documentation.**

- Seam contract: dated amendments to **D19** (the rename, and point 5's ordering), **D22**
  (the probe now runs before each claim pass that has a free slot, rather than before each
  non-empty batch; a pass that cannot start anything spawns nothing) and **D31** (points 2
  and 4 above). Add or complete 042's row in "How to use this": D15 · D19 · D21 · D22 ·
  D24 · D28 · D29 · D31 · D33.
- CLAUDE.md, one Gotchas bullet: `max_turns` and `disallowed_tools` have a team value and a
  runner value, the effective value is the stricter of the two (ADR-0028 point 2), and a
  run is never built from either half alone.
- Regenerate both offline caches with D33 point 3's recipe if any query changed. The
  runner's new setting readers are the likely ones.

## Out of scope

- **Leases.** No `runner_leases` row, no generation other than `0`, no fencing, no
  `Conflict`, no pinning, and no `held_leases` write. `claim.rs`'s two transactions stay
  two. All of that is 043, which replaces step 3 of point 2's body and nothing on the runner.
- **Per-runner reconcile and `startup::survey`.** They stay as they are (043).
- **The long poll**, a `wait` greater than zero from a production caller, heartbeats, and
  expiry (053). Point 4 says what 053 inherits.
- **Eligibility beyond the team ceiling and the runner's listed repositories:** assignment,
  the three revisions, acceptances, the trust list, and the model and effort cap. These are
  045's, added to point 2's body.
- **A runner serving more than one team.** In process, the claim runs under the adapter's
  context, which in solo is the one team. Scoping `Next` for a runner whose owner is in
  several teams is 052's, with its runner token.
- **Run now for a specific runner** (ADR-0031 point 7, 052), **run elsewhere** (057), and a
  `SkipReason` for "this runner has no checkout" (054 and 061).
- **Moving any stored value.** 039 moved team and user settings, and 041 moved runner
  settings, checkouts and schedules. This task adds two runner keys and moves no row.
- **A settings control** for the runner's `max_turns` and `disallowed_tools` (061).
- **Anything under `src/`**, and any new Tauri command or MCP tool. `check-command-wiring.sh`
  and `mcp/scope.rs`'s tool table do not change.
- **Any migration** (D4). The two runner keys are `runner_settings` rows, which is what
  that table is for.
- **Any dependency** (D6, D34).

## Acceptance criteria

**The board side**

- `ClaimTarget::Next { capacity, repositories, wait }` and `FreeCapacity` exist with D31
  point 2's fields and types. `BoardMethod` is unchanged, because `Next` is a claim.
- `selection::plan` takes the runner's view. `selection::first_startable` exists as a pure
  function, with unit tests in `selection.rs` for: board order, a skipped entry passed over,
  a repository with no free slot, a repository missing from `per_repository` (no slot), and
  `total == 0`.
- **Contract cases** in `crates/core/src/testing/board_contract.rs`, acting only through
  `Harness::runner()` and arranging only through `Harness::board()`, pass through the
  in-process adapter:
  - `a_next_claim_takes_the_top_startable_task_in_board_order`;
  - `a_next_claim_passes_over_a_repository_the_runner_did_not_list`;
  - `a_next_claim_passes_over_a_repository_whose_team_ceiling_is_off`;
  - `a_next_claim_honours_each_repositorys_free_slots`;
  - `a_next_claim_with_no_free_capacity_claims_nothing_and_writes_nothing`;
  - `a_next_claim_resumes_a_due_retry_with_the_session_it_continues`;
  - `two_runners_claiming_next_for_one_task_get_exactly_one_claim` (the two claims are
    joined concurrently; exactly one is `Some`, and the task is `running` once);
  - `a_waiting_next_claim_returns_as_soon_as_a_task_becomes_startable` (a task is moved to
    `ready` after the claim began; the clock is not advanced);
  - `a_waiting_next_claim_returns_none_once_its_wait_has_passed` (the `TestClock` is
    advanced past `wait`; nothing is written).
- `the_plan_and_the_next_claim_agree_on_what_starts_first`: for a fixture board with skips
  of every kind, the entry with `queue_position: Some(1)` in `status_with_plan` is the task
  the next `claim(Next)` returns.

**The runner side**

- `crates/runner/src/queue/` holds the loop, `QueueHandle`, `QueueTask` and `supervise`.
  `crates/core/src/scheduler/queue.rs` no longer exists, and nothing in `rimaia-core`
  exports a `QueueHandle`.
- **The loop reads the board only through the port.** Nothing under
  `crates/runner/src/queue/` names `rimaia_core::tasks`, `rimaia_core::repo`,
  `scheduler::selection`, `scheduler::claim`, `scheduler::attempts`, `set_run_state` or
  `.pool`. The one exception is `status_with_plan`, which calls `selection::plan` and says
  so in its doc comment. A reviewer checks this with one `grep`.
- **Every existing scheduler test still passes, and its assertions are unchanged.** Every
  `#[tokio::test]` name in `crates/core/tests/scheduler.rs` at 041's tip exists afterwards
  in exactly one of `crates/core/tests/scheduler.rs` and `crates/runner/tests/queue.rs`.
  039's `the_queue_never_claims_another_teams_task` is among them. In each moved test,
  only import lines, fixture construction and synchronisation helpers differ. The three
  `…_mid_claim_…` tests wait for the held version probe instead of for a slot (point 5),
  and assert exactly what they assert today.
- **Per runner, with two runners over one board.** Each test builds two loops from the
  contract harness's runners `A` and `B`, each with its own runner store in a `TempDir`,
  its own `InFlight` and its own checkout clone. They use a `TestClock`, real git, and
  `FakeCli` replaying fixture streams. No test sleeps.
  - `pausing_one_runners_queue_leaves_the_other_working`: A is paused, and B still starts
    the next task. A's `queue_state` is `paused` and B's is `running`, each read from its
    own store.
  - `a_usage_limit_on_one_runner_holds_only_that_runner`: A's run ends on the recorded
    usage-limit stream. A starts nothing until its reset, and B starts the next task
    before the clock is advanced.
  - `each_runner_applies_its_own_repository_cap`: two ready tasks in one repository, a cap
    of 1 on both runners, and both tasks running at once, one on each runner.
- `a_queue_that_cannot_start_anything_spawns_no_probe`: with the switch off, then with a
  usage-limit hold, then with every slot taken, a board change wakes the loop, and
  `FakeCli` records no version probe and the board records no claim.
- `a_missing_binary_is_found_before_the_queue_claims_anything`: with no `claude`, a started
  queue records a step error, and every ready task stays `idle`.
- `a_consent_withdrawn_after_the_claim_releases_before_anything_spawns`: the runner's
  consent is revoked from inside a `BoardPort` decorator's `claim`, after the board has
  answered. No process starts, and the claim is released.
- `the_queue_status_payload_is_unchanged`: `status_with_plan` for a fixture board
  serialises to a pinned JSON string, asserted exactly, whose keys and nesting are today's
  `QueueStatus`.

**The rename**

- `grep -rnE '\bLease\b|LeaseOwner|LeaseRefused' crates src-tauri` finds nothing.
  `LeaseRef`, `LeasePurpose` and later lease types are not matched. `grep -ni lease
  crates/core/src/scheduler/inflight.rs` finds nothing.
- `src-tauri/src/commands/strategy.rs` and `commands/runs.rs` use `SlotOwner::Manual`.

**The stricter of team and runner**

- Unit tests of `runner::limits::effective`, asserting whole values:
  - `with_no_runner_override_the_team_values_apply_unchanged`;
  - `a_runner_may_lower_the_turn_budget`;
  - `a_runner_cannot_raise_the_turn_budget_above_the_team_ceiling`;
  - `the_default_blocklist_survives_a_runner_addition`;
  - `an_explicitly_empty_team_blocklist_forbids_only_what_the_runner_adds`;
  - `a_rule_on_both_lists_is_forbidden_once`;
  - `the_operations_are_ordered_team_then_runner_then_the_callers_own`.
- In the runner crate: `an_unusable_runner_turn_budget_reads_as_no_override`, for `0`,
  `-1` and `lots`.
- Through real spawns, with `FakeCli` recording argv, asserting the exact `--max-turns`
  value and the exact blocklist argument:
  - `a_queued_run_is_spawned_with_the_stricter_turn_budget_and_both_blocklists`. The team
    sets 300 and one rule, the runner sets 40 and a second rule, and the legacy `settings`
    table holds a third value for both keys, which must appear nowhere.
  - `a_manual_run_gets_the_same_limits_as_a_queued_one`, through 036's starter function.
  - `the_planner_is_held_to_the_runners_turn_budget_only_when_it_is_lower`, with runner
    values of 3 and of 50 against the catalogue's 6.
- `grep -rn "fn max_turns\|fn disallowed_tools\|fn forbidden_operations"
  crates/core/src/runner/process.rs` finds nothing that takes a `SqlitePool`.

**Everything else**

- **Solo is unchanged at its edges.** `git diff --stat` shows no file under `src/`.
  `./scripts/check-command-wiring.sh` passes unchanged, and `mcp/scope.rs`'s `Tool::ALL` is
  unchanged.
- **No migration file is added** in either set.
- D19, D22 and D31 carry the dated amendments from Scope point 10, "How to use this" has
  042's row, and CLAUDE.md has the Gotchas bullet. 044's two references name
  `crates/runner/tests/queue.rs`.
- **By hand, listed in the PR body:** `RIMAIA_DATA_DIR=/tmp/rimaia-042 npm run tauri dev`
  against a copy of a real board. Start the queue, let one task run, pause, quit, and
  relaunch. The queue comes back `paused`, the Runs view shows the same plan as before
  the task, and `runner.db`'s `queue_state` is the value that changed.
- **Every CI check passes**, with `SQLX_OFFLINE=true` exported: `npm run typecheck`,
  `npm run test`, `npm run build`, `cargo test -p rimaia-core`,
  `cargo test -p rimaia-runner`, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo clippy -p rimaia-runner --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`. Both
  `.sqlx` caches are current.

## Notes

**Read first.**

- [ADR-0031](../docs/adr/0031-runners-claim-work-with-leases.md) points 1, 5 and 6: the
  board chooses, and queue control belongs to the runner.
- [ADR-0010](../docs/adr/0010-execution-scheduler.md): Selection, and its 2026-09-03
  amendment. Reaching a window's stop time is `pause`, and a schedule is not queue state.
- [ADR-0028](../docs/adr/0028-the-server-owns-the-board-and-each-runner-keeps-its-own-store.md)
  point 2: the placement table, and the stricter-of rule.
- ADR-0032 points 4 and 5, for why the runner lists only consented repositories and still
  re-checks.
- Seam entries: **D31 in full** (points 2, 4, 5, 7 and 14 in particular), **D19** and
  **D21** (every point; D21 point 5 is the one a loop rewrite breaks silently), **D15** and
  its amendment, **D22** point 2, **D24** (a window overrides mode and concurrency, inside
  `capacity::resolve`), **D28** part 4, **D29** (the resume point a `Next` claim returns),
  and **D33** point 3 (the recipe). Read D4 and D6 as prohibitions.

**Files to start from.**

- `crates/core/src/scheduler/mod.rs` (the header being rewritten), `queue.rs` (`build`,
  `try_step`, `interrupted_since`, `supervise`, `run`), `selection.rs` (`plan`,
  `next_batch`, `skip_reason`), `capacity.rs`, `inflight.rs`, `claim.rs`, `pause.rs` and
  `state.rs`.
- `crates/core/src/schedule/{mod,window,fire,preflight}.rs`, wherever 041 left them.
- `crates/core/src/board/{port,types,service,in_process}.rs` (036), and
  `crates/core/src/testing/board_contract.rs` (036).
- `crates/core/src/runner/process.rs` (`max_turns`, `disallowed_tools`,
  `forbidden_operations`, `run_task`'s intent construction) and `runner/strategy.rs`
  (`planner_intent`, `claim_for_planning`, `PlannerClaim`).
- `crates/core/src/mcp/server.rs` (the Plan now slot), `crates/core/src/testing/cli.rs`
  (`hold_version_probe`; add a probe counter if it has none).
- `src-tauri/src/lib.rs` (the `scheduler::build` call and `shut_down`),
  `src-tauri/src/state.rs`, `src-tauri/src/notify.rs`, and
  `src-tauri/src/commands/{queue,runs,strategy,schedules}.rs`.
- `crates/core/tests/scheduler.rs`, `runner_process.rs` (the argv assertions to copy),
  `runner_strategy.rs` and `doctor.rs`.

**Migration:** none.

**What 041 provides, and what this task assumes about it.** A `RunnerStore` held on
`AppState`, and `runner_settings` as the only source of `queue_state`, `schedule_mode`,
`max_concurrency`, `active_run_window` and `usage_limit_pause_until`. `checkouts` carries
each repository's `max_concurrency` and `unattended_consent`, and `schedules` lives in
`runner.db`. The local queue and schedule MCP tools are injected by the host, and there is a
route by which `run_task` receives `run_environment` from the runner store. 041 may have
moved the loop itself into `crates/runner` in order to reach those values. If it did, point
1's move is already done, and the rest of this task stands. **If any of these is missing,
or reads differently in 041's diff, follow 041. If the runner-owned values still reach
`run_task` through a board `ServiceContext`, stop and ask. Do not add a second route for
the two new keys.**

**What 036, 038 and 039 provide.** 036: `BoardPort` without `Next`, the in-process adapter,
`Claim::resume`, the manual starter function (`runner/start.rs`), `TeamLimits` on
`RunContext`, and the contract harness with runners `A` and `B`. 038: the solo runner's id
on the in-process adapter, and `ChangeEvent` carrying its team. If the harness's `B` still
shares `A`'s runner id, give `B` its own `runners` row through a `testing` helper. The
two-runner tests need it. 039: team settings read from `team_settings` for the task's team,
`subscription_monthly_usd` from `user_settings` for the actor, and the queue's selection
already scoped to one team.

**What the next tasks expect.**

- **043** replaces step 3 of `Next`'s body, and the bodies of `Run` and `Plan`, with one
  transaction that also writes a lease row and bumps `tasks.lease_generation`. It writes
  `held_leases` when the loop receives a `Claim`, and gives the loop the reaction to
  `Conflict` that D31 point 11 describes. It needs every starter already going through
  `claim`, and the slot and the lease under different names. After this task, both hold.
- **045** adds eligibility predicates beside the ceiling check in `Next`'s body, and
  consent re-checks on `preview`, `claim` and `run_context`.
- **052** serves `Next` over HTTP from the same `board::service` function, with the runner
  token scoping the team.
- **053** gives the loop a long poll: `wait` greater than zero over HTTP, and a way to give
  up a claim that has not committed when a slot frees (point 4).
- **059** replaces `status_with_plan`'s board half with an HTTP read.
- **061** adds the Settings control for the runner's two limits.

**Traps.**

- **D21 point 5 moves with the loop.** Keep the `releases` arm, the guarded `join_next`,
  and the drain as the last statement of `run`, each with its comment. A rewrite that
  "simplifies" any of them passes every test that does not free a slot from a manual run.
- **Do not let a claim's future be dropped** (point 4). A `select!` arm whose future is
  `board.claim(..)` compiles, and it strands a task the first time another arm wins.
- **`FreeCapacity` is net, and the board must not subtract again.** A `per_repository` of 1
  means one more run, not a cap of 1.
- **A window's mode and concurrency still override the defaults**, inside
  `capacity::resolve` (D24). `FreeCapacity` is computed from the resolved caps, never from
  the stored keys.
- **`release` lands a claimed task in `failed`.** Every early exit between a `Claim` and
  `supervise` releases, and the tests above cover each one. Do not add a softer "un-claim"
  edge to make that friendlier. ADR-0007's transitions do not have one, and 043 decides
  what a lease that never started a run leaves behind.

**Size.** L. Estimated diff, counting a `git mv` with light edits as its edited lines:

| Part | Lines |
| --- | --- |
| `Next`: body, `first_startable`, `plan`'s view, contract cases | ~600 |
| The loop move and the new `try_step` | ~500 |
| Test move (renames) and the new runner-side tests | ~900 |
| `runner::limits`, the runner keys, three call sites, tests | ~600 |
| The rename | ~250 |
| Shell, `status_with_plan`, docs and seam amendments | ~300 |
| **Total** | **~3,150** |

That fits one session. If it runs over, cut the stricter-of work (Scope point 6 and its
criteria) into a new task with the next free number, placed directly after 042 in
`tasks/README.md` and depending on 042. It shares no code with the split: it needs only
036's `TeamLimits` and 041's runner settings. Say so in the PR. Do not cut the rename or the
two-runner tests. The rename is the reason 043 can be written safely, and the two-runner
tests are the only proof that "per runner" holds.
