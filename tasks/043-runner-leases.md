---
id: "043"
title: Runner leases, fencing and pinning
milestone: v0.5
status: ready
depends_on: ["042"]
adrs: ["0031", "0011", "0016"]
size: L
landed: "#34"
---

# Runner leases, fencing and pinning

## Goal

Make the board the only authority on who is running what, as ADR-0031 decides, while there
is still exactly one runner and it lives in the same process. Every process a runner starts
for a task (an implementation run, a planner, a review, a fix) is claimed in **one**
transaction that takes the run-state edges, increments the task's lease generation and writes
a `runner_leases` row. Every report made under that lease carries the generation, and a report
whose generation is not current is refused with a new `Conflict` error code. A task whose run
was interrupted, or that is waiting to retry, is pinned to the runner that held it. Startup
recovery stops being a global sweep of every `running` task and becomes each runner
reconciling the leases it held.

**In solo, nothing a user can see changes.** The solo runner's leases never expire
(`LeaseTerm::Never`), and it is the only runner, so a pin can never exclude anyone. What
changes is that the crash window `scheduler/claim.rs` documents is closed, and the protocol
that tasks 052 and 053 put on the network already holds, with tests, in process. The two
claim rules that need a second runner before they can fire, the model rule and who may start
a run, are task 067's.

## Why now

Task 042 split selection from the runner loop, so the claim is already a board-side call
through `BoardPort::claim`. It still walks `scheduler::claim`'s two separately committed
`set_run_state` edges and returns generation `0` (D31 point 9: "before 043 there is no lease
row"). Every later team-mode task depends on the lease being real:

- 044 branches from a commit that a lease-fenced `finish_run` recorded;
- 067 and 045 add the model rule and consent to the claim's eligibility check, which this
  task makes a single function;
- 052 serves `BoardPort` over HTTP, where a stale runner is a normal event and not a bug;
- 053 expires leases, and needs the pin, the fencing and the reconcile this task writes;
- 057 releases pins.

Landing the lease in process first means the rules are proven against a faked clock and a real
store before a network is in front of them. D31 point 13's contract suite was built to run the
same cases through both adapters for exactly this reason.

## Scope

**The migration.** `src-tauri/migrations/20261003120100_runner_leases.sql`, exactly as D28
writes it: `runner_leases` with its three indexes and its `CHECK (purpose <> 'strategy' OR
run_id IS NULL)`, then `tasks.lease_generation`, `tasks.pinned_runner_id` with
`idx_tasks_pinned_runner`, `tasks.strategy_requested_at` and `tasks.strategy_requested_by`.
The last two are written by 060 and are only created here. The file's first line is its title
and does not begin with `-- no-transaction` (D28 part 1). No runner migration: `held_leases` is
already in 041's `20261003130100_machine_state.sql`. `Task` gains none of the four columns.
Card presentation is 061's, and `fetch_task_row` names its columns explicitly, so nothing
forces them onto the DTO.

**A conditional run-state write that runs inside someone else's transaction.** This is the
change to task 004's module that `scheduler/claim.rs` names twice ("a `set_run_state` that
takes an expected current state"). Add `tasks::run_state::transition(conn, clock, id, from,
to)`, taking `&mut SqliteConnection`. It checks `is_legal_run_state_transition(from, to)`, runs
`UPDATE tasks SET run_state = ?to, updated_at = ?now WHERE id = ?id AND run_state = ?from`, and
returns whether a row moved. It never commits and never publishes, because its caller does
both after its own commit. `set_run_state` becomes `BEGIN IMMEDIATE`, read, `transition` from
the value it read, commit, publish. Its behaviour, its messages and its exhaustive transition
test do not change. `tasks/run_state.rs` stays the only file that writes `run_state`
(ADR-0006).

**The lease module.** New: `crates/core/src/board/lease.rs`, the code behind
`board::service`'s lease-bearing functions. It is not a new port method, so D31's trait does
not change. It holds:

- **The claim transaction.** `BEGIN IMMEDIATE`. Re-read the task. Check `eligible` (below).
  Take the edges through `transition`:
  - `idle`, `failed` or `cancelled` → `queued` → `running` for a fresh start;
  - `queued` → `running` for a task with no lease row. Once the claim writes both edges in
    one commit, nothing leaves a task `queued` except a build older than 043 (reconciled at
    startup, below) and 057's `release_pin`, which re-queues a task for its next claim;
  - `waiting_retry` → `running` for a resume;
  - no edge for `Plan`, which is D17's planner.

  Then `UPDATE tasks SET lease_generation = lease_generation + 1` and insert the lease with
  that value, the runner, the purpose, `acquired_at` from the clock, and `expires_at` from the
  `LeaseTerm`. Commit, then publish one `ChangeEvent` for the task. Every failure before the
  commit rolls back all of it. A task that already has a lease row, or whose `run_state` no
  longer has the edge, is today's `ClaimOutcome::Lost`, surfaced as the port's `Ok(None)`.

  The purpose:
  - `Plan`: `strategy`;
  - a resume: `LeasePurpose::from(resume.kind)`, so a waiting review is claimed back as a
    `review` (D29 point 3);
  - a fresh `Run` or `Next`: `strategy` when `tasks::strategy::needs_planning(task,
    effective.mode)` holds, otherwise `implementation`. This is ADR-0016's inline planner,
    which `run_task` runs before the implementation, and ADR-0031 point 1 leases a planner
    as `strategy`. `start_run` then moves the purpose to `implementation`, which the `CHECK`
    allows because `run_id` is still NULL. `run_task` runs the inline planner exactly when
    `Claim::purpose` is `strategy` and no longer derives that itself, so the board and the
    runner cannot disagree. From 055 the planner's write-back is authorized by D30 point 5's
    `Strategy` grant, where an `implementation` lease is allowed no tool.
- **The fence.** `current(conn, lease: &LeaseRef, runner_id)` reads the live lease inside the
  caller's transaction:
  - a task that does not exist, or is outside the lease's team, is `NotFound`, which is D31
    point 3's rule and 036's "unknown lease" case;
  - a task that exists with no lease row, or whose lease has another generation or another
    runner, is `Conflict`.

  Every lease-bearing method in `board::service` calls it inside the transaction of the first
  write it guards, never as a separate read before it: `run_context`, `record_branch`,
  `start_run`, `append_transcript`, `finish_run`, `release`, `record_strategy` and
  `record_review_findings`. `finish_run` checks inside the transaction that closes the `runs`
  row. Until 056, it keeps today's "has already been finalized" refusal after that check (D31
  point 12).

  **`publish_tail` is not fenced.** It is synchronous (D31 point 2), and in process it is
  `ServiceContext::publish_tail`, a bare `broadcast::send`, while the fence is an async read.
  D14 makes a tail message worth nothing, so a stale one costs nothing. Its contract case
  stays 036's "the tail is delivered". Over HTTP the server's handler is async, and 052 drops
  a tail there when `current` refuses its lease.
- **The lease's life after the claim.**
  - `start_run` sets the lease's `run_id` and moves its `purpose` to the run's `kind`, under
    the same generation. That enforces D29 point 1's invariant: a lease with a `run_id` has the
    purpose of that run's kind.
  - `finish_run` answering `Released` deletes the lease in the transaction that lands the task.
  - `finish_run` answering `Continue` (021) keeps it, after calling `eligible(conn, task,
    holder, next)` in the same transaction, where `next` is the purpose of the Continue's
    `kind`. When that refuses, the answer is `Released`, the lease is deleted, and the task
    lands as a finish that does not continue would land it (D31 point 4). 067 adds the next
    phase's model to that check and 045 adds consent. Neither adds a second call.
  - `release` deletes the lease in the transaction that takes `running` → `failed`, when the
    task is still `running`, whatever the purpose. A `Plan` claim never took the edge, so its
    release leaves `run_state` alone, which is D31 point 4's rule for a strategy lease. An
    inline planner's release lands `failed`, as the implementation it would have become does.
  - `tasks.lease_generation` is never decremented or reset, so a generation never repeats for a
    task (D31 point 3).
- **`LeaseTerm`.** `LeaseTerm::Never` writes `expires_at` NULL. `LeaseTerm::Renewable(d)`
  writes `now + d`. `pub const LEASE_LIFETIME: Duration` is three minutes (ADR-0031 point 3),
  defined here and nowhere else. 052 and 053 use `LeaseTerm::Renewable(LEASE_LIFETIME)` and
  add no variant. `InProcessBoard::new` takes the term as an argument (D31 point 9), and the
  solo host passes `Never`.
- **`heartbeat`.** One transaction. For each `LeaseRef` in `held`: if it is current for this
  runner, set `expires_at` to `now + d` (a `Never` lease stays NULL), otherwise add it to
  `fenced`. `cancel` stays empty in process (D31 point 4). The heartbeat still has no
  production caller until 053. It is tested through the contract suite.

**The one-transaction claim replaces `scheduler::claim`.** `claim`, `claim_retry` and `release`
leave `scheduler/claim.rs`. `give_up` stays, because it is an operator action on a waiting task
that holds no lease. The header's argument (a conditional write, the interleaving table, why
the scheduler claims all the way to `running`) moves to `board/lease.rs`'s header and is
rewritten in the present tense: one transaction, not two edges. The paragraphs that name the
crash window and the "selected as `idle`, reached `failed`" window are replaced by a sentence
saying `transition`'s expected state closes both. `scheduler::mod`'s re-exports follow. This
transaction is 042's `Next` step 3, and a lost race there still moves on to the next entry
rather than returning.

**Eligibility is one function.** `board::lease::eligible(conn, task, runner, purpose)` is where
the claim decides whether *this* runner may take *this* task. 043 puts one rule in it:
**pinning** (ADR-0031 point 4). A task whose `pinned_runner_id` names another runner is not
this runner's to claim, for every purpose, `strategy` included. 067 adds the model rule and 045
adds consent, here and nowhere else. Selection's rules (ADR-0010's order, dependencies, D21's
per-repository cap) stay where 042 put them, and this function does not restate them.

- **`claim(Next)`.** The runner view 042 passes to `selection::plan` gains the runner's id and
  its `ProviderId` (which 067 reads). `plan` applies `eligible` there, so an ineligible task is
  passed over before queue positions are numbered, and `status_with_plan` and `claim(Next)`
  keep agreeing, as 042's `the_plan_and_the_next_claim_agree_on_what_starts_first` checks. The
  claim transaction checks it again. No `SkipReason` variant is added: a task pinned elsewhere
  is claimable by another runner, so it is not a problem with the card. Rendering "pinned to
  Alice's laptop" is 061's.
- **`claim(Run)` and `claim(Plan)`.** An ineligible task is an `Err(Error::invalid(..))` with a
  sentence (D31 point 4: "a refusal that a person has to read … is an `Err`"). Nothing is
  written.
- **`finish_run`'s `Continue`**, as above.

**Pinning.** Only `board::lease` writes `tasks.pinned_runner_id`. Its writers are 043's
`finish_run`, 053's expiry (which also pins an expired `strategy` lease, which has no
`finish_run`), and 057's `release_pin`, which lives in this module. 043's rule:

- **Set** to the lease holder in `finish_run`'s task-landing transaction, when the run being
  closed has `status = 'interrupted'` or the task lands `waiting_retry`. That covers a
  usage-limit or transient retry, and an interrupted run whether or not its budget allowed a
  resume.
- **Cleared** when a `finish_run` by the pinned runner lands the task anywhere other than
  `waiting_retry` and the run was not interrupted.
- **Unchanged** by a `Continue` (the holder keeps the lease), `give_up`, cancel, a card edit or
  a move. ADR-0031 point 4 says the server never moves a pinned task on its own.

**Recording on the runner, through 041's port.** `MachineStore` gains four `held_leases`
methods: record a lease (task, team, purpose, run, generation, `acquired_at`), set its run and
purpose, forget it by task, and list. `machine_store_contract!` gains a case for each,
including the primary key on `task_id`, so they run against `MemoryMachine` and the runner's
SQLite store. Every starter records through `MachineContext`, after its claim returns and
before anything is spawned:

- the core starter `runner/start.rs`, for Run now and Retry now;
- `claim_for_planning`, for Plan now from the command and from the operator MCP's
  `plan_task_strategy` and `plan_tasks_strategy`, which reach a `MachineContext` through
  041's `LocalTools`;
- the runner loop's `try_step` in `crates/runner/src/queue/`.

`run_task` sets the row's run and purpose after `start_run`, and forgets it on a `Released`
finish or a release. The loop forgets it after each of its early releases. A row left behind
by a missed forget is harmless, because the next reconcile is answered `Conflict` or `NotFound`
for it and drops it. `rimaia-core` still never names `runner.db` or `RunnerStore` (ADR-0027
point 6, 040's structural test).

**Per-runner reconcile replaces the global sweep (ADR-0031 point 5).** Three steps, in core's
`scheduler::reconcile`, so the solo shell and 058's headless binary call the same functions:

1. **`reconcile_held(board: &dyn BoardPort, machine: &MachineContext)`** walks this runner's
   `held_leases` and nothing else:
   - with an open run, it reports `finish_run` with the interrupted outcome. `resume_after`
     is `None`, because the board decides it (D31 point 4), and the close pins. When the
     retry budget is spent, the board's `resume_after` is `None` and `apply_to_task` lands the
     task `failed`, so `settle`'s second hop is never needed for a held lease;
   - with no run, it reports `release`. A `strategy` lease from `Plan` leaves `run_state`
     alone, and an inline planner's lands `failed`;
   - on `Conflict` or `NotFound` (the board already closed that lease), it forgets the local
     row and touches nothing on the board.

   One failure is logged and does not stop the rest, as `reconcile_interrupted` does today.
2. **`reconcile_unrecorded(ctx, runner_id, held: &[String])`.** Solo only. It runs after
   `reconcile_held` and acts through the same services on two sets that only a solo board can
   hold:
   1. **Leases the board records for this runner that `held_leases` does not.** The process
      died between the claim's commit and the record. Two stores cannot share a transaction,
      and a solo lease never expires, so without this arm the task would stay `running`
      forever.
   2. **Tasks in `running` or `queued` with no lease row at all.** A build older than 043 left
      them. They are reconciled exactly as `reconcile_one` and `settle` do today, including
      `queued` → `cancelled`.

   In team mode neither set can exist. The first expires on the server (053). The second cannot
   be written once the claim writes the edges and the lease together. The function's doc says
   so, and says that 065 is where the second set's query can go.
3. **The worktree repair, as 066 hands it off.** `worktree::reconcile` runs after the two
   lease steps, never before. Its `correct_run_state` moves a `running` task to `failed`
   through `set_run_state`. Run before the lease steps, that would leave the lease row behind,
   and every later claim of the task would be `Lost`. After them, no task this runner held is
   still `running`. Its retained-branch write stays `worktree::clear_branch`: most tasks whose
   worktree vanished hold no lease, so there is no lease to write under. 066's comment naming
   043 is replaced by one naming 054's `report_runner`, which takes the write over the port.

**Startup.** `startup::ReconciliationReport` loses `tasks_left_running`, and `survey` keeps
`missing_worktrees` and `missing_run_logs`. In `src-tauri/src/lib.rs`, `reconcile_interrupted`
is replaced by two steps with their own D11 step names, "reconcile the leases this runner held"
and "reconcile runs no lease recorded", followed by the worktree repair, which stays a step
that logs and cannot fail. D15 is unchanged. A reconciled task is offered for resume, not
started, because the queue opens `paused`.

**`Conflict`, the fencing code.** `Error::Conflict { message }`, `Error::conflict(..)` and
`ErrorCode::Conflict`, which serializes as `"conflict"`. `src/types.ts`'s `ErrorCode` union
gains `"conflict"`, with no rendering change: no solo path can produce it. It is the first of
D32 point 3's three variants. `scheduler`'s `lost_the_race` goes with `claim.rs`. Any remaining
code that treats `Invalid | NotFound` as "lost" must not start treating `Conflict` as lost: a
fenced report is not a lost race.

**The contract harness.** D31 point 13's `Harness` gains `async fn start_with(term:
LeaseTerm)`, and `start()` becomes `start_with(LeaseTerm::Never)`. The in-process harness
builds its board in a `TempDir` over `db::connect`'s multi-connection pool, the shape of
`tests/repo_service.rs`'s `file_backed_context`, so the race is a contract case with real
concurrency. An in-memory, single-connection board would serialize the two claims and prove
nothing. 052's HTTP harness already serves a temporary board database and inherits the
requirement.

**The seam contract.** Three additions, each in the file's existing shape:

- **A D8 amendment**, "`Conflict`, the fencing code (task 043)". ADR-0031 point 3 asks for it.
  It names the variant, says it means only "your lease is not the current one", and points at
  D32 point 3 for 046's two variants.
- **A D31 amendment**, "What task 043 decided". It covers `transition`; the claim's edges,
  including `queued` → `running`; the purposes, including the inline planner; the `NotFound`
  versus `Conflict` split; `publish_tail` unfenced in process and dropped by 052's handler;
  `release` keyed on the run state; the `Continue` eligibility call; the pin rule and its three
  writers; `LeaseTerm::Renewable` and `LEASE_LIFETIME`; the harness changes; `held_leases` on
  `MachineStore`; and the solo arm and why team mode needs none. Amendments, not a new `D`
  number: D-numbers are claimed concurrently, and all of this refines D31.
- **The "How to use this" row** for 043, if Phase 0 did not add it: D4 · D8 · D9 · D10 · D11 ·
  D14 · D15 · D17 · D19 · D21 · D23 · D28 · D29 · D30 · D31 · D32 · D33.

**CLAUDE.md.** The "must have tests" list gains **the lease protocol (claim, fencing, pinning,
per-runner reconcile)**, as ADR-0031's consequences require. The commands block does not
change: no crate is added.

## Out of scope

- **The model rule, and who may start a run** (owner, presence, capacity): 067.
- **Expiry, and everything that follows it.** Nothing in 043 closes a lease because time
  passed. The expiry sweep, the restart grace ("extend every lease by one lifetime"), sleep
  recovery, `last_seen_at`, "offline runner" refusals and the one reaction to `Conflict` (stop
  the process, keep the worktree, drop the lease, claim again through the pin, D31 point 11)
  are all 053's. In 043 a `Conflict` from a lease method ends the supervising future as an
  error and is logged. In solo it cannot occur.
- **Releasing a pin.** "Run this elsewhere", the fenced worktree and unpairing are 057's.
- **Consent, assignment and eligibility under ADR-0032** (045).
- **Any HTTP route** and the HTTP adapter (052).
- **Strategy requests** (`strategy_requested_at` and `strategy_requested_by` are written by
  060).
- **UI.** No card shows a holder, a pin or a generation (061). The only frontend change is the
  `ErrorCode` union.
- **Idempotent resends** (056). `finish_run` keeps "already finalized".
- **Changing selection's order, dependency or capacity rules**, the retry policy
  (`retry::decide`), or the attempt fold (D29 point 3).

## Acceptance criteria

**Schema and caches**

- `src-tauri/migrations/20261003120100_runner_leases.sql` matches D28's DDL for task 043
  statement for statement. No other migration is added to either set.
- `no_migration_opts_out_of_its_transaction` still passes and covers the new file.
- Both offline caches are regenerated with D33 point 3's recipe and committed.
  `no_offline_query_cache_at_the_workspace_root` passes.

**The claim**

- `tasks::run_state::transition` exists as described. `set_run_state`'s exhaustive transition
  test is unchanged from 042's tip.
- `a_claim_takes_both_edges_and_the_lease_in_one_transaction`: after `claim(Run)` on an idle
  task, `run_state` is `running`, `lease_generation` is 1, and exactly one `runner_leases` row
  exists: `(task, 'implementation', NULL, <runner>, 1, <clock now>, NULL)`.
- `a_claim_that_cannot_write_its_lease_leaves_the_task_where_it_was`: a file-backed board in a
  `TempDir` has a test-created `BEFORE INSERT ON runner_leases` trigger that raises. The claim
  returns an error. The task is still `idle`, `lease_generation` is 0, no lease exists, and no
  `ChangeEvent` was published. This is the regression test for `claim.rs`'s crash window.
- `two_runners_racing_for_one_task_get_exactly_one_claim`, a contract case over the
  file-backed harness board: runners A and B claim one task concurrently through
  `tokio::join!`, repeated over 50 fresh tasks. Every time, exactly one gets `Some(Claim)` and
  the other gets `Ok(None)`, never an error. One lease row exists, with generation 1.
- `a_lost_race_in_next_moves_on_to_the_next_entry`.
- `a_queued_task_with_no_lease_is_claimed_with_one_edge`.
- `a_strategy_claim_writes_a_lease_and_no_run_state_edge`: the purpose is `strategy`, `run_id`
  is NULL, and `run_state` is unchanged. A release deletes the lease and leaves `run_state`
  unchanged.
- `a_task_that_needs_planning_is_leased_as_strategy_until_start_run`: a fresh claim of a
  `planned` task with no plan has purpose `strategy`, `run_state` `running` and `run_id` NULL.
  `record_strategy` is accepted under it, and `start_run` moves the purpose to
  `implementation` under the same generation. A release before `start_run` lands the task
  `failed` and deletes the lease. A task that needs no planning is claimed as
  `implementation`, and `run_task` spawns no planner for it.
- `start_run_moves_the_lease_to_the_runs_kind_under_one_generation`: implementation →
  review → fix through 021's `Continue` keeps one generation. After each `start_run`,
  `purpose` equals the run's `kind` and `run_id` is that run.
- `a_waiting_review_is_claimed_back_as_a_review`: a review that landed `waiting_retry` is
  resumed with purpose `review` and `Claim::resume.kind == RunKind::Review`.
- `a_released_finish_deletes_the_lease_in_the_transaction_that_lands_the_task`, and
  `continue_keeps_the_lease`.
- `a_continue_that_eligibility_refuses_is_released`: while A holds a lease mid-loop, a testing
  helper pins the task to B. A's finish answers `Released`, the lease is gone, and no next
  phase is started.

**Fencing (D31 point 13's 043 cases, in `crates/core/src/testing/board_contract.rs`, run
through `crates/core/tests/board_port_in_process.rs`)**

- `every_lease_method_refuses_a_stale_generation`: it iterates `BoardMethod::ALL` with an
  exhaustive `match`. Every lease-bearing method called with `generation - 1` returns an error
  whose `code()` is `Conflict` and writes nothing: every row of `tasks`, `runs` and
  `runner_leases` is identical before and after. `preview`, `claim`, `heartbeat` and
  `publish_tail` are matched explicitly as unfenced, so a new variant fails to compile until
  someone decides.
- `a_lease_naming_a_missing_task_is_not_found_and_a_released_lease_is_conflict`.
- `generation_increases_across_a_release_and_a_reclaim`: claim, release, claim gives 2. A
  holder of 1 is `Conflict`.
- `the_heartbeat_renews_current_leases_and_fences_stale_ones_per_lease`: on a harness started
  with `LeaseTerm::Renewable(LEASE_LIFETIME)`, the clock advances two minutes and a heartbeat
  names one current and one stale lease. The current one's `expires_at` is now plus three
  minutes, the stale one is in `fenced`, and the current one was still renewed.
- `a_solo_lease_survives_a_week_of_clock_time`: under `LeaseTerm::Never`, `TestClock` advances
  seven days, then `run_context`, `start_run` and `finish_run` under the original generation
  all succeed, and `expires_at` stayed NULL throughout.
- `Conflict` round-trips `every_error_code_survives_the_crossing` in `mcp/error.rs`, and
  `src/types.ts` carries `"conflict"`.

**Pinning**

- `a_run_that_lands_waiting_retry_pins_the_task_to_its_runner`: driven by the usage-limit
  fixture stream, not a hand-built outcome.
- `an_interrupted_close_pins_the_task_even_when_the_budget_is_spent`.
- `a_pinned_task_is_passed_over_by_another_runners_next_claim_and_claimed_by_its_own`: B's
  `claim(Next)` with free capacity returns `None`, B's plan does not list the task as next,
  and A's claim returns the resume.
- `run_now_and_plan_now_on_another_runner_refuse_a_pinned_task_and_name_the_holder`: B's
  `claim(Run)` and `claim(Plan)` each fail with a message equal to `this task is pinned to
  <label>, which has its worktree and the agent's conversation. Only that runner can run it
  until someone chooses to run it elsewhere.` exactly, with A's label. Nothing is written.
- `a_finish_by_the_pinned_runner_outside_waiting_retry_clears_the_pin`,
  `continue_leaves_the_pin` and `giving_up_leaves_the_pin`.

**Recording**

- `machine_store_contract!`'s four `held_leases` cases pass against `MemoryMachine` and the
  runner's SQLite store.
- `every_starter_records_its_claim_before_it_spawns`: Run now, Retry now, Plan now (the
  command and both MCP tools) and the loop each leave the `held_leases` row their claim
  describes, carry the run id after `start_run`, and leave no row after a `Released` finish.

**Reconcile**

- `a_runner_reconciles_only_the_leases_it_held`: A and B each hold a running task with an open
  run. A's `reconcile_held` closes A's run as `interrupted`, lands A's task by ADR-0011's
  table, pins it to A and empties A's `held_leases`. B's task, run and lease are identical
  before and after.
- `a_held_lease_whose_budget_is_spent_lands_failed_and_pinned`.
- `a_held_lease_the_board_already_closed_is_dropped_without_touching_the_board`.
- `a_strategy_lease_held_at_a_crash_is_released_and_run_state_is_untouched`.
- `a_solo_lease_the_runner_store_never_recorded_is_reconciled_at_startup`: the claim commits,
  nothing is recorded, and the next startup still offers the task for resume.
- `a_task_left_running_by_a_build_without_leases_is_still_offered_for_resume`, and the
  `queued` → `cancelled` case: `reconcile_unrecorded` over a board with no lease rows reaches
  the same end state as 042's `reconcile_interrupted`.
- `a_vanished_worktree_is_repaired_after_the_lease_steps`: a task whose run was interrupted and
  whose worktree is gone ends with its run `interrupted`, no lease, the pin set, its worktree
  record forgotten, and its branch retained or cleared as at 042's tip.
- `ReconciliationReport` has no `tasks_left_running`. `scheduler/claim.rs` has no `claim`,
  `claim_retry` or `release`.

**Everything else**

- Every test in `tests/scheduler.rs` and the runner's loop tests that exists at 042's tip still
  passes, with its end-state assertions unchanged. The only changes allowed are call sites for
  the retired claim entry points, for `reconcile_interrupted` and for `tasks_left_running`:
  - the crash tests
    (`reopening_after_a_crash_shows_one_interrupted_task_and_leaves_the_rest_untouched`,
    `a_task_claimed_before_its_run_row_existed_still_lands_failed`,
    `a_reconciled_task_is_not_picked_up_again_by_the_queue`,
    `a_launch_offers_a_crashed_run_for_resume_and_starts_nothing_until_the_queue_is_started`,
    and the two schedule tests that reconcile a crash) arrange the crash through a real claim
    and a `held_leases` record, and call `reconcile_held`;
  - `a_task_a_crash_caught_still_queued_is_not_stranded` and
    `reconciling_a_task_another_repair_already_settled_still_closes_its_run` arrange lease-less
    rows and call `reconcile_unrecorded`;
  - `a_clean_previous_exit_leaves_the_reconciliation_nothing_to_do` calls both;
  - the claim-section tests whose subject was the two-edge route move to the lease tests, with
    their interleaving assertions kept.
- No test sleeps. Every time-dependent case advances `TestClock`.
- The D8 amendment, the D31 amendment and the CLAUDE.md test-list line exist as described.
- `RIMAIA_DATA_DIR=/tmp/rimaia-043 npm run tauri dev` on a copy of a real `rimaia.db` whose
  last launch was killed mid-run offers that task for resume on the next launch, exactly as
  `main` does. This is checked by hand and listed in the PR body.
- Every CI check passes, run with `SQLX_OFFLINE=true`: `npm run typecheck`, `npm run test`,
  `npm run build`, `cargo test -p rimaia-core`, `cargo test -p rimaia-runner`,
  `cargo fmt --all --check`, `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo clippy -p rimaia-runner --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`.

## Notes

**Read first.** ADR-0031 (all of it). ADR-0011 (resume, and "interrupted"), ADR-0016 (the
planner, and `needs_planning`), ADR-0010 (the claim's single transaction), ADR-0007's run-state
machine, ADR-0006, and ADR-0027 points 5 and 6.

Seam entries:

- **D31**, all of it, especially points 3, 4, 9, 11 and 13;
- **D28** (the `runner_leases` DDL, `tasks.lease_generation`, `held_leases`, and the D4
  amendment under it);
- **D29** points 1 and 3 (`LeasePurpose` against `RunKind`, and `ResumePoint`);
- **D30** point 5 (the grant a lease purpose maps to, which is why the inline planner is leased
  as `strategy`);
- **D32** point 3 (the three error codes);
- **D33** (the recipe);
- D11 (the startup step names), D14 (why the tail is unfenced), D21 (the per-repository cap
  selection keeps), D8, D9 and its amendment, D15, D17 (the planner has no row), D19 (points 4
  and 5), D23 point 7 and D10.

Read D4 and D6 as prohibitions. This task adds no dependency.

**Files to start from (on `main` today):**

- `crates/core/src/scheduler/claim.rs`, `reconcile.rs`, `selection.rs`, `inflight.rs`;
- `crates/core/src/startup.rs`;
- `crates/core/src/tasks/run_state.rs`, `crates/core/src/tasks/strategy.rs`
  (`needs_planning`);
- `crates/core/src/worktree/mod.rs` (`reconcile`, `correct_run_state`);
- `crates/core/src/error.rs`, `crates/core/src/mcp/error.rs`;
- `crates/core/src/runner/process.rs` (the inline planner) and
  `crates/core/src/runner/strategy.rs` (`resolve`, `claim_for_planning`);
- `crates/core/src/mcp/server.rs` (`plan_task_strategy`, `plan_tasks_strategy`);
- `crates/core/src/testing/clock.rs`;
- `crates/core/tests/scheduler.rs` (`two_concurrent_claims_of_one_task_leave_exactly_one_winner`,
  the claim section and the reconcile tests), `crates/core/tests/repo_service.rs`
  (`file_backed_context`);
- `src-tauri/src/lib.rs` (the survey and reconcile steps), `src-tauri/src/commands/runs.rs`,
  `src-tauri/src/commands/strategy.rs`;
- `src/types.ts`.

**Files earlier tasks create, which this task edits:**

- `crates/core/src/board/{port,types,service,in_process}.rs`, `runner/start.rs`,
  `crates/core/src/testing/board_contract.rs` and `crates/core/tests/board_port_in_process.rs`
  (036);
- `runners`, `solo_identity` and the context's actor (038);
- `crates/runner/` and `RunnerStore` (040);
- `crates/core/src/machine/`, `testing::machine::MemoryMachine`,
  `crates/core/src/testing/machine_contract.rs`, `LocalTools` and `held_leases` (041);
- worktree records on the runner and `worktree::clear_branch` (066);
- the board-side `claim(Next)`, `selection::plan`'s runner view, the runner loop and
  `LocalSlot` (042).

**What the chain provides.**

- 036 gives the port with generation `0` and today's claim routes behind it.
- 038 gives the solo runner's id, both runners the contract harness uses, and the caller actor
  on `ServiceContext`.
- 040 and 041 give `runner.db` with `held_leases` in it, and `MachineContext`, which `run_task`
  and the loop already take.
- 042 gives `claim(Next)` board-side and moves spawning into the runner loop.
- 021 (landed before 038) gives `NextStep::Continue` and review and fix phases under one lease.

**If any of these is not where D31, 041 and 066 say it is, stop and ask.** Do not add a second
claim path beside the port. In particular:

- if 036's "unknown lease is `NotFound`" case means something other than "a task that does
  not exist", ask before changing the fence's split;
- if a starter cannot reach a `MachineContext`, ask. Do not record from the shell.

**What the next tasks expect.**

- 044 expects the lease to fence the `finish_run` that records `head_sha`.
- 067 adds the model rule to `eligible`, including on `Continue`, and `authorize_start`.
- 045 adds assignment and consent to `eligible`, including on `Continue`.
- 052 serves every lease method over HTTP, runs this task's contract cases through `HttpBoard`
  over a multi-connection temporary board, and drops a tail whose lease `current` refuses.
- 053 adds the expiry sweep through the same landing that pins, plus the restart grace, the one
  reaction to `Conflict`, and the heartbeat's production caller. It uses
  `LeaseTerm::Renewable(LEASE_LIFETIME)` from `board/lease.rs`, and adds neither a variant nor
  a second constant.
- 057 puts `release_pin` in `board/lease.rs`. The claim's `queued` → `running` edge is the one
  its re-queued task takes. Whether selection offers a `queued` task is 057's to decide.
- 060's strategy requests arrive through `Next` with purpose `strategy` and no edge. An inline
  planner's claim also has purpose `strategy`, with the task `running`. 060 keeps the two apart
  and amends D31 to say how.
- 061 renders holder and pin.

**Two things that look optional and are not.**

- **The fence's read must share the write's transaction.** A check followed by a write in a
  second transaction is the double-claim bug `set_run_state`'s doc measured (598 of 600
  losers on a ten-connection pool), moved into the fence.
- **`BEGIN IMMEDIATE` on every lease transaction,** for the reason that same doc gives.

**Size.** L: roughly 1,000 lines of production code (the lease module, the claim rewrite, the
recording, the reconcile arms), 1,600 of tests, the migration, and both caches. The model rule
and `authorize_start` were cut to 067 before launch to keep this inside one session. Do not
pull them back in.
