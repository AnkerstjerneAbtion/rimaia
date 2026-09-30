---
id: "043"
title: Runner leases, fencing and pinning
milestone: v0.5
status: ready
depends_on: ["042"]
adrs: ["0031", "0011", "0016"]
size: L
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
(`LeaseTerm::Never`), it is the only runner, so a pin can never exclude anyone, and Claude is
the only production provider, so the model rule can never skip anything. What changes is that
the crash window `scheduler/claim.rs` documents is closed, and the protocol that tasks 052 and
053 put on the network already holds, with tests, in process.

## Why now

Task 042 split selection from the runner loop, so the claim is already a board-side call
through `BoardPort::claim`. It still walks `scheduler::claim`'s two separately committed
`set_run_state` edges and returns generation `0` (D31 point 9: "before 043 there is no lease
row"). Every later team-mode task depends on the lease being real:

- 044 branches from a commit that a lease-fenced `finish_run` recorded;
- 045 adds consent to the claim's eligibility check, which this task makes a single function;
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

- **The claim transaction.** `BEGIN IMMEDIATE`. Re-read the task. Check eligibility (below).
  Take the edges through `transition`:
  - `idle`, `failed` or `cancelled` → `queued` → `running` for a fresh start;
  - `waiting_retry` → `running` for a resume;
  - no edge for purpose `strategy`, which is D17's planner, D31's `Plan`.

  Then `UPDATE tasks SET lease_generation = lease_generation + 1` and insert the lease with
  that value, the runner, the purpose, `acquired_at` from the clock, and `expires_at` from the
  `LeaseTerm`. Commit, then publish one `ChangeEvent` for the task. Every failure before the
  commit rolls back all of it. A task that already has a lease row, or whose `run_state` no
  longer has the edge, is today's `ClaimOutcome::Lost`, surfaced as the port's `Ok(None)`.
  The purpose of a fresh claim is `implementation`, or `strategy` for `Plan`. The purpose of a
  resume is `LeasePurpose::from(resume.kind)`, so a waiting review is claimed back as a
  `review` (D29 point 3).
- **The fence.** `current(conn, lease: &LeaseRef, runner_id)` reads the live lease inside the
  caller's transaction:
  - a task that does not exist, or is outside the lease's team, is `NotFound`, which is D31
    point 3's rule and 036's "unknown lease" case;
  - a task that exists with no lease row, or whose lease has another generation or another
    runner, is `Conflict`.

  Every lease-bearing method in `board::service` calls it inside the transaction of the first
  write it guards, never as a separate read before it: `run_context`, `record_branch`,
  `start_run`, `append_transcript`, `finish_run`, `release`, `record_strategy` and
  `record_review_findings`. `publish_tail` cannot fail (D31 point 4), so a tail under a stale
  generation is dropped and logged at `debug`. `finish_run` checks inside the transaction that
  closes the `runs` row. Until 056, it keeps today's "has already been finalized" refusal after
  that check (D31 point 12).
- **The lease's life after the claim.**
  - `start_run` sets the lease's `run_id` and moves its `purpose` to the run's `kind`, under
    the same generation. That enforces D29 point 1's invariant: a lease with a `run_id` has the
    purpose of that run's kind.
  - `finish_run` answering `Released` deletes the lease in the transaction that lands the task.
    `Continue` (021) keeps it.
  - `release` deletes it in the transaction that takes `running` → `failed`. For a `strategy`
    lease it deletes it and leaves `run_state` alone (D31 point 4).
  - `tasks.lease_generation` is never decremented or reset, so a generation never repeats for a
    task (D31 point 3).
- **`LeaseTerm`.** `LeaseTerm::Never` writes `expires_at` NULL. `LeaseTerm::Renewable(d)`
  writes `now + d`. `pub const LEASE_LIFETIME: Duration` is three minutes (ADR-0031 point 3).
  053 is its first production user. `InProcessBoard::new` takes the term as an argument (D31
  point 9), and the solo host passes `Never`.
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
saying `transition`'s expected state closes both. `scheduler::mod`'s re-exports follow.

**Eligibility is one function.** `board::lease::eligible(conn, task, runner, purpose)` is where
the claim decides whether *this* runner may take *this* task. 045 adds consent to this function
and nowhere else. 043 puts three rules in it:

1. **Pinning (ADR-0031 point 4).** A task with `pinned_runner_id` set to another runner is not
   this runner's to claim.
2. **The model rule (ADR-0031 point 1, ADR-0016).** Defined under "A decision this task makes"
   below.
3. **Selection's existing rules** (ADR-0010's order, dependencies, D21's per-repository cap)
   stay where 042 put them. This function does not restate them.

For `claim(Next)`, an ineligible task is passed over for that runner before queue positions
are numbered. No `SkipReason` variant is added: a task pinned elsewhere, or with a model this
provider cannot run, is claimable by another runner, so it is not a problem with the card.
Rendering "pinned to Alice's laptop" is 061's. For `claim(Run)` and `claim(Plan)`, an
ineligible task is an `Err(Error::invalid(..))` with a sentence (D31 point 4: "a refusal that a
person has to read … is an `Err`"). Nothing is written.

**Pinning.** `board::lease::pin(conn, task_id, runner_id)` and `unpin(conn, task_id)`, called
only inside `finish_run`'s task-landing transaction:

- **Set** when the run being closed has `status = 'interrupted'`, or when the task lands
  `waiting_retry`. Either way it is set to the lease holder. That covers a usage-limit or
  transient retry, and an interrupted run whether or not its budget allowed a resume.
- **Cleared** when a `finish_run` by the pinned runner lands the task anywhere other than
  `waiting_retry` and the run was not interrupted.
- **Left alone** by everything else: `give_up`, cancel, a card edit or a move. ADR-0031 point 4
  says the server never moves a pinned task on its own. The only releases are "run elsewhere"
  and unpairing, both 057's.

053's expiry sweep closes a run as interrupted through this same `finish_run` path, so it pins
without code of its own.

**Run now, Retry now and Plan now name a runner (ADR-0031 point 7).**
`board::service::authorize_start(ctx, runner_id, presence) -> Result<RunTrigger>`. It is one
function, called by the `rimaia-core` starter that D31 point 5 created (and 042 kept) before
it claims, and by 052's route later.

- `ctx` is the caller's context (038's actor), never the in-process adapter's `System` one.
- The runner must exist in the caller's team, or `NotFound`, and must not be unpaired, or
  `Invalid`.
- **Owner only.** The caller must be `runners.user_id`, or `Invalid` with the sentence below.
  A teammate makes a task claimable by assigning it. Starting a process on someone else's
  machine is not a board action.
- **Presence decides the posture.** `Presence::AtRunner` returns `RunTrigger::Manual`
  (ADR-0012 point 6, `acceptEdits`). `Presence::Remote` returns `RunTrigger::Queued`, which
  runs as an unattended run and is therefore subject to ADR-0012's per-repository opt-in, and
  from 045 to consent, exactly as a queued run is.

  Presence is decided by the door, never by a request field. In 043 every door is a local
  command or the loopback operator MCP server on the runner's own machine, so every caller
  passes `AtRunner`. 052's browser route passes `Remote`.
- **Capacity does not apply (D19 point 5).** `claim(Run)` takes no `FreeCapacity`, and the
  board applies none. The runner's `LocalSlot::acquire_unbounded` (042's rename of D19's
  `Lease`) is unchanged.
- **Plan now** calls the same function for the owner rule. The planner's own posture is
  unchanged.

**Per-runner reconcile replaces the global sweep (ADR-0031 point 5).**

- **Recording.** Every starter records its claim in `runner.db`'s `held_leases` (task,
  team, purpose, run, generation, `acquired_at`) through one `rimaia-runner` function,
  `held::record`, after the claim returns and before anything is spawned. `start_run` updates
  the row's `run_id` and `purpose`. A `Released` finish, or a release, deletes it. If a starter
  still lives in `rimaia-core` after 042, its host in `src-tauri` does the recording.
  `rimaia-core` never learns about `runner.db` (ADR-0027 point 6, 040's structural test).
- **`rimaia_runner::reconcile::reconcile_held(board: &dyn BoardPort, store: &RunnerStore)`**
  walks `held_leases` and nothing else:
  - with an open run, it reports `finish_run` with the interrupted outcome (`resume_after`
    `None`; the board decides it, D31 point 4, and pins);
  - with no run, or with purpose `strategy`, it reports `release`;
  - on `Conflict` or `NotFound` (the board already closed that lease), it deletes the local
    row and touches nothing on the board.

  One failure is logged and does not stop the rest, as `reconcile_interrupted` does today.
- **The solo arm.** `scheduler::reconcile::reconcile_unrecorded(ctx, runner_id, held:
  &[String])`. It is solo only, runs after `reconcile_held`, and acts through the same
  services on two sets that only a solo board can hold:
  1. **Leases the board records for this runner that `held_leases` does not.** The process
     died between the claim's commit and `held::record`. Two stores cannot share a
     transaction, and a solo lease never expires, so without this arm the task would stay
     `running` forever.
  2. **Tasks in `running` or `queued` with no lease row at all.** A build older than 043 left
     them. They are reconciled exactly as `reconcile_one` and `settle` do today, including
     `queued` → `cancelled`.

  In team mode neither set can exist. The first expires on the server (053). The second cannot
  be written once the claim writes the edges and the lease together. The function's doc says
  so, and says that 065 is where the second set's query can go.
- **Startup.** `startup::ReconciliationReport` loses `tasks_left_running`, and `survey` keeps
  `missing_worktrees` and `missing_run_logs`. In `src-tauri/src/lib.rs`, `reconcile_interrupted`
  is replaced by two steps with their own D11 step names: "reconcile the leases this runner
  held" and "reconcile runs no lease recorded". D15 is unchanged. A reconciled task is offered
  for resume, not started, because the queue opens `paused`.

**`Conflict`, the fencing code.** `Error::Conflict { message }`, `Error::conflict(..)` and
`ErrorCode::Conflict`, which serializes as `"conflict"`. `src/types.ts`'s `ErrorCode` union
gains `"conflict"`, with no rendering change: no solo path can produce it. It is the first of
D32 point 3's three variants. `scheduler`'s `lost_the_race` goes with `claim.rs`. Any remaining
code that treats `Invalid | NotFound` as "lost" must not start treating `Conflict` as lost: a
fenced report is not a lost race.

**A decision this task makes, and records: what "a model the runner's provider cannot run"
means.** ADR-0031 point 1 has the claim skip such a task and does not define the phrase. The
obvious reading ("the model is not in the catalogue resolved for this runner's provider") is a
behaviour change in solo. `tasks.model` is free text, `update_task` and the MCP tools accept
any string, and Claude runs a full model id (`claude-…`) that no catalogue lists. Under that
reading a queue that runs such a card tonight would skip it tomorrow. So:

> A model `m` cannot run on a runner whose provider is `P` when `m` is **not** an id in the
> catalogue the board resolves for `P`, **and** `m` **is** an id in the default catalogue of
> some other provider this build knows.

That skips what ADR-0031 is protecting against, a card set to one provider's model reaching
another provider's runner. It leaves a model no provider claims going to the CLI as it does
today, and on a Claude-only build it can never fire. It needs `ProviderId::ALL` and
`ProviderId::default_catalogue(self)`, which delegates to each provider's own
`default_catalogue`. Ledger's arm is `#[cfg(feature = "testing")]` like the variant. The
`AgentProvider` trait does not change. The rule is a pure function,
`strategy::catalogue::runs_on(model, provider, resolved: &Catalogue) -> bool`, called from
`eligible`. It checks the model the claimed phase would spawn with:

- `implementation` and `fix`: `EffectiveStrategy::model`;
- `review`: the review model from 021's `RunContext::review`, if it names one;
- `strategy`: exempt, because the planner budget comes from the runner's own provider's
  catalogue.

The claiming runner's provider is the adapter's `provider` in process (D31 point 9), and the
`ProviderId` the runner sends over HTTP (D31 point 10, 052).

**The seam contract.** Three additions, each in the file's existing shape:

- **A D8 amendment**, "`Conflict`, the fencing code (task 043)". ADR-0031 point 3 asks for it.
  It names the variant, says it means only "your lease is not the current one", and points at
  D32 point 3 for 046's two variants.
- **A D31 amendment**, "What task 043 decided". It covers `transition`; the `NotFound` versus
  `Conflict` split in the fence; `publish_tail` dropping silently; the pin rule; the model
  rule above; `authorize_start` and `Presence`; `LeaseTerm::Renewable` and `LEASE_LIFETIME`;
  the solo arm and why team mode needs none; and the eligibility function 045 extends.
  Amendments, not a new `D` number: D-numbers are claimed concurrently, and all of this refines
  D31.
- **The "How to use this" row** for 043, if Phase 0 did not add it: D4 · D8 · D9 · D10 · D15 ·
  D17 · D19 · D23 · D28 · D29 · D31 · D32 · D33.

**CLAUDE.md.** The "must have tests" list gains **the lease protocol (claim, fencing, pinning,
per-runner reconcile)**, as ADR-0031's consequences require. The commands block does not
change: no crate is added.

## Out of scope

- **Expiry, and everything that follows it.** Nothing in 043 closes a lease because time
  passed. The expiry sweep, the restart grace ("extend every lease by one lifetime"), sleep
  recovery, `last_seen_at`, "offline runner" refusals and the one reaction to `Conflict` (stop
  the process, keep the worktree, drop the lease, claim again through the pin, D31 point 11)
  are all 053's. In 043 a `Conflict` from a lease method ends the supervising future as an
  error and is logged. In solo it cannot occur.
- **Releasing a pin.** "Run this elsewhere", the fenced worktree and unpairing are 057's.
- **Consent, assignment and eligibility under ADR-0032** (045). 045 extends `eligible`.
- **Any HTTP route**, the HTTP adapter, and the browser's Run now (052). `Presence::Remote` has
  no production caller in 043.
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
  test is byte-identical to `main`.
- A structural test in `crates/core/tests/harness.rs` fails if any file under
  `crates/core/src/` other than `tasks/run_state.rs` and `board/lease.rs` names
  `RunState::Running` as the target of a run-state write. The claim is then the only way into
  `running`.
- `a_claim_takes_both_edges_and_the_lease_in_one_transaction`: after `claim(Run)` on an idle
  task, `run_state` is `running`, `lease_generation` is 1, and exactly one `runner_leases` row
  exists: `(task, 'implementation', NULL, <runner>, 1, <clock now>, NULL)`.
- `a_claim_that_cannot_write_its_lease_leaves_the_task_where_it_was`: a file-backed board in a
  `TempDir` has a test-created `BEFORE INSERT ON runner_leases` trigger that raises. The claim
  returns an error. The task is still `idle`, `lease_generation` is 0, no lease exists, and no
  `ChangeEvent` was published. This is the regression test for `claim.rs`'s crash window.
- `two_runners_racing_for_one_task_get_exactly_one_claim`: a multi-connection file-backed pool
  in a `TempDir` (the shape of `tests/repo_service.rs`'s `file_backed_context`). Runners A and
  B claim one task concurrently through `tokio::join!`, repeated over 50 fresh tasks. Every
  time, exactly one gets `Some(Claim)` and the other gets `Ok(None)`, never an error. One lease
  row exists, with generation 1.
- `a_strategy_claim_writes_a_lease_and_no_run_state_edge`: the purpose is `strategy`, `run_id`
  is NULL, and `run_state` is unchanged. A release deletes the lease and leaves `run_state`
  unchanged.
- `start_run_moves_the_lease_to_the_runs_kind_under_one_generation`: implementation →
  review → fix through 021's `Continue` keeps one generation. After each `start_run`,
  `purpose` equals the run's `kind` and `run_id` is that run.
- `a_waiting_review_is_claimed_back_as_a_review`: a review that landed `waiting_retry` is
  resumed with purpose `review` and `Claim::resume.kind == RunKind::Review`.
- `a_released_finish_deletes_the_lease_in_the_transaction_that_lands_the_task`, and
  `continue_keeps_the_lease`.

**Fencing (D31 point 13's 043 cases, in `crates/core/src/testing/board_contract.rs`, run
through `crates/core/tests/board_port_in_process.rs`)**

- `every_lease_method_refuses_a_stale_generation`: it iterates `BoardMethod::ALL` with an
  exhaustive `match`. Every lease-bearing method called with `generation - 1` returns an error
  whose `code()` is `Conflict` and writes nothing: the row counts and `updated_at` of `tasks`,
  `runs` and `runner_leases` are unchanged. `publish_tail` under a stale generation delivers
  nothing to a subscriber. `preview`, `claim` and `heartbeat` are matched explicitly as taking
  no lease, so a new variant fails to compile until someone decides.
- `a_lease_naming_a_missing_task_is_not_found_and_a_released_lease_is_conflict`.
- `generation_increases_across_a_release_and_a_reclaim`: claim, release, claim gives 2. A
  holder of 1 is `Conflict`.
- `the_heartbeat_renews_current_leases_and_fences_stale_ones_per_lease`: under
  `LeaseTerm::Renewable(LEASE_LIFETIME)`, the clock advances two minutes and a heartbeat names
  one current and one stale lease. The current one's `expires_at` is now plus three minutes,
  the stale one is in `fenced`, and the current one was still renewed.
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
  `claim(Next)` with free capacity returns `None`, and A's returns the resume.
- `run_now_on_another_runner_refuses_a_pinned_task_and_names_the_holder`: the message equals
  `this task is pinned to <label>, which has its worktree and the agent's conversation. Only
  that runner can run it until someone chooses to run it elsewhere.` exactly, with A's label.
  Nothing is written.
- `a_finish_by_the_pinned_runner_outside_waiting_retry_clears_the_pin`, and
  `giving_up_leaves_the_pin`.

**The model rule**

- `strategy::catalogue::runs_on` has unit tests for four cases: a model in the runner's
  catalogue runs; another provider's default model does not; a model no provider lists runs;
  an edited catalogue that lists another provider's model runs.
- `a_runner_is_not_offered_a_task_whose_model_belongs_to_another_provider`: runner B uses the
  Ledger provider, and a task is set to `opus`. B's `claim(Next)` passes it over and A's takes
  it. B's `claim(Run)` refuses with `this task asks for the model "opus", which Ledger cannot
  run. Change the task's model, or run it on a runner whose provider offers it.`, using the
  provider's `display_name`.
- `a_model_no_provider_lists_still_reaches_the_cli`: a task set to `claude-opus-4-5` is claimed
  and its argv carries `--model claude-opus-4-5`, as on `main`.
- A planner claim is not subject to the rule.

**Run now**

- `only_a_runners_owner_can_start_a_run_on_it`: the solo user asks to start a run on a second
  runner owned by a second user. The answer is `Invalid` with `only this runner's owner can
  start a run on it. Assign the task and leave it ready, and their runner will pick it up.`
  Nothing is written. Plan now is refused the same way.
- `an_unpaired_runner_is_refused`.
- `run_now_is_not_bound_by_capacity`: with `FreeCapacity { total: 0, .. }`, `claim(Next)`
  returns `None` and `claim(Run)` for the same runner and task claims.
- `presence_decides_the_permission_posture`: `AtRunner` gives `RunTrigger::Manual`
  (`acceptEdits`). `Remote` gives `RunTrigger::Queued` (`bypassPermissions`), and on a
  repository without the unattended opt-in the claim is refused with the existing opt-in
  sentence before anything is written.
- `start_task_run`, `retry_task_now` and `plan_task_strategy` (the command and the operator MCP
  tool) all reach `authorize_start` with `Presence::AtRunner`.

**Reconcile**

- `a_runner_reconciles_only_the_leases_it_held`: A and B each hold a running task with an open
  run. A's `reconcile_held` closes A's run as `interrupted`, lands A's task by ADR-0011's
  table, pins it to A and empties A's `held_leases`. B's task, run and lease are byte-identical
  before and after.
- `a_held_lease_the_board_already_closed_is_dropped_without_touching_the_board`.
- `a_strategy_lease_held_at_a_crash_is_released_and_run_state_is_untouched`.
- `a_solo_lease_the_runner_store_never_recorded_is_reconciled_at_startup`: the claim commits,
  there is no `held::record`, and the next startup still offers the task for resume.
- `a_task_left_running_by_a_build_without_leases_is_still_offered_for_resume`, and the
  `queued` → `cancelled` case: `reconcile_unrecorded` over a board with no lease rows reaches
  the same end state as `main`'s `reconcile_interrupted`.
- `a_launch_offers_a_crashed_run_for_resume_and_starts_nothing_until_the_queue_is_started`
  still passes, and its assertions are unchanged.
- `ReconciliationReport` has no `tasks_left_running`. `scheduler/claim.rs` has no `claim`,
  `claim_retry` or `release`.

**Everything else**

- Every test in `tests/scheduler.rs` and the runner's loop tests that exists when this task
  starts still passes. The only changes allowed are call sites for the retired claim entry
  points. The claim-section tests whose subject was the two-edge route move to the lease
  tests, with their interleaving assertions kept.
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
effective strategy's precedence chain), ADR-0010 (the claim's single transaction), ADR-0012
point 6, ADR-0007's run-state machine, ADR-0006, and ADR-0027 points 5 and 6.

Seam entries:

- **D31**, all of it, especially points 3, 4, 9, 11 and 13;
- **D28** (the `runner_leases` DDL, `tasks.lease_generation`, `held_leases`, and the D4
  amendment under it);
- **D29** points 1 and 3 (`LeasePurpose` against `RunKind`, and `ResumePoint`);
- **D32** point 3 (the three error codes);
- **D33** (the recipe);
- D8, D9 and its amendment, D15, D17 (the planner has no row), D19 (points 4 and 5), D23
  point 7 and D10.

Read D4 and D6 as prohibitions. This task adds no dependency.

**Files to start from (on `main` today):**

- `crates/core/src/scheduler/claim.rs`, `reconcile.rs`, `selection.rs`, `inflight.rs`;
- `crates/core/src/startup.rs`;
- `crates/core/src/tasks/run_state.rs`;
- `crates/core/src/error.rs`, `crates/core/src/mcp/error.rs`;
- `crates/core/src/strategy/catalogue.rs`, `crates/core/src/strategy/resolve.rs`;
- `crates/core/src/runner/provider/mod.rs` (`ProviderId`), `crates/core/src/testing/provider.rs`
  (Ledger's catalogue);
- `crates/core/src/runner/strategy.rs` (`claim_for_planning`);
- `crates/core/src/testing/clock.rs`;
- `crates/core/tests/scheduler.rs` (`two_concurrent_claims_of_one_task_leave_exactly_one_winner`
  and the claim section), `crates/core/tests/repo_service.rs` (`file_backed_context`),
  `crates/core/tests/harness.rs` (the structural tests' style);
- `src-tauri/src/lib.rs` (the survey and reconcile steps), `src-tauri/src/commands/runs.rs`,
  `src-tauri/src/commands/strategy.rs`;
- `src/types.ts`.

**Files earlier tasks create, which this task edits:**

- `crates/core/src/board/{port,types,service,in_process}.rs`,
  `crates/core/src/testing/board_contract.rs` and `crates/core/tests/board_port_in_process.rs`
  (036);
- `runners`, `solo_identity` and the context's actor (038);
- `crates/runner/` and `RunnerStore` (040);
- `held_leases` and the store on `AppState` (041);
- the board-side `claim(Next)`, the runner loop and `LocalSlot` (042).

**What the chain provides.**

- 036 gives the port with generation `0` and today's claim routes behind it.
- 038 gives the solo runner's id, both runners the contract harness uses, and the caller actor
  on `ServiceContext`.
- 040 and 041 give `runner.db` with `held_leases` in it.
- 042 gives `claim(Next)` board-side and moves spawning into the runner loop.
- 021 (landed before 038) gives `NextStep::Continue` and review and fix phases under one lease.

**If any of these is not where D31 says it is, stop and ask.** Do not add a second claim path
beside the port. In particular:

- if 036's "unknown lease is `NotFound`" case means something other than "a task that does
  not exist", ask before changing the fence's split;
- if 042 left a starter in `rimaia-core`, do its `held::record` in `src-tauri`, never in core.

**What the next tasks expect.**

- 044 expects the lease to fence the `finish_run` that records `head_sha`.
- 045 extends `board::lease::eligible` with assignment and consent, and re-checks consent on
  `Continue`.
- 052 serves every lease method over HTTP, calls `authorize_start` with `Presence::Remote`,
  runs this task's contract cases through `HttpBoard`, and sends the `ProviderId` the model
  rule reads.
- 053 adds the expiry sweep through the same `finish_run` path that pins, plus the restart
  grace, the one reaction to `Conflict`, and the heartbeat's production caller, using
  `LeaseTerm::Renewable(LEASE_LIFETIME)`.
- 057 clears `pinned_runner_id` and fences the old worktree.
- 061 renders holder and pin.

**Two things that look optional and are not.**

- **The fence's read must share the write's transaction.** A check followed by a write in a
  second transaction is the double-claim bug `set_run_state`'s doc measured (598 of 600
  losers on a ten-connection pool), moved into the fence.
- **`BEGIN IMMEDIATE` on every lease transaction,** for the reason that same doc gives.

**Size, and where to cut.** L, and the upper edge of one session: roughly 1,200 lines of
production code (the lease module, the claim rewrite, the reconcile arms, the model rule,
`authorize_start`), 1,800 of tests, the migration, and both caches. If the diff passes about
3,500 lines excluding `.sqlx/`, stop and propose a split rather than trimming tests. The clean
cut is to keep the migration, the one-transaction claim, fencing, `Conflict`, `LeaseTerm`,
pinning and the per-runner reconcile here, and move the model rule and `authorize_start`
(owner, presence, capacity) to a follow-up task with the next free id, ordered before 045.
Neither of those is on the critical path of 044, and neither can fire in solo.
