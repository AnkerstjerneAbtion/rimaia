---
id: "036"
title: A board port between the runner and the board
milestone: v0.4
status: ready
depends_on: ["035"]
adrs: ["0027", "0031", "0015", "0011", "0006"]
size: L
landed: "#34"
---

# A board port between the runner and the board

## Goal

Draw the line [ADR-0027](../docs/adr/0027-a-server-for-the-board-and-runners-for-the-work.md)
point 5 decides, while there is still only one process on each side of it: a `BoardPort`
trait that runner code uses for everything it says to the board, an in-process adapter
that solo uses, and the skeleton of the contract suite that task 052's HTTP adapter will
also have to pass. After this task, `runner::process::run_task` and `runner/strategy.rs`
reach the board only through the port, and `runner/outcome.rs` is board-side code that
`board::service` calls. Nothing about the board, the queue or a run behaves differently,
apart from the two changes named under Scope.

**This task is a refactor.** It adds no migration, no command, no MCP tool, no lease row
and no second adapter. The signature is seam-contract D31's, and this task implements it
without redesigning it.

## Why now

The next task is 021, the review-and-fix loop. It is the first task that has to write a
run's result back to the board from something other than an implementation run: review
findings, a fix phase that follows a review, and a loop budget the board has to enforce.
If 021 is written against `ServiceContext`, the way `run_task` is today, the loop's rules
end up in the runner, and task 052 later has to move them across a network boundary that
did not exist when they were written. If 021 is written against the port, its rules land
board-side in `board::service` from the start, and D31's `NextStep::Continue` is a variant
021 adds rather than a redesign.

The refactor is also cheapest now. There is one adapter, one process and one database, so
every behaviour this task has to preserve can be checked by the existing suite. Each task
from 038 to 045 adds a field or a check to a port that already exists. Without the port,
each of them would add one more direct `ctx.pool` read to `run_task` for 052 to find.

## Scope

Read D31 in full before starting. Everything below refines it; nothing contradicts it.

**The module.** New files in `crates/core/src/board/`:
- `mod.rs`;
- `port.rs`: `BoardFuture` and `BoardPort`;
- `types.rs`: the DTOs;
- `service.rs`: one function per method, and the only code either adapter calls;
- `in_process.rs`: `InProcessBoard { ctx, paths, provider }`.

The trait uses boxed futures, is object-safe, and is held as `Arc<dyn BoardPort>`, as D31
point 2 requires. No `async-trait` dependency (D6, D34).

**What 036 ships of D31's signature, and what it leaves to the task that owns it.** D31
point 6 names who adds each later field. Adding a field or a variant later is not a trait
signature change, so none of these is a placeholder in this diff:

| Item | In 036 | Added by |
| --- | --- | --- |
| `preview`, `claim`, `heartbeat`, `run_context`, `record_branch`, `start_run`, `append_transcript`, `publish_tail`, `finish_run`, `release`, `record_strategy`, `record_review_findings` | yes, each with an in-process body | — |
| `run_tool`, `RunToolCall`, `BoardMethod::RunTool` | no | 055 (D31 point 7) |
| `LeaseRef::team_id` | no. `LeaseRef { task_id, generation }` | 038 |
| `LeaseRef::generation` | yes, always `0` | 043 makes it real |
| `ClaimTarget::Run`, `ClaimTarget::Plan` | yes | — |
| `LeasePurpose` (D28's `CHECK` spelling), `TeamLimits`, `Heartbeat`, `TranscriptChunk`, `TranscriptAck`, `FinishReceipt` | yes, with D31 point 2's fields | — |
| `ClaimTarget::Next`, `FreeCapacity` | no. A variant whose only body is a refusal cannot be told apart from a bug. D31 point 7 says otherwise, and the amendment below corrects it | 042 |
| `NextStep::Released` | yes | — |
| `NextStep::Continue`, `RunContext::review` | no | 021 |
| `RunContext::base` | no | 044 |
| `StartRun::base_sha`, `FinishRun::head_sha`, `FinishRun::bundle` | yes. 033 landed them | — |
| `StartRun::kind`, `Claim::resume: Option<ResumePoint>` | yes. 035 landed `RunKind` and `ResumePoint` | — |
| `TranscriptEnd::{Complete, KeptOnRunner}` | yes. In process both are acknowledged the same way | 056 gives `KeptOnRunner` its meaning |

`BoardMethod` has one variant per method this task ships, `BoardMethod::ALL`, and
`as_str()`, which returns the trait method's name. Every type in `types.rs` derives
`Serialize` and `Deserialize` with `#[serde(rename_all = "camelCase")]`, and none uses
`deny_unknown_fields`. Where a core type carried inside a DTO (`TaskDetail`, `Repository`,
`Catalogue`, `RunOutcome`, `Run`, `RunTail`, `StrategyPlan`, 033's bundle, 035's
`NewReviewFinding`) lacks `Deserialize`, it gains the derive. Its `Serialize` output does
not change, because `src/types.ts` reads it. One carried type breaks D31 point 6's rule:
`Catalogue`, `CatalogueEntry` and `PlannerBudget` keep `deny_unknown_fields`, which is what
turns a misspelled key in the stored setting into a warning (`strategy/catalogue.rs`). Over
HTTP it would make a newer board's catalogue unreadable to an older runner. 052 decides it,
with a wire mirror or by relaxing the attribute. This task leaves it alone and names it in
the D31 amendment.

**The in-process adapter** follows D31 point 9:
- `ctx` is re-sourced to `MutationSource::System` at construction.
- `provider` is read only to build `RunContext::catalogue`. It must be the same provider as
  the `RunnerConfig` it serves (see Traps).
- Every method is one call into `board::service`.
- `claim` returns generation `0` and runs today's routes, **reads first, edges last**. It
  builds the `RunContext` and, for `Run { continue_session: true }`, reads 035's
  `attempts::resume_point` and applies `resume_as_implementation`. Only then does it take
  the edges: `scheduler::claim::claim` for `continue_session: false`, `claim_retry` for
  `true`, none for `Plan`. Reading the point before the edge is safe for 035's reason:
  every caller already holds D19's slot. So a failed read, including 035's refusal of a
  review or fix point, writes nothing, and a waiting task stays `waiting_retry`, as 035's
  `a_waiting_review_resumes_as_a_review_and_not_as_an_implementation` asserts. Nothing is
  read after the edges. A read a later task adds there must release on `Err`. A lost race
  is `Ok(None)`, never an `Err`.
- `heartbeat` answers with two empty lists.
- `append_transcript` acknowledges through `offset + bytes.len()` and copies nothing. In
  solo, the runner's file is the board's copy (ADR-0028 point 4).
- `publish_tail` is `ServiceContext::publish_tail`.

**`board::service` holds the board's decisions, moved out of the runner, not copied:**
- `claim` and `preview` build `RunContext`: the task detail, the repository, base
  instructions, the effective strategy (`strategy::effective_strategy` over the global
  and repository defaults), the catalogue for the adapter's provider, and `TeamLimits`
  (`max_turns` and the stored `DISALLOWED_TOOLS` value, where `None` means unset). `preview`
  writes nothing. `max_turns` and `disallowed_tools` stay defined in `runner/process.rs`,
  where `runner/mod.rs` and `tests/runner_process.rs` import them, but only
  `board::service` calls `max_turns`. `forbidden_operations` becomes a pure function over
  `&TeamLimits`.
- `start_run` inserts the `runs` row under the `run_id` the runner minted (D10), with
  `kind`, `base_ref` and `base_sha` taken from `StartRun`. `outcome::start_run` keeps its
  signature and keeps minting an id for its existing callers. Both reach one crate-private
  insert, so the seven test files that arrange rows through it do not change.
- `finish_run` refuses an outcome that carries `resume_after` with `Error::invalid`. It
  then computes `resume_after` board-side, from `attempts::history`, `retry::decide`, the
  board's clock and `FinishRun::window_closes_at`, which is today's `apply_retry_policy`
  minus its pause write. It stores 033's head and bundle, calls `outcome::finish_run`,
  which still lands the task through `apply_to_task`, and answers
  `NextStep::Released { resume_after }`.
- `release` is `scheduler::claim::release`'s rule, unchanged: `running` becomes `failed`
  only if the task is still `running`. Before 043 there is no lease row and `LeaseRef`
  carries no purpose (D31 point 3), so the rule does not branch on purpose. A strategy
  lease still leaves `run_state` alone: a `Plan` claim takes no edge, and while the planner
  holds D19's slot no run of that task is live. The only `running` task it could meet is
  one a crash stranded, which the next launch's reconcile fails anyway. 043 makes `release`
  read the purpose from the lease row. A task that does not exist is `Error::not_found`.
- `record_strategy` is `set_task_strategy(…, StrategySource::Planner)`, always.
- `record_branch` writes `tasks.branch` only, never `worktree_path`, and publishes.
- `record_review_findings` calls task 035's writer, after checking that `run_id` is a
  run of the lease's task.
- Every method that takes a `LeaseRef` refuses a task that does not exist with
  `Error::not_found`. Every method that also takes a `run_id` refuses a run that belongs
  to a different task the same way. "Scoped by `lease`" (D31 point 3) means the lease
  bounds what a call may touch. No new error code: `Conflict` is 043's (D8).

**`run_task` takes a claim.** The signature is D31 point 5's, exactly: `board`, `ctx`,
`paths`, `config`, `claim`, `request`. `RunRequest` keeps `cancel` and `in_flight` and
loses `task_id`, `trigger` and `resume`. `RunRequest::manual` and `RunRequest::resuming`
go with them. Inside `run_task`:
- The prelude reads nothing from the board. It checks the opt-in and negotiates against
  `claim.context`, and releases if either refuses (D31 point 5: a refusal after the
  claim). The `RunIntent` it negotiates is built by one function that the starter also
  calls. The starter has no claim yet, so it passes a placeholder conversation id:
  `negotiate` judges whether the session is opened or continued, not which one.
- **Two `run_context` reads, both after `prepare_worktree`.** The first feeds
  `strategy::resolve`, whose planner prompt writes `- Branch:` through `task_context`
  (`runner/prompt.rs`), and, on a resume, the effective model and effort. The second,
  after the planner step, is what the implementation prompt is composed from. Neither is
  `claim.context`, which was read before `prepare` created the branch. Today's `get_task`
  after `prepare` and the one after the planner exist for these two reasons.
  `plan_claimed` keeps planning from its claim's context, as it plans from
  `claim_for_planning`'s pre-`prepare` read today.
- The five `process::release` sites become `board.release(&lease)`, and
  `process::release` and `process::claim` are deleted.
- `outcome::start_run` becomes `board.start_run` with a runner-minted id. The two
  `outcome::finish_run` calls become `board.finish_run`, with
  `transcript: TranscriptEnd::Complete { length }` read off the transcript file.
- `apply_retry_policy` is deleted. The window read stays runner-side and travels as
  `window_closes_at`. The pause write moves to after `NextStep::Released`, plus the note
  before `finish_run` below.
- `execute`'s `EventStream` publishes the tail through `board.publish_tail(&lease, …)`.
  This is a builder in the style of `EventStream::driven_by`, whose default stays
  `ServiceContext::publish_tail`, so `tests/runner_events.rs` does not change.

**Two intended behaviour changes.**
- **A manual run no longer strands its card on `running`.** Today `start_task_run` and
  `retry_task_now` claim through `scheduler::claim` and then spawn `run_task`, and every
  `?` in `run_task` before its internal `claim` returns without releasing: `get_task`,
  `negotiate`, `probe_cli`, `prepare_worktree`. The card then reads "running" until the
  next launch reconciles it. The queue does not have this bug, because `supervise`
  releases on `Err`. Once `run_task` takes a claim, every error path after it releases.
- **A usage limit with a known reset holds new starts before the board hears the run
  finished, and until at least that reset.** Today `apply_retry_policy` writes the pause
  and then `finish_run` publishes, but only when `retry::decide` returns a `resume_after`.
  D31 moves the write to after `NextStep::Released`, which opens a window in which
  `finish_run`'s change event wakes the queue before the pause exists, and a free slot can
  start another task into a closed window. To close it, before calling `finish_run` for a
  `usage_limit` outcome whose `usage_limit_resets_at` is known, the runner notes the pause
  at that instant. After `Released`, it notes it again at `resume_after`, if there is one.
  `pause::note_usage_limit` only ever lengthens the pause.
  - When the board resumes the task, the stored pause ends at `resume_after`, as today,
    though the setting is now written twice and publishes twice.
  - When it does not, the pause now holds until the reset where today there is none. That
    happens when the reset outlasts the run window (`GiveUp::OutlastsRunWindow`) or the
    attempt history cannot be read. This is deliberate: the limit is the account's, so a
    task started before the reset hits the same wall.
  - When the CLI reported no reset time, the fallback-poll case, the window remains. It
    is named as a residual.

  Record all three as a dated amendment to D31 point 4. A reviewer otherwise sees a
  decision that is in no ADR and no seam entry.

**The starters.**
- **Manual starts.** D31 point 5's preflight becomes one `rimaia-core` function that both
  `start_task_run` and `retry_task_now` call. The suggested home is
  `crates/core/src/runner/start.rs`. It runs `preview`, takes D19's slot
  (`acquire_unbounded`, `LeaseOwner::Manual`), checks the opt-in, negotiates, runs
  `probe_cli`, then `claim(Run { trigger, continue_session })`, and returns the slot and the
  `Claim`. It takes the `RunTrigger` as a parameter, so ADR-0026's "an unattended refusal
  writes nothing" keeps a caller that can assert it. A lost race keeps today's two
  sentences, byte for byte. 035's refusal of a review or fix point now arrives as the
  claim's `Err` and is returned as it is today. The shell commands shrink to calling this
  function and spawning `run_task`.
- **The queue.** `scheduler::build` gains an `Arc<dyn BoardPort>` parameter.
  - `try_step`'s `claim::claim` and `claim::claim_retry` calls become
    `claim(Run { trigger: Queued, continue_session })`.
  - Its `resume_point` read becomes `Claim::resume`. An `Invalid` from a
    `continue_session: true` claim is 035's refusal, handled as 035 handles it (warn, drop
    the slot, `continue`). Any other error propagates as today.
  - Its `claim::release` becomes `board.release`, and so does `supervise`'s. That makes
    `supervise`'s release a no-op after `run_task` has already released, which is correct,
    because the rule only acts on a `running` task.
- **The planner.** `claim_for_planning` becomes `preview`, then the slot, then
  `claim(Plan)`. `PlannerClaim` holds the `Claim` beside the slot, and every `PlanSkip`
  keeps its wording. `plan_claimed` releases its `Plan` claim on every path, after the
  last `record_strategy` and before the slot drops: a no-op before 043, and the lease
  row's deletion from it. `resolve`, which `run_task` calls under the implementation
  claim, releases nothing. `resolve` and `plan` take the `RunContext` their caller read
  and use its `{strategy, catalogue, limits}` in place of `effective_for`,
  `catalogue::catalogue` and `forbidden_operations(&ctx.pool, …)`. `effective_for` stays,
  for `would_plan` and `effective_mode` only.
  The "did it write" check reads `strategy_updated_at` through `run_context`.
  `record_failure` and `stamp_run_metadata` call `record_strategy`. `plan_all` takes the
  board. `selected_tasks`, `would_plan` and `effective_mode` are operator reads and stay
  where they are (D31 point 14).
- **Where the port is held.** D31 point 8:
  - `PlannerAccess` gets a `board` field. `testing::doctor::planner_access()` keeps its
    signature and fills it with `testing::board::Unwired`, a test-only `BoardPort` whose
    every fallible method answers `Error::invalid("this test does not plan")`, which is
    the helper's documented contract;
  - `AppState` gets `board_port`;
  - `src-tauri/src/lib.rs`'s `setup()` builds one `InProcessBoard` from the `runner` value
    it already builds, and hands clones to the queue, the MCP server's `PlannerAccess` and
    `AppState`;
  - `testing::context` gains `TestContext::board(&self, paths: &AppPaths, config:
    &RunnerConfig) -> Arc<dyn BoardPort>`, over the test's own context, taking the
    provider from `config` so a test cannot hit the provider trap below.
  - `RunnerConfig` and `ServiceContext` gain nothing.

**The contract suite skeleton.**
- `crates/core/src/testing/board_contract.rs`, behind the `testing` feature, exports the
  `Harness` trait with D31 point 13's four members, a `Which { A, B }`, and
  `board_contract!(Harness)`. The macro expands to one `#[tokio::test]` per case, so a
  failure names its case.
- `crates/core/tests/board_port_in_process.rs` invokes the macro with a harness that
  builds two `InProcessBoard`s over one `TestContext`.
- Cases arrange and inspect the board **only** through `Harness::board()`'s core services,
  and act only through `Harness::runner()`. None names `InProcessBoard`. That is what lets
  052 invoke the same macro over HTTP without editing a case.

## Out of scope

- **Leases.** No `runner_leases` row, no `tasks.lease_generation` read, no fencing, no
  `Conflict`, no expiry, no pinning. That is 043, and D28 owns the DDL. `claim.rs`'s two
  separately committed transactions stay two. 043 closes that window, and this task only
  routes through it.
- **`worktree::prepare`'s own write** of `tasks.branch` and `tasks.worktree_path`
  (`write_worktree_columns`, also reached from `plan_claimed`) stays a direct
  `ServiceContext` write. D31 point 7 assigns it to 066, which moves the path to
  `runner.db`. `record_branch` exists and is covered by the suite, and has no production
  caller until then. `base_ref::resolve` stays too (044).
- **Runner-owned state.** `settings::run_environment`, `schedule::window::active`,
  `pause::*`, `capacity`, `queue_state`, and the `QueueHandle` verbs are read through
  `ServiceContext` until 041 (D31 point 14). They never go through the port.
- **`reconcile::reconcile` and `startup::survey`** keep calling `outcome::finish_run` and
  `set_run_state` directly. Per-runner reconciliation is 043.
- **Selection.** `ClaimTarget::Next` and board-side selection belong to 042. The queue
  still ranks with `selection::plan` and claims each entry through `claim(Run)`.
- `run_tool`, and the planner's own `set_task_strategy` over the scoped MCP handle. That
  call keeps going to the MCP server directly until 055.
- **Transcripts leaving the machine**, and the outbox and resend rules (D31 point 12). That
  is 056. `finish_run` keeps today's "has already been finalized" refusal.
- **`runner/prompt.rs`.** `tests/prompt.rs` asserts its output exactly, and composition
  stays a pure function on the runner (ADR-0031 point 1).
- All of `src/`. No new Tauri command and no new MCP tool, so `check-command-wiring.sh`
  and `mcp/scope.rs`'s tool table do not change.

## Acceptance criteria

- `crates/core/src/board/{mod,port,types,service,in_process}.rs` exist. `BoardPort` has
  exactly the twelve methods in the Scope table, and `BoardMethod::ALL` names exactly
  those twelve. The trait is object-safe: `Arc<dyn BoardPort>` compiles.
- `run_task`'s signature is D31 point 5's. `RunRequest` has exactly `cancel` and
  `in_flight`. `runner::process::claim`, `runner::process::release` and
  `apply_retry_policy` no longer exist.
- **No function body in `runner/process.rs` or `runner/strategy.rs` calls the board
  directly.** The exceptions are the operator reads `selected_tasks`, `would_plan`,
  `effective_mode` and `effective_for`, and the definitions of `max_turns` and
  `disallowed_tools`, whose only `settings::get(` calls these are. A reviewer checks it
  with one `grep` over the two files for these patterns:
  - `set_run_state(`, `set_task_strategy(`
  - `get_task(`, `repo::get(`, `base_instructions(`
  - `max_turns(&`, `disallowed_tools(`, `forbidden_operations(&ctx.pool`, `settings::get(`
  - `strategy::settings::`, `catalogue::catalogue(`
  - `attempts::`, `retry::decide(`
  - `outcome::start_run(`, `outcome::finish_run(`, `ctx.publish_tail(`

  The remaining `ctx` uses are the clock, the runner-owned reads under Out of scope,
  `worktree::prepare` and the operator reads.
- **The existing suite passes, and changes only at construction sites.** Construction
  sites are:
  - calls to `run_task`, `scheduler::build`, `plan_all` and `PlannerAccess { … }`;
  - test fixture helpers that build a `RunRequest`, which now claim through
    `TestContext::board` first.

  Two tests in `tests/scheduler.rs` stand in, by their own comments, for the commands the
  starter replaces: `a_starter_that_claims_before_it_spawns_never_produces_a_second_process`
  and `retry_now_starts_a_waiting_task_before_its_deadline`. They move onto the starter or
  onto `TestContext::board`'s `claim(Run { … })`, so their claim calls, the `ClaimOutcome`
  checks and the second's session read and `RunRequest::resuming` change. Their end-state
  assertions do not: one process, the attempt count, the column, the `--resume` value.

  Otherwise no assertion changes. `tests/prompt.rs`, `tests/runner_events.rs`,
  `tests/runner_outcome.rs`, `tests/mcp_scope.rs` and `tests/mcp_tools.rs` are
  byte-identical to 035's tip. Every other hunk in `tests/scheduler.rs` touches only a call
  to `run_task` or `scheduler::build`.
- **Moved, not deleted.** The tests that asserted "refused before any run state is
  written" by calling `run_task` on an unclaimed task now call the starter function, with
  the same assertions: `a_missing_claude_binary_is_refused_before_any_run_state_is_written`,
  `a_repository_that_has_not_opted_in_cannot_start_a_task`,
  `a_provider_that_cannot_deny_a_tool_refuses_an_unattended_run`
  (with `RunTrigger::Queued`),
  and `a_task_that_failed_last_night_can_be_started_again`. Their names do not change.
  The third pins the starter's order only: no production caller starts a `Queued` run
  through the starter. The queue's half is the `run_task` release test below.
- **New behaviour tests**, with a faked clock, real git in a `TempDir`, the CLI replayed
  from fixture streams, and no `sleep`:
  - `a_manual_run_whose_worktree_cannot_be_prepared_does_not_leave_its_card_running`
    (CLAUDE.md, "Bug fix → failing test first"). CI runs on every commit, so the red run
    is never committed. Write it first against 035's API (`scheduler::claim`, then
    `run_task` with a `RunRequest`), run it red locally, record the failure in the commit
    message, then port it.
  - `run_task_releases_a_claim_whose_context_no_longer_negotiates`, driven by one queue
    pass over a provider that cannot deny a tool: the task ends `failed` and nothing is
    spawned.
  - `a_retry_claim_for_a_waiting_review_writes_nothing`. 035's `Error::invalid`, and the
    task is still `waiting_retry` with its row and `resume_after` unchanged. 021 replaces
    it.
  - `a_usage_limit_holds_new_starts_before_the_board_hears_the_run_finished`. A test-local
    `BoardPort` decorator reads `pause::active_until` when `finish_run` is entered, then
    delegates, driven by the recorded usage-limit fixture.
  - `the_pause_a_usage_limit_leaves_is_the_instant_the_board_chose_to_resume_at`. It
    asserts the exact `resume_after` and the exact stored pause against `TestClock`.
  - `a_usage_limit_that_outlasts_the_run_window_still_holds_new_starts_until_the_reset`.
    `resume_after` is `None`, and the stored pause is the exact reset.
  - `a_lost_manual_start_answers_with_todays_sentence`, and the same for a lost retry.
    Both assert exact strings.
  - `the_prompt_is_composed_from_the_task_as_it_reads_after_the_worktree_exists` and
    `the_planner_prompt_names_the_branch_prepare_created`. Each asserts its whole prompt,
    implementation and strategy, for a first-run task whose branch did not exist at claim
    time.
- **The contract skeleton runs against the in-process adapter.** `board_port_in_process.rs`
  passes, and covers at least D31 point 13's 036 list plus the methods this task ships:
  - `a_claimed_run_started_and_finished_lands_the_task_as_it_does_today`;
  - `a_claim_lost_to_another_starter_is_none_and_writes_nothing`;
  - `a_retry_claim_carries_the_session_and_kind_it_resumes`;
  - `a_plan_claim_moves_no_run_state_and_opens_no_run`;
  - `preview_returns_what_a_claim_would_and_writes_nothing`;
  - `releasing_an_implementation_lease_fails_a_running_task`;
  - `releasing_a_strategy_lease_leaves_run_state_alone`, over a `Plan` claim on a task
    that is not `running` (see `release` above);
  - `a_finish_that_chooses_its_own_resume_after_is_invalid`;
  - `a_recorded_strategy_is_always_sourced_as_the_planner`;
  - `a_published_tail_reaches_a_tail_subscriber`;
  - `a_transcript_chunk_is_acknowledged_through_its_end`;
  - `start_run_records_the_run_id_the_runner_minted`;
  - `record_branch_sets_the_branch_and_never_the_worktree_path`;
  - `review_findings_recorded_through_the_port_land_on_their_review_run`;
  - `a_finish_for_another_tasks_run_is_not_found`;
  - `a_solo_heartbeat_fences_nothing_and_cancels_nothing`;
  - `every_lease_method_answers_not_found_for_a_task_that_does_not_exist`. It iterates
    `BoardMethod::ALL`, skipping the three lease-less methods and `publish_tail`, which
    cannot fail. 043 extends it into D31's `every_lease_method_refuses_a_stale_generation`.

  No case names `InProcessBoard`.
- `every_board_dto_round_trips_through_json` passes for one value of each type in
  `types.rs`, and `git diff` shows no change to any existing type's `Serialize` attributes.
- D31 carries a dated amendment by this task: the usage-limit note and its three cases
  (point 4); `Next` and `FreeCapacity` added by 042, not 036 (point 7); and `Catalogue`'s
  `deny_unknown_fields`, left to 052 (point 6). The seam-contract "How to use this" table
  has a row for 036 listing D5, D8, D10, D14, D17, D19, D23, D27, D28, D29, D30, D31, D32
  and D34.
- **No migration was added.** `.sqlx/` is regenerated only for queries this task adds
  (`record_branch`'s `UPDATE`, and anything else new). Existing query text is unchanged.
- Every CI check passes: `npm run typecheck`, `npm run test`, `npm run build`,
  `cargo test -p rimaia-core`, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`.

## Notes

**Read first:**
- [ADR-0027](../docs/adr/0027-a-server-for-the-board-and-runners-for-the-work.md) point 5;
- [ADR-0031](../docs/adr/0031-runners-claim-work-with-leases.md) points 1, 5 and 7, for what
  a claim will become;
- [ADR-0015](../docs/adr/0015-testing-strategy-and-crate-split.md);
- ADR-0011 (the retry table and pause this task relocates) and ADR-0006 (one rule, one
  place);
- seam-contract **D31 in full**;
- **D29** points 1–3 and 9 (`RunKind`, `ResumePoint`, `StartRun::kind`);
- **D30** point 7 (from here on, `finish_run` is where "did the review record anything" is
  read);
- **D19** (the slot stays the runner's);
- **D23** (the retry policy being moved);
- **D14** (a dropped tail costs nothing);
- **D17.5** (the planner has no row);
- **D27** (the `Arc` provider shape, and why the board's provider must be the runner's);
- **D28** (`LeasePurpose`'s spelling; no DDL here), **D32** point 3 (why `board_port`);
- **D8**, **D10**, **D5** (`.sqlx`), and D4, D6 and D34 as prohibitions (no `async-trait`).

**Files to start from:**
- `crates/core/src/runner/process.rs`: `run_task` from about line 732, `apply_retry_policy`,
  `claim`, `release`, `execute`;
- `crates/core/src/runner/outcome.rs`: `start_run`, `finish_run`, `apply_to_task`;
- `crates/core/src/runner/strategy.rs`: `resolve`, `effective_for`, `plan`,
  `planner_intent`, `record_failure`, `stamp_run_metadata`, `claim_for_planning`,
  `plan_claimed`, `plan_all`, `PlannerAccess`;
- `crates/core/src/runner/events.rs`: `EventStream::create`, `driven_by`;
- `crates/core/src/scheduler/claim.rs`, `queue.rs` (`build`, `try_step`, `supervise`),
  `attempts.rs`, `pause.rs`;
- `crates/core/src/testing/context.rs` and `testing/doctor.rs` (`planner_access`);
- `crates/core/src/runner/prompt.rs` (`task_context`), read only;
- `crates/core/src/clock.rs`, for the boxed-future shape;
- `src-tauri/src/commands/runs.rs` (`start_task_run`, `retry_task_now`),
  `commands/strategy.rs`, `commands/mcp.rs`, `src-tauri/src/state.rs`,
  `src-tauri/src/lib.rs` (`setup()`, around the `scheduler::build` call).

**Test churn.** These test files call `run_task` or build what it needs, and change at
construction sites only:
- `runner_process.rs`
- `runner_strategy.rs`
- `runner_credentials.rs`
- `provider_process.rs`
- `provider_seam.rs`
- `scheduler.rs`
- `doctor.rs`

**Migration:** none. D4 makes adding one a stop-and-ask, and nothing here needs a column.

**What the chain hands this task.**
- **033:** `runs.head_sha`, `runs.base_sha` and the review bundle, computed by the runner at
  finish. They become `StartRun::base_sha`, `FinishRun::head_sha` and `FinishRun::bundle`.
- **035:** `RunKind`, `NewRun::kind`, `ResumePoint` with its kind dispatch at both resume
  callers, the findings writer, and the `rimaia-run` handle name (D30). Every one of these
  is a value the port carries, and this task defines none of them.

**What the next tasks expect.**
- **021:** adds `NextStep::Continue`, `RunContext::review`, and the review and fix arms of
  `finish_run`, in `board::service`. It dispatches `run_task` on the kind that
  `start_run` records.
- **038:** adds `LeaseRef::team_id` and the solo runner's id on `InProcessBoard`.
- **042:** adds `ClaimTarget::Next`.
- **043:** replaces `claim`'s body with one transaction and a lease row, and extends the
  contract suite through `BoardMethod::ALL`.
- **052:** invokes `board_contract!` over HTTP without editing a case. If a case here
  reaches past `Harness`, 052 finds out the hard way.

**Traps.**
- `claim.context` predates the branch, and two prompts name it: the planner's and the
  implementation's. See Scope.
- `InProcessBoard.provider` and `RunnerConfig.provider` must be the same value. With the
  Ledger provider in `provider_process.rs`, a default-built board gives the planner
  Claude's catalogue, and nothing fails loudly. `TestContext::board` rules it out in
  tests. `lib.rs` has to get it right by building both from one value.
- Re-sourcing the adapter's context to `System` changes the `source` field on the
  `tracing` spans of a manual run's board writes, which used to say `ui`. D31 point 9
  decides this. It is not a regression, and nothing stored changes.

**Size.** L, and close to the ceiling. Estimated diff:

| Part | Lines |
| --- | --- |
| `board/` | ~1,000–1,200 |
| `runner/` edits | ~600 |
| Starter function, queue and shell | ~300 |
| Contract module and its tests | ~700 |
| Construction-site churn | ~300 |
| **Total** | **~3,000** |

If it runs over, cut at the planner. Land the implementation path first: the port, the
adapter, `run_task`, the queue, the manual starters and the suite, with `record_strategy`
and `claim(Plan)` implemented and covered by contract cases but not yet called from
`runner/strategy.rs`. Then stop and return `blocked`, proposing the follow-up that moves
`runner/strategy.rs` onto them. The workflow runs a task list fixed at launch, so a person
has to add the follow-up before 021 starts. Do not leave `strategy.rs` half-moved: every
function in it reaches the board one way or the other.
