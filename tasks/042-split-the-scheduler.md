---
id: "042"
title: Split the scheduler into board selection and a runner loop
milestone: v0.5
status: ready
depends_on: ["041", "066"]
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
- **Runner side.** The queue loop moves to `crates/runner`. The rules it obeys (the go
  signal, capacity, run windows and schedules, the usage-limit pause) stay in `rimaia-core`
  over 041's `MachineContext`, so they are per runner already. The loop reaches the board
  through the port, apart from four named reads in one module (Scope point 5).

Three smaller changes ride with the split, because each is about which half owns what:

- **`InFlight`'s `Lease` becomes `LocalSlot`.** `LeaseOwner` and `LeaseRefused` become
  `SlotOwner` and `SlotRefused`. From here on, "lease" in this codebase means only the
  board's lease (`LeaseRef`, `LeasePurpose`, and 043's `runner_leases`).
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
per-repository cap counts *that* runner's runs. 041 and 066 moved where those values are
stored.
This task moves the loop that obeys them, and proves "per runner" with two runners over one
board while it is still one process and still testable with a `TestClock`.

The rename is cheapest now. D31 point 2 named the board type `LeaseRef` rather than `Lease`
because the in-process slot had that name "until 042 renames it `LocalSlot`". From 043 on,
both kinds of lease are live in one function (`try_step` holds a slot and a `LeaseRef` at
once), and a diff that confuses them compiles.

## Scope

Read D31 in full, and D19, D21, D22 and D23 with their amendments, before starting.
Everything below refines them and contradicts none of them.

**1. Where each piece lives when this task lands.**

| Piece | Lives in | Reads the board through |
| --- | --- | --- |
| selection: `plan`, `skip_reason`, `next_deadline`, `first_startable` | `rimaia_core::scheduler::selection` | `ServiceContext` (it *is* the board) |
| the claim edges: `claim`, `claim_retry`, `release`, `give_up` | `rimaia_core::scheduler::claim` | `ServiceContext`. 043 replaces the bodies |
| attempts, the retry policy, reconcile | `rimaia_core::scheduler::{attempts, retry, reconcile}` | `ServiceContext`. Per-runner reconcile is 043's |
| `ClaimTarget::Next`'s body | `rimaia_core::board::service` | `ServiceContext` |
| the go signal, the pause, capacity, windows, schedules: `scheduler::{state, pause, capacity}`, `schedule::{window, fire}`, the schedule CRUD | `rimaia-core`, over `&MachineContext` (041) | never |
| `schedule::preflight::preview` | `rimaia-core` | `selection::plan`, as today |
| the runner's view: `scheduler::view::for_runner` (point 5) | `rimaia-core`, over `&MachineContext` and `&InFlight` | never |
| `runner::limits` and the runner's two limit keys (point 7) | `rimaia-core`, over `&MachineContext` | never |
| `InFlight`, `LocalSlot`, `SlotOwner`, the preparation lock | `rimaia_core::scheduler::inflight` | nothing. It is in memory |
| the loop: `QueueTask`, `QueueHandle`, `supervise`, `try_step` | `crates/runner/src/queue/` | `BoardPort`, and `queue/solo.rs` only (point 5) |

Only the loop moves. The rules stay where 041 left them, because core's own local MCP
handlers (041's `LocalTools`: `get_run_capacity`, `set_max_concurrency`, the schedule tools,
`preview_schedule_preflight`) call them, and `rimaia-core` cannot depend on `rimaia-runner`.
`InFlight` stays in core because `run_task`'s preparation lock, the planner's
`claim_for_planning` and the MCP server's Plan now all hold it, and all three are core code.
ADR-0031 point 6 calls it "the runner's local registry". That describes what it means, not
which crate compiles it.

`rimaia_core::scheduler::queue` is deleted, and nothing re-exports it under its old path. A
second path to a loop is how two loops happen. `rimaia_core::scheduler`'s module header is
rewritten to describe what is left: the board half of the scheduler, the per-runner rules
over `MachineContext`, and the slot registry. Its "The six pieces" section currently
describes a loop that no longer lives there.

**2. `ClaimTarget::Next` (D31 points 2 and 4).** 036 left it out on purpose ("a variant whose
only body is a refusal cannot be told apart from a bug"). This task adds it and gives it a
real body:

```rust
ClaimTarget::Next { capacity: FreeCapacity, repositories: Vec<String>, wait: Duration }
pub struct FreeCapacity { pub total: usize, pub per_repository: BTreeMap<String, usize> }
```

The shape is D31's. What each field means, as this task pins it (and records in a dated D31
amendment):

- **`repositories`** lists the repositories this runner has a checkout for *and* has given
  unattended consent for (`checkouts.unattended_consent`, read through `MachineContext`
  since 066). **In 042 that list is the whole opt-in: a repository is opted in exactly when
  the runner listed it.** That is today's single toggle, read from where 066 moved it.
  Listing only consented checkouts keeps the board from offering a task the runner would
  refuse after the claim, which is a `release`, which lands the task in `failed`. 045 adds
  the team ceiling beside this check, with its personal-team exemption, because a
  repository registered after 066 has ceiling column `0`. Reading the ceiling here would
  stop the solo queue for every such repository.
- **`capacity`** is what the runner has *free*, already net of its own in-flight runs
  (point 5). **A repository that appears in `repositories` and is missing from
  `per_repository` has no free slot.** This is the conservative reading `next_batch` gives
  a missing key today ("never unbounded"), applied to a value that is already net.
- **`wait`** is a `std::time::Duration`, serialised as integer milliseconds (`"wait": 0`)
  through a small serde helper, so 052's JSON has one form. In process it is honoured as D31
  point 4 says: by waiting on `ServiceContext::subscribe` and `Clock::sleep_until`, never
  `tokio::time::sleep`. The body tries once, waits until a change event or the deadline,
  tries again, and returns `None` when the deadline passes with nothing claimed. **The solo
  loop always passes `Duration::ZERO`** (point 4). The waiting form has contract cases now,
  so 053 inherits a tested body and adds the clamp and the `earliest_due` wake to it.

The body, in `board::service`, is one function that the in-process adapter calls and 052's
HTTP handler will call:

1. `selection::plan(ctx, &repositories)`. `plan` gains this parameter. A repository not in
   it gets `SkipReason::UnattendedRunsNotAllowed`, exactly as today's opt-in does.
   `skip_reason` stays a pure function and learns nothing new.
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
team ceiling and the assignee, consent and trust predicates beside the listed-repository
check, and 043 replaces step 3 with the lease transaction. Neither should touch the runner.

**3. The loop moves to `crates/runner/src/queue/`.** It keeps its shape: the same arms, the
same order (shutdown check, drain, `tick_schedules`, step), the same `DEADLINE_CAP`, and the
`JoinSet` drain as the last statement of `run`. D21 point 5 and the module header's
arguments move with it, verbatim where they still hold. What changes is `try_step`, in this
order:

1. **Look without spending anything.** Read the switch and the pause through
   `MachineContext`, and build the view with `scheduler::view::for_runner`. If the switch is
   off, the pause is active (`IdleUntil` its end, as today) or `FreeCapacity::total` is
   zero, the pass ends: no probe, no claim.
2. **The probe, memoised.** The loop keeps `probe_cli`'s last answer, success or failure,
   with the clock instant it was taken, and reuses it until `DEADLINE_CAP` has passed on the
   injected clock. `QueueHandle::start` and `resume` clear it, because the doctor they run
   has just asked the same question. A failed answer is the step error it is today. The
   memo exists because the loop can no longer tell whether anything is startable before it
   claims: today's probe runs only before a non-empty batch, which the loop knew from its
   own plan read, and a probe per pass would be the per-change spawn D22 point 2 argues
   against.
3. **Re-check the switch and shutdown** (`interrupted_since`, with no cancel signal yet,
   because nothing is held). If either changed, the pass ends.
4. **Then, one claim at a time:** re-derive the view, so the slots this pass already took
   are counted, and stop when `total` is zero; `claim(Next { wait: ZERO })`, stopping on
   `None`; re-check the switch and shutdown, and `release` if either changed; take the
   `LocalSlot` with `acquire` under the resolved caps; spawn `supervise` with the claim and
   the slot. Claiming several before acquiring any would derive `FreeCapacity` from slots
   not yet taken, over-claim, and release the surplus into `failed`.

A slot refused after a won claim has two causes. A capacity refusal means a Run now on this
runner took the slot after the view was built. The loop then takes it with
`acquire_unbounded`: the board decided on the capacity the runner reported, and the
overshoot is one run a person started on purpose, still under `CONCURRENCY_CEILING`.
`AlreadyInFlight` means a Run now for this very task sits between its slot and its own
claim. The loop releases, and that start then loses its `claim(Run)` and says so, as a lost
race does today. A ceiling refusal from `acquire_unbounded` also releases.

The loop does not re-check consent before spawning. That re-check is 045's point 7, at the
last point before the agent process starts.

`build` takes 041's `MachineContext`, an `Arc<dyn BoardPort>`, a board-change receiver,
point 5's `SoloBoard`, the `InFlight`, `AppPaths` and the `RunnerConfig`. It no longer takes
a bare board `ServiceContext`. `run_task` and `doctor::run` are called with what they take
after 041. This task changes neither signature, apart from point 7's limits.

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
and it replaces today's `changes` arm one for one. `releases`, the signals and the join arm
are unchanged.

**The deadline arm keeps its source.** Today an empty batch returns
`IdleUntil(selection::next_deadline(&plan))`, and that is the only thing that wakes the loop
when a `waiting_retry` task's `resume_after` arrives, because nothing publishes an event
then (D23 point 1). Under `Next` the loop holds no plan, and `None` carries no deadline. So a
pass that ends with the switch on, no pause and nothing spawned on its last try returns
`IdleUntil(SoloBoard::next_deadline(&repositories))` when there is one, capped by
`DEADLINE_CAP` as today. That is every case today's empty batch covers, a full queue
included. D31's `Option<Claim>` does not change. For a remote runner, 053's long poll wakes
board-side at the same instant (its `earliest_due`).

The long poll, and cancelling a claim that has not yet committed, are 053's. They need a
one-transaction claim (043) and lease expiry to recover a reply that never arrived (D31
point 4, `heartbeat`). Record this point in the D31 amendment.

**5. The runner's view, and the four board reads the solo loop keeps.**
`scheduler::view::for_runner(&MachineContext, &InFlight) -> Result<(Vec<String>,
FreeCapacity)>` builds the consented checkouts and the free capacity: `capacity::resolve`
(the window, the mode, `max_concurrency`, each checkout's `max_concurrency`) minus
`InFlight::counts()`. It is the one builder. The loop, `status_with_plan` and
`schedule::preflight::preview`'s core function all call it, so the plan a card shows, the
plan a schedule previews and the plan a claim acts on come from one view.
`preflight::preview` gains the repository list as a parameter, and both its doors (the
Tauri command and the local MCP handler) build it here.

`crates/runner/src/queue/solo.rs` holds `SoloBoard`, a wrapper over the board
`ServiceContext` the shell passes. It is the only code in `crates/runner/src/queue/` that
holds one, and it exposes exactly four reads:

- `next_deadline(&repositories)`: `selection::plan` and `next_deadline`, for point 4;
- the plan half of `status_with_plan`, below;
- the doctor: `QueueHandle::start`, `resume` and `open_window` run `doctor::run`, which reads
  the board's repository list. D22 point 1's gate stays where it is;
- the fire-time preflight log: `open_window`'s `schedule::preflight::preview`.

Each is a read. `SoloBoard` also hands `supervise` the board context 041 left on
`run_task` for `worktree::prepare`, until 044 removes it. Every write the loop causes goes
through the port or through `MachineContext`. 058's headless runner and 059's connected
mode have no board context, and this module is the one place they replace.

`QueueStatus` keeps its wire shape. `rimaia_runner::queue::status_with_plan` joins the runner
half from the handle with `SoloBoard`'s plan, built from the same view the loop sends with
`Next`. The `get_queue_status` command calls it.

**6. The slot is taken after the claim when the board picks the task.** D19 and the queue's
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

**7. The stricter of team and runner (ADR-0028 point 2).**

- **The runner half.** Two `runner_settings` keys, `max_turns` and `disallowed_tools`, read
  by one typed accessor in core, `runner::limits::runner_limits(&MachineContext) ->
  Result<RunnerLimits>`, beside 041's accessors (D3). Both are absent by default, and absent
  means no override. **Neither is ever adopted.** They stay out of
  `db::settings::RUNNER_KEYS`, `runner_placed` and 040's adoption step, although the board's
  `settings` holds team keys of the same names: adopting them would copy the team's value
  into the runner's override, against D28 part 4's "the runner's stricter override starts
  out absent". Reads are tolerant. A `max_turns` that is unparseable or `0` warns and reads
  as absent, never as `0` and never as the team's value. The blocklist is one pattern per
  line, blank lines ignored, which is today's `disallowed_tools` format. There is no
  command, no MCP tool and no UI: they are set in the sqlite3 CLI (ADR-0003), and a control
  is 061's.
- **The rule.** Pure functions in `rimaia_core::runner::limits`, over `TeamLimits` (D31) and
  a new `RunnerLimits { max_turns: Option<u32>, disallowed_tools: Vec<String> }`:
  - `effective(team: &TeamLimits, runner: &RunnerLimits, provider: ProviderId, extra: impl
    IntoIterator<Item = ForbiddenOperation>) -> EffectiveLimits`. `max_turns` is
    `min(team, runner)` when the runner set one, and the team's otherwise. The forbidden
    operations are the team's first: `claude::DEFAULT_FORBIDDEN` when
    `TeamLimits::disallowed_tools` is `None`, and one `ProviderRule` per stored rule
    otherwise, an explicitly empty list included (D27). Then come the runner's rules not
    already present, as `ProviderRule`s tagged with `provider`. Then `extra`
    (`RIMAIA_TOOL_SURFACE`, the planner's own). The order is fixed, so the argv is
    deterministic.
  - `planner_max_turns(catalogue: u32, runner: &RunnerLimits) -> u32`, which is
    `min(catalogue.planner.max_turns, runner)`. The team's `max_turns` keeps capping only
    the runs it caps today, so a solo planner is unchanged. The planner's operations still
    come from `effective`.
  - The runner can only add. Nothing in `RunnerLimits` can remove a team rule or raise a
    budget, and the type makes no room for it.
- **Every process a runner starts goes through them** (ADR-0032 point 5): the
  implementation, review and fix phases of `run_task` (021's phase loop builds each phase's
  intent through `effective`), whether the run was queued, Run now or Retry now, and the
  planner. The runner half is read through `MachineContext` when each run starts, by the
  route 041 gave `run_environment`, and never cached across runs. The team half is read
  only where `board::service` builds `TeamLimits`. `runner/process.rs` and
  `runner/strategy.rs` keep no reader of either.

**8. The rename.** `Lease` → `LocalSlot`, `LeaseOwner` → `SlotOwner`, `LeaseRefused` →
`SlotRefused`, across `crates/core/src/scheduler/inflight.rs`, `runner/start.rs` (036's
Run now), `runner/strategy.rs`, `mcp/server.rs`, the moved queue,
`src-tauri/src/commands/strategy.rs`, `commands/runs.rs` if it still names the owner, and
every test, `crates/core/tests/runner_strategy.rs` included. Method names (`acquire`,
`acquire_unbounded`, `cancel_owned_by`, `releases`) do not change, and neither does
`PlannerClaim`'s shape. Doc comments that call the slot a "lease" say "slot". The
`releases` arm's comments keep their argument and change their noun.

**9. The shell.** `src-tauri/src/lib.rs` builds the queue with
`rimaia_runner::queue::build`, passing 041's `machine`, 036's `board_port`,
`context.subscribe()`, a `SoloBoard` over the board context and the one `InFlight`.
`AppState::queue`, `notify.rs` and `commands/queue.rs` change their import paths.

**10. Tests move with their code.** Loop tests leave `crates/core/tests/scheduler.rs` for
`crates/runner/tests/queue.rs`, keeping `#![cfg(unix)]` and the file's header argument. The
claim, selection and reconcile tests stay in core. `rimaia-core`'s tests cannot name
`rimaia-runner` (040's `rimaia_core_does_not_depend_on_rimaia_runner`), so a test that
builds a loop has to live in the runner crate. In the same commit, update the two
references to `crates/core/tests/scheduler.rs` in
`tasks/044-branch-from-the-dependencys-commit.md`, and in 045 mark the runner test
`the_queue_offers_the_board_only_consented_repositories` as built by 042. 045 keeps its
pre-spawn re-check and `a_runner_refuses_to_spawn_where_it_never_consented_…`.

**11. Documentation.**

- Seam contract, dated amendments:
  - **D19:** the rename, and point 6's ordering.
  - **D22:** the probe runs at most once per `DEADLINE_CAP` of clock time on a pass with a
    free slot, rather than before each non-empty batch. With free capacity, a board change
    costs at most one `--version` spawn a minute whether or not anything is ready. A pass
    that is switched off, held or full spawns nothing. A `claude` removed while the queue
    runs is noticed within the same minute, and one claimed task can fail at spawn before
    it is.
  - **D28 part 4:** the runner row gains `max_turns` and `disallowed_tools`, which are never
    adopted.
  - **D31:** point 2's field meanings and `wait`'s type and JSON form; point 4's
    `wait: ZERO` and the deadline's solo source; point 8, where `build` is now
    `rimaia_runner::queue::build` and takes the `MachineContext`; point 13, where 042 owns
    the `Next` race case and 043's race case is its lease form.
- "How to use this" gets 042's row: D3 · D15 · D19 · D21 · D22 · D23 · D24 · D27 ·
  D28 · D29 · D31 · D32 · D33, with D4, D6 and D34 as prohibitions.
- CLAUDE.md, one Gotchas bullet: `max_turns` and `disallowed_tools` have a team value and a
  runner value, the effective value is the stricter of the two (ADR-0028 point 2), and a
  run is never built from either half alone.
- Regenerate both offline caches with D33 point 3's recipe if any query changed.

## Out of scope

- **Leases.** No `runner_leases` row, no generation other than `0`, no fencing, no
  `Conflict`, no pinning, and no `held_leases` write. `claim.rs`'s two transactions stay
  two. All of that is 043, which replaces step 3 of point 2's body and nothing on the runner.
- **Per-runner reconcile and `startup::survey`.** They stay as they are (043).
- **The long poll**, a `wait` greater than zero from a production caller, heartbeats, and
  expiry (053). Point 4 says what 053 inherits.
- **Eligibility beyond the runner's listed repositories:** the team ceiling, assignment, the
  three revisions, acceptances, the trust list, the model and effort cap, and the runner's
  re-check before it spawns. These are 045's.
- **A runner serving more than one team.** In process, the claim runs under the adapter's
  context, which in solo is the one team. Scoping `Next` for a runner whose owner is in
  several teams is 052's, with its runner token.
- **Run now for a specific runner** (ADR-0031 point 7, 052) and **run elsewhere** (057).
  No `SkipReason` for "this runner has no checkout" is ever added: 054 limits `Next` to
  mapped repositories, and 061's board line shows a repository nobody maps.
- **Moving any stored value or any rule.** 039 moved team and user settings, and 041 moved
  runner settings, checkouts and schedules. This task adds two runner keys and moves no row.
- **A settings control** for the runner's `max_turns` and `disallowed_tools` (061).
- **Anything under `src/`**, and any new Tauri command or MCP tool. `check-command-wiring.sh`
  and `mcp/scope.rs`'s tool table do not change.
- **Any migration** (D4). The two runner keys are `runner_settings` rows.
- **Any dependency** (D6, D34).

## Acceptance criteria

**The board side**

- `ClaimTarget::Next { capacity, repositories, wait }` and `FreeCapacity` exist with D31
  point 2's fields, `wait` a `std::time::Duration` that serialises as integer milliseconds.
  `BoardMethod` is unchanged, because `Next` is a claim.
- `selection::plan` takes the repository list. `selection::first_startable` exists as a pure
  function, with unit tests in `selection.rs` for: board order, a skipped entry passed over,
  a repository with no free slot, a repository missing from `per_repository` (no slot), and
  `total == 0`.
- **Contract cases** in `crates/core/src/testing/board_contract.rs`, acting only through
  `Harness::runner()` and arranging only through `Harness::board()`, pass through the
  in-process adapter:
  - `a_next_claim_takes_the_top_startable_task_in_board_order`;
  - `a_next_claim_passes_over_a_repository_the_runner_did_not_list`;
  - `a_next_claim_takes_a_listed_repository_whatever_its_ceiling_column_says` (registered
    through core services, which after 066 leave the column `0`; pins that 042 reads no
    ceiling);
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
- **The loop reads the board only through the port and `queue/solo.rs`.** Outside
  `solo.rs`, nothing under `crates/runner/src/queue/` names `ServiceContext`,
  `rimaia_core::tasks`, `rimaia_core::repo`, `scheduler::selection`, `scheduler::claim`,
  `scheduler::attempts`, `set_run_state`, `doctor::run`, `schedule::preflight` or `.pool`.
  `solo.rs` exposes exactly the four reads of Scope point 5 and 044's temporary context,
  each saying so in its doc comment. A reviewer checks this with one `grep`.
- **Every existing scheduler test still passes, and its assertions are unchanged.** Every
  `#[tokio::test]` name in `crates/core/tests/scheduler.rs` at 066's tip exists afterwards
  in exactly one of `crates/core/tests/scheduler.rs` and `crates/runner/tests/queue.rs`.
  039's `the_queue_never_claims_another_teams_task` is among them. In each moved test,
  only import lines, fixture construction and synchronisation helpers differ. The three
  `…_mid_claim_…` tests hold the version probe instead of a slot (point 6). After
  releasing it, each waits until `FakeCli`'s count of returned probes is 1 **and** the
  handle's completed-pass count (a `testing` accessor) has moved past its value at the
  release, then asserts exactly what it asserts today.
- **Per runner, with two runners over one board.** Each test builds two loops from the
  contract harness's runners `A` and `B`, each with its own `MachineContext` over its own
  `RunnerStore` in a `TempDir`, its own `InFlight` and its own checkout clone. They use a
  `TestClock`, real git, and `FakeCli` replaying fixture streams. No test sleeps.
  - `pausing_one_runners_queue_leaves_the_other_working`: A is paused, and B still starts
    the next task. A's `queue_state` is `paused` and B's is `running`, each read from its
    own store.
  - `a_usage_limit_on_one_runner_holds_only_that_runner`: A's run ends on the recorded
    usage-limit stream. A starts nothing until its reset, and B starts the next task
    before the clock is advanced.
  - `each_runner_applies_its_own_repository_cap`: two ready tasks in one repository, a cap
    of 1 on both runners, and both tasks running at once, one on each runner.
- `a_transient_retry_resumes_at_its_backoff_with_no_board_change`: a run ends on a
  transient stream, the loop goes idle, and advancing the `TestClock` past `resume_after`,
  with no `ChangeEvent` published, starts the retry.
- `a_queue_that_cannot_start_anything_spawns_no_probe`: with the switch off, then with a
  usage-limit hold, then with every slot taken, a board change wakes the loop, and
  `FakeCli` records no version probe and the board records no claim.
- `an_idle_board_costs_at_most_one_probe_per_deadline_cap`: running, free capacity, an
  empty `ready` column. Two board changes inside one `DEADLINE_CAP` record one probe.
  After the clock passes `DEADLINE_CAP`, one more change records a second.
- `a_missing_binary_is_found_before_the_queue_claims_anything`: with no `claude`, a started
  queue records a step error, and every ready task stays `idle`.
- `the_queue_offers_the_board_only_consented_repositories`: two checkouts, one with
  consent; a `BoardPort` decorator records every `Next`'s `repositories`, which name only
  the consented one.
- `a_capacity_refusal_after_a_won_claim_still_starts_the_task`: a Run now takes the last
  slot in the repository from inside a `BoardPort` decorator's `claim`, after the board has
  answered. The queued task still starts, and both runs are in flight.
- `the_queue_status_payload_is_unchanged`: `status_with_plan` for a fixture board
  serialises to a pinned JSON string, asserted exactly, whose keys and nesting are today's
  `QueueStatus`.

**The rename**

- `grep -rnE '\bLease\b|LeaseOwner|LeaseRefused' crates src-tauri` finds nothing.
  `LeaseRef`, `LeasePurpose` and later lease types are not matched. `grep -ni lease
  crates/core/src/scheduler/inflight.rs` finds nothing.
- Every site that names the manual owner (`runner/start.rs`, `commands/strategy.rs`, and
  `commands/runs.rs` if it still does) uses `SlotOwner::Manual`.

**The stricter of team and runner**

- Unit tests of `runner::limits`, asserting whole values:
  - `with_no_runner_override_the_team_values_apply_unchanged`;
  - `a_runner_may_lower_the_turn_budget`;
  - `a_runner_cannot_raise_the_turn_budget_above_the_team_ceiling`;
  - `the_default_blocklist_survives_a_runner_addition`;
  - `an_explicitly_empty_team_blocklist_forbids_only_what_the_runner_adds`;
  - `a_rule_on_both_lists_is_forbidden_once`;
  - `the_operations_are_ordered_team_then_runner_then_the_callers_own`, through `extra`;
  - `the_planner_budget_is_the_lower_of_the_catalogue_and_the_runner`.
- In core, against `MemoryMachine`: `an_unusable_runner_turn_budget_reads_as_no_override`,
  for `0`, `-1` and `lots`.
- In the runner crate: `a_fresh_adoption_leaves_both_runner_limit_keys_absent`, with both
  keys set in the board's `settings`.
- Through real spawns, with `FakeCli` recording argv, asserting the exact `--max-turns`
  value and the exact blocklist argument:
  - `a_queued_run_is_spawned_with_the_stricter_turn_budget_and_both_blocklists`. The team
    sets 300 and one rule, the runner sets 40 and a second rule, and the legacy `settings`
    table holds a third value for both keys, which must appear nowhere.
  - `a_manual_run_gets_the_same_limits_as_a_queued_one`, through 036's starter function.
  - `a_review_phase_is_spawned_with_the_same_limits_as_its_implementation_phase`.
  - `the_planner_is_held_to_the_runners_turn_budget_only_when_it_is_lower`, with runner
    values of 3 and of 50 against the catalogue's 6.
- `runner/process.rs` and `runner/strategy.rs` define and call no reader of `max_turns`,
  `disallowed_tools` or `forbidden_operations`. Nothing under `crates/runner/` names
  `subscription_monthly_usd`, `team_settings` or `user_settings`. Both are one `grep`.

**Everything else**

- **Solo is unchanged at its edges.** `git diff --stat` shows no file under `src/`.
  `./scripts/check-command-wiring.sh` passes unchanged, and `mcp/scope.rs`'s `Tool::ALL` is
  unchanged.
- **No migration file is added** in either set.
- D19, D22, D28 and D31 carry the dated amendments from Scope point 11, "How to use this"
  has 042's row, and CLAUDE.md has the Gotchas bullet. 044's two references name
  `crates/runner/tests/queue.rs`, and 045 names 042 for the listing test.
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
- ADR-0032 point 4, for why the runner lists only consented repositories.
- Seam entries: **D31 in full** (points 2, 4, 7, 8, 13 and 14 in particular), **D19** and
  **D21** (every point; D21 point 5 is the one a loop rewrite breaks silently), **D15** and
  its amendment, **D22** points 1 and 2, **D23** points 1–3 and 5 (`sleep_until`, the
  deadline arm, the usage-limit hold), **D24** (a window overrides mode and concurrency,
  inside `capacity::resolve`), **D3** (typed accessors beside their owner), **D27**
  (`ProviderRule` tagging), **D28** part 4, **D29** (the resume point a `Next` claim
  returns), **D32** point 8 (local handlers' board reads) and **D33** point 3 (the recipe).
  Read D4, D6 and D34 as prohibitions.

**Files to start from.**

- `crates/core/src/scheduler/mod.rs` (the header being rewritten), `queue.rs` (`build`,
  `try_step`, `interrupted_since`, `open_window`, `supervise`, `run`), `selection.rs`
  (`plan`, `next_batch`, `next_deadline`, `skip_reason`), `capacity.rs`, `inflight.rs`,
  `claim.rs`, `pause.rs` and `state.rs`.
- `crates/core/src/machine/` (041), `crates/core/src/schedule/{window,fire,preflight}.rs`,
  and `crates/core/src/doctor/mod.rs` (`run`).
- `crates/core/src/board/{port,types,service,in_process}.rs` (036), and
  `crates/core/src/testing/board_contract.rs` (036).
- `crates/core/src/runner/process.rs` (`max_turns`, `disallowed_tools`,
  `forbidden_operations`, `run_task`'s intent construction and phase loop),
  `runner/start.rs` (036) and `runner/strategy.rs` (`planner_intent`,
  `claim_for_planning`, `PlannerClaim`).
- `crates/core/src/mcp/server.rs` (the Plan now slot, `LocalTools`),
  `crates/core/src/testing/cli.rs` (`hold_version_probe`; add a returned-probe counter if
  it has none).
- `src-tauri/src/lib.rs` (the `scheduler::build` call and `shut_down`),
  `src-tauri/src/state.rs`, `src-tauri/src/notify.rs`, and
  `src-tauri/src/commands/{queue,runs,strategy,schedules}.rs`.
- `crates/core/tests/scheduler.rs`, `runner_process.rs` (the argv assertions to copy),
  `runner_strategy.rs` and `doctor.rs`.

**Migration:** none.

**What 041 and 066 provide, and what this task assumes about them.**
`rimaia_core::machine::{MachineStore, MachineContext}`, a `MachineContext` on `AppState` as
`machine`, and `TestContext::machine()` over `MemoryMachine`. Every typed accessor of the
ten runner keys, the schedule CRUD and `scheduler::build` take `&MachineContext`. `run_task`
takes `&MachineContext`, and keeps a board context for `worktree::prepare` alone until 044.
`checkouts` carries each repository's `max_concurrency` and `unattended_consent`, and 066
switched their readers (`capacity::resolve`'s per-repository caps,
`ensure_unattended_runs_allowed`) to `MachineContext`. `LocalTools` serves the machine MCP
tools from core. This task reads runner state only through `MachineContext` and moves no
storage. **If any of this reads differently in 041's or 066's diff, follow it. If the
runner-owned values still reach `run_task` through a board `ServiceContext`, stop and ask.
Do not add a second route for the two new keys.**

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
  `held_leases` when the loop receives a `Claim`, and keeps step 3's "a lost race moves to
  the next entry". The loop's reaction to `Conflict` (D31 point 11) is 053's. It needs every
  starter already going through
  `claim`, and the slot and the lease under different names. After this task, both hold.
- **045** adds the team ceiling (with its personal-team exemption) and the eligibility
  predicates beside the listed-repository check in `Next`'s body, consent re-checks on
  `preview`, `claim` and `run_context`, and the runner's re-check before it spawns.
- **052** serves `Next` over HTTP from the same `board::service` function, with the runner
  token scoping the team.
- **053** gives the loop a long poll: `wait` greater than zero over HTTP, the board-side
  `earliest_due` wake, and a way to give up a claim that has not committed when a slot
  frees (point 4).
- **058** and **059** replace `queue/solo.rs`'s four reads for a runner with no board
  context.
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
  `supervise` releases, except point 3's capacity fallback. Do not add a softer "un-claim"
  edge to make that friendlier. ADR-0007's transitions do not have one, and 043 decides
  what a lease that never started a run leaves behind.

**Size.** L. Estimated diff, counting a `git mv` with light edits as its edited lines:

| Part | Lines |
| --- | --- |
| `Next`: body, `first_startable`, `plan`'s parameter, contract cases | ~600 |
| The loop move, the new `try_step`, the view, `solo.rs` | ~550 |
| Test move (renames) and the new runner-side tests | ~950 |
| `runner::limits`, the runner keys, the call sites, tests | ~600 |
| The rename | ~250 |
| Shell, `status_with_plan`, docs and seam amendments | ~300 |
| **Total** | **~3,250** |

That fits one session. If it runs over, cut the stricter-of work (Scope point 7 and its
criteria) into a new task with the next free number, placed directly after 042 in
`tasks/README.md` and depending on 042. It shares no code with the split: it needs only
036's `TeamLimits` and 041's `MachineContext`. Say so in the PR. Do not cut the rename or
the two-runner tests. The rename is the reason 043 can be written safely, and the
two-runner tests are the only proof that "per runner" holds.
