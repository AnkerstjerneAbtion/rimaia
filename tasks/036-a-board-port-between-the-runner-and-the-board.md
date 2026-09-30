---
id: "036"
title: A board port between the runner and the board
milestone: v0.4
status: ready
depends_on: ["035"]
adrs: ["0027", "0031", "0015"]
size: L
---

# A board port between the runner and the board

## Goal

Draw the line [ADR-0027](../docs/adr/0027-a-server-for-the-board-and-runners-for-the-work.md)
point 5 decides, while there is still only one process on each side of it: a `BoardPort`
trait that runner code uses for everything it says to the board, an in-process adapter
that solo uses, and the skeleton of the contract suite that task 052's HTTP adapter will
also have to pass. After this task, `runner::process::run_task`, `runner/outcome.rs` and
`runner/strategy.rs` reach the board only through the port. Nothing about the board, the
queue or a run behaves differently, apart from the one fix named under Scope.

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
| `ClaimTarget::Next`, `FreeCapacity` | no. A variant whose only body is a refusal cannot be told apart from a bug | 042 |
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
not change, because `src/types.ts` reads it.

**The in-process adapter** follows D31 point 9:
- `ctx` is re-sourced to `MutationSource::System` at construction.
- `provider` is read only to build `RunContext::catalogue`. It must be the same provider as
  the `RunnerConfig` it serves. `lib.rs` and the tests build both from one value.
- Every method is one call into `board::service`.
- `claim` returns generation `0` and runs today's routes: `scheduler::claim::claim` for
  `Run { continue_session: false }`, `claim_retry` and 035's `attempts::resume_point` for
  `Run { continue_session: true }`, and no state edge for `Plan`. A lost race is `Ok(None)`,
  never an `Err`.
- `heartbeat` answers with two empty lists.
- `append_transcript` acknowledges through `offset + bytes.len()` and copies nothing. In
  solo, the runner's file is the board's copy (ADR-0028 point 4).
- `publish_tail` is `ServiceContext::publish_tail`.

**`board::service` holds the board's decisions, moved out of the runner, not copied:**
- `claim` and `preview` build `RunContext`: the task detail, the repository, base
  instructions, the effective strategy (`strategy::effective_strategy` over the global
  and repository defaults), the catalogue for the adapter's provider, and `TeamLimits`
  (`max_turns` and the stored `DISALLOWED_TOOLS` value, where `None` means unset). `preview`
  writes nothing.
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
- `release` is `scheduler::claim::release`'s rule for an implementation lease (`running`
  becomes `failed` only if the task is still `running`). For a strategy lease it leaves
  `run_state` alone. A task that does not exist is `Error::not_found`.
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
  claim).
- The prompt is composed from **one `run_context` read taken after `worktree::prepare` and
  after the planner**, never from `claim.context`. The claim was read before the branch
  existed, and today's code re-reads the task after `prepare` for exactly that reason.
- The five `process::release` sites become `board.release(&lease)`, and
  `process::release` and `process::claim` are deleted.
- `outcome::start_run` becomes `board.start_run` with a runner-minted id. The two
  `outcome::finish_run` calls become `board.finish_run`, with
  `transcript: TranscriptEnd::Complete { length }` read off the transcript file.
- `apply_retry_policy` is deleted. The window read stays runner-side and travels as
  `window_closes_at`. The pause write moves to after `NextStep::Released`, plus the
  ordering fix below.
- `execute`'s `EventStream` publishes the tail through `board.publish_tail(&lease, …)`.
  This is a builder in the style of `EventStream::driven_by`, whose default stays
  `ServiceContext::publish_tail`, so `tests/runner_events.rs` does not change.

**The one intended behaviour change, and the one ordering this task must keep.**
- **A manual run no longer strands its card on `running`.** Today `start_task_run` and
  `retry_task_now` claim through `scheduler::claim` and then spawn `run_task`, and every
  `?` in `run_task` before its internal `claim` returns without releasing: `get_task`,
  `negotiate`, `probe_cli`, `prepare_worktree`. The card then reads "running" until the
  next launch reconciles it. The queue does not have this bug, because `supervise`
  releases on `Err`. Once `run_task` takes a claim, every error path after it releases.
  Write the failing test first (CLAUDE.md, "Bug fix → failing test first").
- **The usage-limit pause stays in force before the board hears the run finished.** Today
  `apply_retry_policy` writes the pause and then `finish_run` publishes. D31 moves the
  pause write to after `NextStep::Released`, which opens a window in which `finish_run`'s
  change event wakes the queue before the pause exists, and a free slot can start another
  task into a closed window. Keep today's order. Before calling `finish_run` for a
  `usage_limit` outcome whose `usage_limit_resets_at` is known, note the pause at that
  instant. After `Released`, note it again at `resume_after`. `pause::note_usage_limit`
  only ever lengthens the pause, so the stored value ends up exactly where it does today.
  When the CLI reported no reset time, the fallback-poll case, the window remains, and it
  is named as a residual. Record this as a dated amendment to D31 point 4. A reviewer
  otherwise sees a decision that is in no ADR and no seam entry.

**The starters.**
- **Manual starts.** D31 point 5's preflight becomes one `rimaia-core` function that both
  `start_task_run` and `retry_task_now` call. The suggested home is
  `crates/core/src/runner/start.rs`. It runs `preview`, takes D19's slot
  (`acquire_unbounded`, `LeaseOwner::Manual`), checks the opt-in, negotiates, runs
  `probe_cli`, then `claim(Run { trigger, continue_session })`, and returns the slot and the
  `Claim`. It takes the `RunTrigger` as a parameter, so ADR-0026's "an unattended refusal
  writes nothing" keeps a caller that can assert it. A lost race keeps today's two
  sentences, byte for byte. A `Claim::resume` whose kind is not `Implementation` is
  refused with `Error::invalid` and released, as 035 wired it. The shell commands shrink
  to calling this function and spawning `run_task`.
- **The queue.** `scheduler::build` gains an `Arc<dyn BoardPort>` parameter.
  - `try_step`'s `claim::claim` and `claim::claim_retry` calls become
    `claim(Run { trigger: Queued, continue_session })`.
  - Its `resume_point` read becomes `Claim::resume`.
  - Its `claim::release` becomes `board.release`, and so does `supervise`'s. That makes
    `supervise`'s release a no-op after `run_task` has already released, which is correct,
    because the rule only acts on a `running` task.
- **The planner.** `claim_for_planning` becomes `preview`, then the slot, then
  `claim(Plan)`. `PlannerClaim` holds the `Claim` beside the slot, and every `PlanSkip`
  keeps its wording. `resolve` and `plan` read
  `RunContext::{strategy, catalogue, limits}` in place of `effective_for`,
  `catalogue::catalogue` and `forbidden_operations(&ctx.pool, …)`.
  The "did it write" check reads `strategy_updated_at` through `run_context`.
  `record_failure` and `stamp_run_metadata` call `record_strategy`. `plan_all` takes the
  board. `selected_tasks`, `would_plan` and `effective_mode` are operator reads and stay
  where they are (D31 point 14).
- **Where the port is held.** D31 point 8:
  - `PlannerAccess` gets a `board` field;
  - `AppState` gets `board_port`;
  - `src-tauri/src/lib.rs`'s `setup()` builds one `InProcessBoard` from the `runner` value
    it already builds, and hands clones to the queue, the MCP server's `PlannerAccess` and
    `AppState`;
  - `testing::context` gains `TestContext::board`, over the test's own context.
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
  `ServiceContext` write. D31 point 7 assigns it to 041, which moves the path to
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
- **No function in `runner/process.rs`, or reachable from `resolve`, `plan`,
  `plan_claimed` or `claim_for_planning` in `runner/strategy.rs`, calls any of these:**
  - `set_run_state`
  - `set_task_strategy`
  - `tasks::get_task`
  - `repo::get`
  - `settings::base_instructions`
  - `max_turns`
  - `strategy::settings::*`
  - `catalogue::catalogue`
  - `attempts::*`
  - `retry::decide`
  - `outcome::start_run` or `outcome::finish_run`
  - `ServiceContext::publish_tail`

  A reviewer checks this with one `grep` over the two files. The remaining `ctx` uses are
  the clock, the runner-owned reads under Out of scope, `worktree::prepare`, and
  `selected_tasks`, `would_plan` and `effective_mode`, the operator reads.
- **The existing suite passes, and changes only at construction sites.** Construction
  sites are:
  - calls to `run_task`, `scheduler::build`, `plan_all`, `PlannerAccess { … }` and
    `testing::doctor::planner_access`;
  - test fixture helpers that build a `RunRequest`, which now claim through
    `TestContext::board` first.

  No assertion changes. `tests/prompt.rs`, `tests/runner_events.rs`,
  `tests/runner_outcome.rs`, `tests/mcp_scope.rs` and `tests/mcp_tools.rs` are
  byte-identical to 035's tip. In `tests/scheduler.rs`, every hunk touches only a call to
  `run_task` or `scheduler::build`.
- **Moved, not deleted.** The tests that asserted "refused before any run state is
  written" by calling `run_task` on an unclaimed task now call the starter function, with
  the same assertions: `a_missing_claude_binary_is_refused_before_any_run_state_is_written`,
  `a_repository_that_has_not_opted_in_cannot_start_a_task`,
  `a_provider_that_cannot_deny_a_tool_refuses_an_unattended_run`
  (with `RunTrigger::Queued`),
  and `a_task_that_failed_last_night_can_be_started_again`. Their names do not change.
- **New behaviour tests**, with a faked clock, real git in a `TempDir`, the CLI replayed
  from fixture streams, and no `sleep`:
  - `a_manual_run_whose_worktree_cannot_be_prepared_does_not_leave_its_card_running`. It
    fails on 035's tip and passes here.
  - `run_task_releases_a_claim_whose_context_no_longer_negotiates`.
  - `a_usage_limit_holds_new_starts_before_the_board_hears_the_run_finished`. A test-local
    `BoardPort` decorator reads `pause::active_until` when `finish_run` is entered, then
    delegates, driven by the recorded usage-limit fixture.
  - `the_pause_a_usage_limit_leaves_is_the_instant_the_board_chose_to_resume_at`. It
    asserts the exact `resume_after` and the exact stored pause against `TestClock`.
  - `a_lost_manual_start_answers_with_todays_sentence`, and the same for a lost retry.
    Both assert exact strings.
  - `the_prompt_is_composed_from_the_task_as_it_reads_after_the_worktree_exists`. It
    asserts the whole prompt against a first-run task whose branch did not exist at claim
    time.
- **The contract skeleton runs against the in-process adapter.** `board_port_in_process.rs`
  passes, and covers at least D31 point 13's 036 list plus the methods this task ships:
  - `a_claimed_run_started_and_finished_lands_the_task_as_it_does_today`;
  - `a_claim_lost_to_another_starter_is_none_and_writes_nothing`;
  - `a_retry_claim_carries_the_session_and_kind_it_resumes`;
  - `a_plan_claim_moves_no_run_state_and_opens_no_run`;
  - `preview_returns_what_a_claim_would_and_writes_nothing`;
  - `releasing_an_implementation_lease_fails_a_running_task`;
  - `releasing_a_strategy_lease_leaves_run_state_alone`;
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
- D31 carries a dated amendment by this task for the usage-limit ordering, and the
  seam-contract "How to use this" table has a row for 036 listing D8, D10, D14, D17, D19,
  D23, D29, D30 and D31.
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
- seam-contract **D31 in full**;
- **D29** points 1–3 and 9 (`RunKind`, `ResumePoint`, `StartRun::kind`);
- **D30** point 7 (from here on, `finish_run` is where "did the review record anything" is
  read);
- **D19** (the slot stays the runner's);
- **D23** (the retry policy being moved);
- **D14** (a dropped tail costs nothing);
- **D17.5** (the planner has no row);
- **D8**, **D10**, and D4 and D6 as prohibitions.

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
- `claim.context` is stale by the time the prompt is composed. See Scope.
- `InProcessBoard.provider` and `RunnerConfig.provider` must be the same value. With the
  Ledger provider in `provider_process.rs`, a default-built board gives the planner
  Claude's catalogue, and nothing fails loudly.
- Re-sourcing the adapter's context to `System` changes the `source` field on the
  `tracing` spans of a manual run's board writes, which used to say `ui`. D31 point 9
  decides this. It is not a regression, and nothing stored changes.
- `supervise`'s `release` stays. It is a no-op once `run_task` has released, and the
  backstop if a future error path forgets to.

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
`runner/strategy.rs`. Then move `runner/strategy.rs` onto them as a follow-up task with the
next free number, placed directly after 036 in `tasks/README.md`. Say so in the PR. Do not
leave `strategy.rs` half-moved: every function in it reaches the board one way or the
other.
