---
id: "035"
title: Runs have a kind, and review findings have a home
milestone: v0.4
status: ready
depends_on: ["033", "034"]
adrs: ["0006", "0011", "0016", "0017", "0021", "0022", "0026"]
size: L
---

# Runs have a kind, and review findings have a home

## Goal

Lay the ground task 021's loop runs on, without running a loop yet:

- **`runs.kind`.** Every run row says whether it is an implementation, a review or a fix,
  and every reader of `runs` on `main` says which kinds it means, as seam-contract D29
  decides for each one.
- **A findings store.** `review_findings` holds what a reviewer found. A reviewer writes it
  through the run-scoped handle, a fix run resolves entries in it, and the operator reads
  it.
- **The task-021 trap, closed.** The run-scoped handle is served as `rimaia-run` (D30).
  The operator surface, `mcp__rimaia*`, stays denied to every run the runner spawns, and
  a run-scoped handle reaches only its own task.

When this lands, no review or fix run has been spawned. Every door those runs will use
exists, is scoped and is tested, and every reader already gives the right answer on a
board that has review and fix rows.

## Why now

Task 021 needs three things that do not exist on `main`:

- a way to tell its rows apart from implementation attempts;
- somewhere to write findings;
- a handle a review run can call without tripping the denial task 020 installed.

Each of the three is a cross-cutting change, and none of them is a loop. Building them
inside 021 would put the one behavioural change D29 makes to the retry budget, a
security-relevant rename of the scoped server, and the loop engine in one diff. The
reviewer could then not tell which of the three a failing scheduler test was about.

Order matters as well. Task 036 moves `run_task`'s board writes behind the `BoardPort`
trait (D31), and that trait's `start_run` carries `StartRun::kind`, its `claim` carries
`Claim::resume: Option<ResumePoint>`, and it has a `record_review_findings` method. All
three are this task's types. Landing them first means 036 moves code that already speaks
kinds, rather than moving it and then changing it.

The trap itself is the last reason. `runner/process.rs` carries a doc comment titled
"A trap for task 021", which lists three ways out and chooses none. D30 chooses one. The
choice has to be made before any code hands a run a grant, because the obvious
alternative, denying only when no grant was minted, would open the whole operator surface
to exactly the runs that carry a handle, under `bypassPermissions`.

## Scope

The work falls into two halves that touch different files, and they land as two runs of
commits in this order, so a reviewer can read them apart:

1. the migration, D29's audit and the findings store;
2. D30's respelling, grants, the fixtures, the three tools and the command.

### The contract is already amended; implement it

D28's amendment of 2026-09-30 ("two columns in task 035's file, before it is written") and
the dated line under D30 point 7 were made in Phase 0, and say why. Both columns are already
in D28 part 6's DDL for this file, and the first is already in 038's rebuild of `runs`:

- `runs.findings_recorded_at TEXT`, the witness of a clean review, set by
  `review::findings::record` in the same transaction as its rows, including when it writes
  none;
- `review_findings.ordinal INTEGER NOT NULL`, with `idx_review_findings_review_run`
  becoming `UNIQUE (review_run_id, ordinal)`.

This task writes the file as part 6 now reads. It edits neither entry again.

### The migration

`src-tauri/migrations/20261001120100_run_kinds_and_review_findings.sql`, exactly D28
part 6's DDL for this file. It includes task 021's columns, which this task writes and does
not read:
- `tasks.review_instructions`;
- `tasks.review_config`;
- `repositories.review_config`.

The file's header comment says so, in the voice of the existing migrations. Regenerate
`.sqlx/` with D5's recipe, including `--all-targets`. D33 changes nothing here: the second
cache arrives with task 040.

### Runs have a kind (D29, all nine points)

- `RunKind { Implementation, Review, Fix }` in `crates/core/src/db/models.rs`, beside
  `RunStatus`, with the same derives and `rename_all = "snake_case"`.
  - `Run` gains `kind`. It does not gain `findings_recorded_at`; the findings store owns
    the one reader of that column (below).
  - `NewRun` gains `kind: RunKind` as a required field with no `Default`.
  - `start_run` binds it explicitly.
  - `finish_run` never takes it from its caller.
  - Every explicit-column `query_as!(Run, …)` adds `kind AS "kind: RunKind"`. On `main`
    there are three: `fetch_run_row` (`runner/outcome.rs`), `fetch_last_run`
    (`tasks/service.rs`) and `fetch_run` in `tests/store.rs`. The last one does not stay
    valid through the column default, whatever D29's list of test readers says, because a
    `query_as!` into a struct with a field it does not select does not compile.
- **`attempt` stays one sequence per task, across every kind.** `idx_runs_task_attempt`
  does not change (point 2).
- **The retry budget ends at the first row whose kind or session differs** (point 3,
  amending D23 point 7).
  - `attempt_rows` selects `kind`.
  - `fold` gets one extra condition in its single newest-first pass.
  - `resumable_session` becomes `resume_point(ctx, task_id) ->
    Result<Option<ResumePoint>>`, with `ResumePoint { kind, session_id }`.
- **A resume is decided before the claim, and a refusal writes nothing** (point 3).
  - `scheduler::attempts::resume_as_implementation(point: Option<ResumePoint>) ->
    Result<Option<ResumeSession>>` is a pure function. `Implementation` becomes today's
    `ResumeSession`, `None` stays `None`, and `Review` and `Fix` refuse with
    `Error::invalid`, whose message names task 021 (D8: no new code). It is the one place
    the refusal is spelled.
  - **Both callers read the point and apply that function before they claim.** Today both
    claim first and read the session afterwards: `try_step`'s `resuming` branch in
    `scheduler/queue.rs` calls `claim_retry` and then `resumable_session`, and so does
    `retry_task_now` in `src-tauri/src/commands/runs.rs`. A refusal after the claim would
    leave the task `running` with no process, and `claim::release` would then move it to
    `failed`, throwing away the retry it was waiting for. So the read moves above the
    claim. It is safe there because both callers already hold D19's `InFlight` slot for the
    task, and while it is held nothing else in the app can start a run of that task, so the
    point read before the claim is the point the claim resumes. The comment in `try_step` that
    says "After the claim, never before" is rewritten to say this.
  - **The queue logs the refusal and moves to the next entry.** `tracing::warn!` with the
    task id and the refusal, drop that entry's slot, `continue`. It does not use `?`, which
    would abort the step for every other entry in the batch. It does not set `worked`, so a
    refused entry does not make the loop spin; the next change event wakes it as usual. A
    database error from `resume_point` still propagates as today's `resumable_session`
    error does.
  - **"Retry now" returns the refusal** to the UI before it calls `claim_retry`. The task
    stays `waiting_retry`, with its row and `resume_after` untouched.
  - Before 021, no production path writes a review or fix row, so neither refusal is
    reachable outside a test or a hand-edited board. In sequential mode a refused entry
    would hold the one slot on every pass. That is accepted for the same reason, and 021
    replaces both refusals.
- **`finish_run` dispatches on the kind of the row it closes, before it writes anything**
  (point 9). One `match run.kind` sits after the "already finalized" check and before the
  `UPDATE`. The `Implementation` arm is today's body: the `UPDATE`, the commit, the two
  events and `apply_to_task`. The `Review` and `Fix` arms return the same 021 refusal
  without touching the row or the task. Refusing after the `UPDATE`, which is where
  `apply_to_task` runs today, would commit a closed row and then leave the task `running`
  with no `waiting_retry` and no `resume_after` applied.
- **Tests close review and fix rows with a test helper, never with `finish_run`.**
  `crates/core/src/testing/runs.rs` gains `close_run(ctx, run_id, status, exit_class,
  resume_after)`, which `UPDATE`s the row's `ended_at` (from `ctx.clock`), `status`,
  `exit_class` and `resume_after` directly. A test that needs the task waiting then calls
  `set_run_state(ctx, task_id, RunState::WaitingRetry)`, which is ADR-0011's legal
  `running → waiting_retry` edge. Implementation rows in the same tests still go through
  `finish_run`. The helper's query is a `query!` and goes into the `.sqlx` cache with the
  rest.
- **The card's last run is the newest row of any kind** (point 4). `LastRunSummary`
  gains `kind`, read from the same correlated join in `TASK_SUMMARY_SELECT`, so one board
  read is still one query. `mcp::responses::RunView` gains `kind`, so `get_task`'s
  `last_run` says which kind it is. `fetch_last_run` keeps its `WHERE` and `ORDER BY` and
  adds the column. `has_scheduled_resume`, `recorded_base_ref` and `skip_reason` keep
  their SQL unchanged.
- **History readers take every kind with no filter** (point 6): `list_runs_for_task`,
  `fetch_run` and what is built on it, both of `prune_logs`' `SELECT`s,
  `missing_run_logs`, `open_runs`, `ensure_repository_is_reassignable` and
  `observed_run_cost`. `RunFilter` gains `kind: Option<RunKind>`; the control on screen is
  task 037's.
- **Analytics** (point 7, ADR-0022).
  - `runs_in` selects `r.kind`.
  - `implementation_spend_usd`, `outcomes` with its `failure_rate`,
    `median_duration_seconds` and `strategies` count implementation rows only. Every other
    figure counts every kind.
  - `review_loop_spend_usd` and `review_loop_outcomes: RunOutcomes` are added.
  - The report asserts `spend_usd == implementation_spend_usd + review_loop_spend_usd`.
  - **Both doors carry the new fields** (ADR-0021). `src/types.ts` mirrors them, and
    `mcp::responses::AnalyticsView` gains `review_loop_spend_usd` and
    `review_loop_outcomes`, a nested view with the same five counts and `failure_rate` that
    its flat fields carry. The doc comments on those flat fields (`runs_total` …
    `failure_rate`) say they now count implementation runs only.
- **Loop numbers are derived, never stored** (point 8). Task 034's `review::digest`
  reports one entry per task. This task adds two fields to `DigestEntry`, and changes no
  existing field:
  - `last_run_kind: Option<RunKind>`, the kind of the newest row the entry's outcome is
    already taken from. The digest's newest-row read selects `kind`. It is `None` for a
    `Blocked` or `Skipped` entry with no row in the window. The outcome table does not
    change: a succeeded review is `Completed` with `last_run_kind: Some(Review)`, and what
    that means for the card is 021's.
  - `review_loop: Option<DigestLoop>`, where `DigestLoop { reviews_since_implementation:
    u32, open_findings: u32 }`. The first is the number of review rows after the task's
    newest implementation row. The second is the task's `open` findings of every loop. It
    is `None` when both are zero. Both are computed from rows, with no new column.
  - Totals stay over runs, and count every kind, as D29 point 7 counts spend.
  - `src/types.ts` mirrors both fields and `DigestLoop` (`lastRunKind`, `reviewLoop`,
    `reviewsSinceImplementation`, `openFindings`). 034's digest projection in
    `mcp/responses.rs` gains them in snake_case.
- `runner/strategy.rs`'s header: the third reason the planner has no row ("a fourth
  migration") is no longer true. Rewrite it to point at D29 point 1, and keep analytics
  (`planner_spend` counted twice) as the reason that still holds. `'strategy'` is not a
  `RunKind`.
- `src/types.ts` mirrors the enum and every field above: `Run`, `LastRunSummary`,
  `RunListEntry`, the analytics report and `DigestEntry`. Every object that builds one of
  these gains the fields, or `npm run typecheck` fails: the frontend test fixtures in
  `src/**/*.test.tsx`, and task 028's typed seed in `src/dev/fixtures/`, as 033 did for
  `headSha`. Every existing seeded row is an implementation run, so the seed gains
  `kind: "implementation"` and nothing new to look at. **No component renders them.**
  Rendering is task 037's.
- Seam-contract pointers, one dated line each, editing nothing else:
  - under D12: `last_run.kind`;
  - under D23 point 7: the budget boundary.

### The findings store

A new file, `crates/core/src/review/findings.rs`, in the `review/` module task 034
created. It is the only writer of `review_findings` and the only reader of
`runs.findings_recorded_at`. Its types are:

- `FindingSeverity { Critical, High, Medium, Low }` and
  `FindingStatus { Open, Fixed, Rejected }`. These are enums, per CLAUDE.md, with D28's
  `CHECK` spelling.
- `NewReviewFinding { severity, title, body, file: Option<String>, line: Option<i64> }`,
  serde `camelCase`. Every field is one word, so `camelCase` and D16.1's `snake_case`
  spell them identically, and the MCP tool reuses the type as its argument element. A
  field with two words, if one is ever added, gets its own projection in
  `mcp/requests.rs` instead. Task 036's `board::types` re-exports this type rather than
  redefining it.
- `ReviewFinding`, the row, including `ordinal`.

Its four functions are the whole of the store. The two that write carry ADR-0019's
`#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), task_id =
%task_id))]`, as 034's actions do.

- **`record(ctx, task_id, review_run_id, findings) -> Result<Vec<ReviewFinding>>`.** One
  transaction. It refuses, with `Error::invalid` and a specific message for each case,
  when:
  - the run is not `kind = 'review'`;
  - the run does not belong to `task_id`;
  - the run is no longer `running`;
  - the run already has `findings_recorded_at` set, meaning a second call.

  It also refuses a finding whose `title` or `body` is blank, and one with a `line` but no
  `file`. Otherwise it inserts every finding with `status = 'open'`, `ordinal` set to the
  finding's index in `findings` (from 0), and `created_at` from `ctx.clock`, and sets
  `findings_recorded_at` from `ctx.clock`, including for `findings: []`.
- **`resolve(ctx, task_id, finding_id, fix_run_id, resolution) -> Result<ReviewFinding>`.**
  `resolution` is `Fixed { note: Option<String> }` or `Rejected { reason: String }`.
  It refuses when:
  - the finding is not on `task_id`;
  - the finding is not `open`;
  - the fix run is not `kind = 'fix'`, not this task's, or not `running`;
  - a rejection's reason is blank. This is refused before SQL. D28's table `CHECK` is the
    backstop, not the message.

  Otherwise it sets `status`, `resolution`, `resolved_by_run_id` and `resolved_at` from
  `ctx.clock`.
- **`list(ctx, task_id, status: Option<FindingStatus>) -> Result<Vec<ReviewFinding>>`.**
  Ordered by the review run's `attempt`, then by `ordinal`. D10 ids are UUIDs and say
  nothing about order, and `TestClock` gives equal timestamps.
- **`recorded_at(ctx, review_run_id) -> Result<Option<DateTime<Utc>>>`.** D30 point 7's
  witness, as its amendment reads it: set means the review called, `None` means it did
  not. Nothing calls it in this task. 021 reads it, through 036's `finish_run`.

`fingerprint` is written `NULL`. The column is "021's key for 'the same finding again'"
(D28), and how it is computed and matched is 021's decision. **A reviewer never supplies
it.** A finding that says which earlier finding it repeats is a reviewer grading its own
novelty.

Each write publishes the change event that `get_task`'s readers already listen on, the
way other task-scoped writes do (D2, ADR-0018).

### The run-scoped handle is `rimaia-run` (D30 points 1–5, 7 and 8)

1. **Server names** (D30 point 1).
   - `RUN_MCP_SERVER_NAME = "rimaia-run"` goes beside `MCP_SERVER_NAME` in
     `crates/core/src/mcp/mod.rs`.
   - Production builds every `RimaiaHandle` with it. Today that is the two sites in
     `runner/strategy.rs`.
   - `negotiate` refuses, on `RefusalAxis::HandleInjection`, an intent whose
     `rimaia_handle.server` is `MCP_SERVER_NAME`. That turns D30's "no handle is ever built
     with the operator name" into a check every intent passes through, including 021's,
     rather than a property of today's two call sites. Test code that builds a
     `RimaiaHandle` by hand keeps doing so.
   - The server's `get_info` (`mcp/server.rs`) reports `RUN_MCP_SERVER_NAME` when the
     scope is `RunScope::Run`.
   - Append a dated one-line pointer to D17.4 and to ADR-0006's 2026-08-28 amendment.
     Edit nothing else in either.
2. **The operator-surface denial becomes unconditional** (D30 point 2).
   - `forbidden_operations` appends `ForbiddenOperation::RimaiaToolSurface` itself, for
     every intent the runner builds.
   - `extra` keeps only caller-specific denials: `run_task` passes none, and the planner
     passes `PLANNER_FORBIDDEN`.
   - The surface is spelled at `MCP_SERVER_NAME` only.
   - The "A trap for task 021" doc comment becomes a pointer to D30.
   - `RIMAIA_TOOL_SURFACE` stays named as it is. ADR-0032 §6 refers to it, together
     with `rimaia_tool_surface`.
3. **`required_tools` are spelled at the handle's server** (D30 point 3).
   - `ClaudeProvider::plan_spawn` spells each entry with
     `self.tool_handle(handle.server, tool)`.
   - `tool_handle` normalises the server segment the way the CLI does: any character
     outside `[A-Za-z0-9_-]` becomes `_`.
   - `negotiate` refuses, on `RefusalAxis::HandleInjection`, an intent that has a
     non-empty `required_tools` and `rimaia_handle: None`.
   - The private shorthand hard-wired to `MCP_SERVER_NAME` survives only for
     `rimaia_tool_surface`.
4. **The planner, respelled** (D30 point 4). `--allowedTools
   mcp__rimaia-run__set_task_strategy`. `compose_strategy_prompt` and
   `compose_strategy_system_append` receive the `rimaia-run` spelling. The exact strings
   in these tests change to the new value and are **not** loosened:
   - `tests/prompt.rs`: `TOOL`, and the three prompt literals that embed it;
   - `tests/runner_strategy.rs`: `SET_TASK_STRATEGY_TOOL`, and the
     `config["mcpServers"]["rimaia"]` lookup;
   - the handle cases in `tests/runner_process.rs` and `tests/provider_seam.rs`.
5. **Grants say what they are for** (D30 point 5), in `crates/core/src/mcp/scope.rs`.
   - `Grant = Strategy | Review { run_id } | Fix { run_id }`, with `GrantKind` as its
     `Copy` discriminant.
   - `RunHandles::grant(task_id, Grant)`. Its table goes from token → task id to token →
     `(task id, Grant)`, and `resolve` hands both to the scope.
   - `RunScope::Run { task_id, grant }`.
   - **`RunGrant` and `Grant` are different things and both stay.** `RunGrant` is the
     token holder `grant` returns, whose `Drop` revokes the token; it does not change
     beyond its constructor's argument. `Grant` is what the token was minted for, and it
     is the only one of the two that access decisions read.
   - `Tool::run_access(self, GrantKind)` implements D30's table exactly. The planner's
     grant is `Grant::Strategy`, and its behaviour is unchanged.
   - `Tool::is_run_output(self)` is true for the two write tools below, and
     `RunScope::authorize` refuses them on `RunScope::Operator`. That is the first thing
     the operator's door has ever been refused.
   - **All six of task 034's tools are `Refused` for every `GrantKind`**: the three
     actions (`approve_task`, `reject_task`, `request_task_changes`), both reads
     (`get_task_dependents`, `get_review_digest`) and the marker write
     (`mark_review_digest_seen`). This is D30's "everything else" row and 034's Notes.
   - An implementation run still gets no grant and `rimaia_handle: None`.
6. **Recorded CLI facts** (D30 point 8). Two recordings in
   `crates/core/tests/fixtures/cli/` (D27.6), made against the CLI version the doctor
   accepts, captured the way `strategy-proposal.jsonl` was: from a shell, in a checkout
   `make-test-repo.sh` built, with `--strict-mcp-config --setting-sources project,local`
   and one `--mcp-config` document registering two stdio stand-in servers:
   - `rimaia`, which lists `get_task` and `set_task_strategy` and answers anything;
   - `rimaia-run`, which lists `set_task_strategy` and answers it.

   The stand-ins are not committed, as `strategy-proposal.jsonl`'s was not. The README
   section says what each one listed and answered, and gives each recording's exact argv,
   so a `MINIMUM_VERSION` bump can rebuild them.

   - **`run-scoped-server-name.jsonl` pins the denial.** Posture
     `--permission-mode bypassPermissions`, which is what an implementation run has and
     what 021's inheriting runs may have. `--disallowedTools mcp__rimaia`, the bare rule
     and nothing else. **No `--allowedTools`.** The run is told to call
     `mcp__rimaia__get_task` once and then `mcp__rimaia-run__set_task_strategy` once. Under
     `bypassPermissions` an unlisted call is approved, so a denial of the first call can
     only come from the bare rule, and a success of the second shows the rule did not match
     `rimaia-run` as a prefix. Leaving out `--allowedTools` is deliberate: with it, the
     second call's success could also mean "allowed overrides disallowed", which D30's Why
     calls unverified.
   - **`run-scoped-server-allowed.jsonl` pins the planner's shape.** Posture
     `--permission-mode acceptEdits`, `--allowedTools mcp__rimaia-run__set_task_strategy`,
     `--disallowedTools mcp__rimaia`. The run calls `mcp__rimaia-run__set_task_strategy`
     once. Under `acceptEdits` an unallowed MCP call is refused (the
     `strategy-proposal.jsonl` section records it), so a success shows that
     `--allowedTools` matches a hyphenated server segment. Without this the respelled
     planner could fall back on every run and nothing would fail.

   What each outcome means, and what the code does about it:

   | `run-scoped-server-name.jsonl` shows | Then |
   | --- | --- |
   | `rimaia__get_task` denied, `rimaia-run__…` succeeds | Exact match. `rimaia_tool_surface` keeps the bare entry. The test is `the_bare_server_rule_denies_its_own_server_and_not_rimaia_run` |
   | both denied | Prefix match. `rimaia_tool_surface` drops the bare `mcp__<name>` entry and keeps the per-tool entries, as D30 point 8 already decides. The test is `the_bare_server_rule_also_denies_rimaia_run` |
   | neither denied | The bare rule matches nothing, so tools outside `Tool::ALL` would be undenied. D30 does not cover this. **Stop and ask** |

   If `run-scoped-server-allowed.jsonl` shows the call refused, the planner cannot reach
   its tool under the new name. **Stop and ask**; do not ship the respelling.

   The fixtures' tests read the recordings: the tool calls, their results, and
   `result.permission_denials`. The README gains one section for both, saying what was
   faked, in the style of the `strategy-proposal.jsonl` section.

D30 point 6 (resolving inherited registrations by URL, and the `RIMAIA_` environment
strip) is task 055's. D30 point 7's `required_tools` for review and fix intents are task
021's, because no review or fix intent is built here.

### Three MCP tools, and one command (ADR-0021)

The handlers stay thin over `review::findings` (ADR-0006). Arguments go in
`mcp/requests.rs` and responses in `mcp/responses.rs`. Each gets a row in `Tool`, in
`Tool::ALL`, and in `run_access`. `Tool::ALL` grows by three from the count task 034 left
(54 as 034 is written, so 57). Tests iterate `Tool::ALL` or read `Tool::ALL.len()`, and
never assert a literal count.

| Tool | Arguments | Operator | `Strategy` | `Review` | `Fix` |
| --- | --- | --- | --- | --- | --- |
| `record_review_findings` | `task_id`, `findings: [NewReviewFinding]` | ✘ `is_run_output` | ✘ | own task; `review_run_id` is the grant's `run_id` | ✘ |
| `resolve_review_finding` | `task_id`, `finding_id`, `status: fixed \| rejected`, `resolution` | ✘ `is_run_output` | ✘ | ✘ | own task's open findings; `fix_run_id` is the grant's `run_id` |
| `list_review_findings` | `task_id`, `status?` | ✔ | ✘ | ✘ | ✘ |

- **A run id is never a request argument.** It comes from the grant, so a run cannot
  write under another run's id, even on its own task.
- `list_review_findings` is `Refused` to every grant. That is D30's "everything else" row,
  applied as written. A fix run is handed its findings in its prompt, which is task 021's
  `RunContext::review`, and does not go looking for them.
- `list_review_findings` also ships as a Tauri command in
  `src-tauri/src/commands/review.rs`, the module task 034 created for review commands,
  registered in both `generate_handler!` lists in `src-tauri/src/lib.rs`, with a
  `listReviewFindings` wrapper in `src/lib/commands.ts`. This way task 037 is only UI.
  The two write tools have no command: no UI writes a finding (D30 point 5), so ADR-0021's
  parity is not affected.
- **The wrapper gets its fixture row in the same commit** (task 028: every command
  `commands.ts` sends has a row, or `fixtures.test.ts`'s coverage test fails). The row in
  `src/dev/fixtures/` is a seeded answer typed `ReviewFinding[]` against `src/types.ts`,
  not a refusal, since the read has something to show. Every scenario answers `[]`, except
  `busy`, which answers two findings of one review run, one `open` and one `fixed` with its
  `resolution` and `resolvedByRunId` set, so the row exercises every field of the type
  rather than an empty array that type-checks against anything. No view calls the wrapper
  yet, so no screenshot changes. 037 seeds the states it renders.
- **D32's appendix gains one row in the same commit** (D32 point 8), under the dated
  "added after 728a049" sub-heading task 034 created: `list_review_findings`, module
  `review`, kind `board`, effect `Read`, From `046`, citing ADR-0021 point 3 and this task.
  The appendix's counts describe `main` at 728a049 and are left as they are.

## Out of scope

- **Spawning a review or fix run**, composing their prompts, `review_instructions` and
  `review_config` (the columns ship, nothing reads them), the loop budget and its exits,
  `NextStep::Continue`, and computing or matching `fingerprint`. All of these are task
  021's, as are the `Review` and `Fix` arms this task leaves refusing.
- **Every pixel.** That covers "Attempt N" becoming "Review · #4" in
  `RunHistorySection.tsx` and `RunDetailOverlay.tsx`, the card telling "reviewing" from
  "running", the Runs view's kind filter, the digest's loop line, and the findings list.
  All are task 037's.
- **The `BoardPort` trait** and moving `run_task`'s writes behind it (task 036).
  `LeasePurpose` is D31's, and this task does not add it.
- `runs::latest_successful_head` (D29 point 5, task 044).
- D30 point 6: `inherited_mcp_servers`, `OwnEndpoints::is_own`, alias denial and the
  `RIMAIA_` strip (task 055). The hosted server's mapping from lease purpose to grant
  (task 055).
- Registering `rimaia-run` anywhere in a user's configuration. Nothing ever does (D30
  point 1).
- A second migration file. If a column turns out to be missing, **stop and ask** (D4,
  and D28's D4 amendment).

## Acceptance criteria

**Schema**

- `20261001120100_run_kinds_and_review_findings.sql` matches D28 part 6 as amended on
  2026-09-30, including `runs.findings_recorded_at`, `review_findings.ordinal` and the
  unique `idx_review_findings_review_run`.
- `runs_recorded_before_kinds_existed_read_as_implementation` (`tests/store.rs`), on the
  precedent of `an_existing_row_backfills_to_ui`: after `test_pool`'s full migration, a
  run inserted by a statement that names no `kind`, standing in for every row written
  before the column, reads back as `Implementation`. The test also asserts that
  `idx_runs_task_attempt`'s `sql` in `sqlite_master` is byte-identical to the statement in
  the migration that created it. No partial-migration harness is built for this.
- `the_runs_table_refuses_a_strategy_kind`: an `INSERT` with `kind = 'strategy'` fails
  on the `CHECK`.
- `NewRun` has no `Default`, and `start_run`'s `INSERT` names `kind`. It is the only
  `INSERT INTO runs` in non-test code under `crates/core/src`. The `#[cfg(test)]` inserts
  in `runs/mod.rs` and `startup.rs` may rely on the default, as D29 point 1 says test
  fixtures do.
- `.sqlx/` is regenerated, and `cargo check --workspace --all-targets` passes with
  `SQLX_OFFLINE=true`.

**Kinds (D29's required tests, by these names)**

- `a_review_between_two_fix_phases_ends_the_first_fix_phases_budget`. Implementation rows
  are closed with `finish_run`; review and fix rows with `testing::runs::close_run` and
  `set_run_state`. `TestClock`, no `sleep`.
- `a_waiting_review_resumes_as_a_review_and_not_as_an_implementation`:
  - `resume_point` returns `Review`, and `resume_as_implementation` refuses it with
    `Error::invalid` naming 021;
  - one queue pass, with capacity for two, over that task and a second, ready task: the
    review task is still `waiting_retry` with its row and `resume_after` unchanged, no
    child is spawned for it, and the second task's implementation run is spawned in the
    same pass;
  - `retry_task_now` calls `resume_point` and `resume_as_implementation` before
    `claim_retry`. The shell has no test harness, so this ordering is read in review.
- `finish_run_refuses_a_review_row_and_writes_nothing`: the row is still open and the
  task's `run_state` is unchanged.
- `the_cards_last_run_is_the_newest_row_of_any_kind`. `list_tasks`' summary and
  `get_task`'s `last_run` both report the review's kind and status.
- `pruning_a_task_removes_its_review_and_fix_transcripts_too`. This uses real files in a
  `TempDir`.
- `implementation_and_review_loop_spend_sum_to_total_spend`. `outcomes`, `failure_rate`,
  `median_duration_seconds` and `strategies` are unchanged by adding review rows, and
  `review_loop_outcomes` counts them. `get_analytics` over MCP reports the same
  `review_loop_spend_usd` and `review_loop_outcomes` as the Tauri command.
- `a_fix_that_resumes_the_implementation_session_gets_its_own_budget`. A fix row that
  shares the implementation's `session_id`, with a review row between them, does not
  inherit the implementation's spent retries.
- `a_looped_task_is_one_digest_entry`: a task with an implementation, a review and a fix
  row is one entry, not three. Its `last_run_kind` is `Fix`, and its `review_loop` is
  `Some(DigestLoop { reviews_since_implementation: 1, open_findings: n })` for that task's
  `n` open findings. A task with only an implementation row has `review_loop: None`.
  `get_review_digest` over MCP reports the same two fields.
- `tests/runner_strategy.rs`'s assertion that the planner writes no `runs` row still
  passes unchanged.

**Findings**

- `a_clean_review_is_an_explicit_empty_record_call`: `record` with `findings: []` sets
  `findings_recorded_at` and writes no rows, and `recorded_at` returns it. A review row
  that never called has it `NULL`, and `recorded_at` returns `None`.
- `a_second_record_call_for_the_same_review_is_refused`. The refusal leaves the first
  call's rows and timestamp untouched.
- `findings_are_refused_from_a_run_that_is_not_a_running_review_of_this_task`: one case
  each for an implementation run, a finished review, and another task's review.
- `rejecting_a_finding_requires_a_reason`, and `a_resolved_finding_cannot_be_resolved_again`.
- `a_fix_run_cannot_resolve_another_tasks_finding`.
- `findings_list_in_review_order_then_in_the_order_the_reviewer_gave_them`, under a
  `TestClock` that returns one instant for every call, with each call's findings given in
  an order that neither their ids nor their titles would sort into.
- Deleting a task deletes its findings, and a finding's `resolved_by_run_id` becomes
  `NULL` if the fix run's row goes (D28's `ON DELETE` clauses, asserted rather than
  assumed).

**The handle (D30)**

- `an_implementation_run_is_still_denied_every_operator_tool`. The argv of an
  implementation run carries `mcp__rimaia__<tool>` for every tool in `Tool::ALL`, plus the
  bare `mcp__rimaia` unless `run-scoped-server-name.jsonl` showed a prefix match, and no
  `--mcp-config`. The expected argv is built from `Tool::ALL`, and compared as an exact
  string, not a substring.
- `the_planner_is_denied_the_operator_surface_too`. The planner's argv carries the same
  denial after `PLANNER_FORBIDDEN`'s patterns, and `--allowedTools
  mcp__rimaia-run__set_task_strategy`. Its `--mcp-config` document's `mcpServers` has
  exactly one key, `rimaia-run`.
- `no_intent_ever_denies_the_run_scoped_server`. `claude::spell_out` emits no
  `mcp__rimaia-run` pattern for any intent.
- `a_handle_named_for_the_operator_server_is_refused` (`tests/provider_seam.rs`):
  `negotiate` refuses an intent whose `rimaia_handle.server` is `MCP_SERVER_NAME`, on
  `HandleInjection`.
- `required_tools_without_a_handle_are_refused_on_handle_injection`
  (`tests/provider_seam.rs`): `negotiate` refuses a non-empty `required_tools` with
  `rimaia_handle: None`, on `HandleInjection`.
- `the_run_scoped_server_reports_its_own_name`. `get_info` answers `rimaia-run` on
  `/mcp/run/{token}` and `rimaia` on `/mcp`.
- `a_run_scoped_handle_reaches_only_its_own_task`, driven through the real handlers in
  `tests/mcp_scope.rs` for each of `Strategy`, `Review` and `Fix`:
  - `get_task` on another task is refused;
  - `record_review_findings` against another task is refused;
  - a `Review` grant's `record_review_findings` writes under the grant's `run_id`;
  - every tool D30's table marks ✘ for that grant is refused, including all six of 034's
    tools and `list_review_findings`.
- `the_operator_cannot_write_a_finding`. `record_review_findings` and
  `resolve_review_finding` on `/mcp` are refused with the same `{ code, message }`
  payload as every other refusal.
- `every_registered_tool_has_a_run_scope_decision` covers every `GrantKind`, and fails
  if a tool is added without a decision for each.
- Both fixtures' tests, named for the behaviour they pin, pass from the recordings, and
  `crates/core/tests/fixtures/cli/README.md` describes both recordings, their argv and
  their stand-ins.
- The "A trap for task 021" doc comment is gone, and `runner/strategy.rs`'s header no
  longer gives the missing migration as a reason.

**Every door**

- `list_review_findings` returns the same findings, in the same order, through the MCP
  tool and through the Tauri command. `./scripts/check-command-wiring.sh` passes with the
  new command.
- `src/dev/fixtures/` has a `list_review_findings` row, a seeded answer and not a refusal,
  and task 028's `it("has an answer or an explicit refusal for every command commands.ts
  sends")` and `it("never reaches invoke or listen in fixture mode")` pass with the new
  wrapper, without changes to either test. `busy`'s answer holds one `open` and one `fixed`
  finding; the seed's existing `Run`, `LastRunSummary`, `RunListEntry`, analytics and
  digest objects carry the new fields, so `npm run typecheck` compiles the seed against
  them.
- D32's appendix has the `list_review_findings` row under the "added after 728a049"
  sub-heading.
- `src/types.ts` carries `RunKind`, the `kind` fields, `ReviewFinding`,
  `FindingSeverity`, `FindingStatus`, `DigestLoop`, the two new `DigestEntry` fields and
  the new analytics fields. No component in `src/` reads any of them.

**CI**

- Every CI check passes: `npm run typecheck`, `npm run test`, `npm run build`,
  `cargo test -p rimaia-core`, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`.
  CLAUDE.md and `ci.yml` are unchanged, because this task adds no crate and no dependency.

## Notes

**Seam entries to read.** Read D29 and D30 in full, since they are this task's
specification. Then:
- D28: part 5's rules for additive files, part 6's DDL for this file, the D4 amendment's
  freezing rule, and the 2026-09-30 amendment adding this file's two columns;
- D31 points 2, 4 and 6: `record_review_findings`, `StartRun::kind` and `Claim::resume`,
  which are what 036 builds on this task's types;
- D32 point 8 and its appendix, for the one command this task adds;
- D33, which leaves this task on D5's recipe;
- D5 for `.sqlx`, D8 for refusals, D10 for ids, D12 and D23 point 7 (amended here), D16.1
  for tool argument spelling, D17.4 and D17.9 (the planner's handle and its
  store-witnessed call), D18 (capture columns on review rows), and D19 (the slot that
  makes reading the resume point before the claim safe);
- D27.2 to D27.6: the provider seam, `tool_handle`, and fixture directories.

`docs/seam-contract.md`'s "How to use this" table has no row for 035 yet. Add one, listing
D4 · D5 · D8 · D10 · D12 · D16 · D17 · D18 · D19 · D23 · D27 · D28 · D29 · D30 · D31 ·
D32 · D33.

**ADRs.** ADR-0017 is the loop this prepares. ADR-0021 point 3 is why every new tool
needs a scope decision, and its parity rule is why the analytics and digest fields go
through MCP too. ADR-0022 is why analytics has to say which kinds it counts. ADR-0026
point 4 covers denials as operations and `tool_handle` as the provider's spelling.
ADR-0006's 2026-08-28 amendment and ADR-0016 are the scoped route and its threat model;
this task appends a pointer to the first. ADR-0011 is the retry budget D29 point 3 bounds
by kind, and the `running → waiting_retry` edge the test helper takes.

**Files to start from.** These paths are verified on `main` @ 728a049; line numbers are
from D29's inventory and will have drifted by 033 and 034.
- The D29 inventory table lists every reader. Work down it, and do not grep for `runs`
  instead: the indirect readers under the table are the ones a grep misses. 034's
  digest query is a reader the table predates; it takes every kind, newest row first.
- `crates/core/src/runner/outcome.rs`: `NewRun`, `start_run`, `finish_run`,
  `apply_to_task`, `fetch_run_row`, `observed_run_cost`.
- `crates/core/src/scheduler/attempts.rs`: `attempt_rows`, `fold`, `resumable_session`.
  Also its re-export in `scheduler/mod.rs`, and its callers `scheduler/queue.rs`
  (`try_step`, the `resuming` branch after the leases are taken) and
  `src-tauri/src/commands/runs.rs` (`retry_task_now`).
- `crates/core/src/tasks/service.rs`: `TASK_SUMMARY_SELECT`, `fetch_last_run`.
- `crates/core/src/runs/mod.rs`: `RunFilter`, `list_runs`, `prune_logs`.
- `crates/core/src/analytics/mod.rs`: `runs_in`, and the report struct at
  `implementation_spend_usd`. `mcp/responses.rs`: `AnalyticsView`.
- `crates/core/src/review/digest.rs` (034): the newest-row read and `DigestEntry`.
- `crates/core/src/mcp/mod.rs` (`MCP_SERVER_NAME`), `mcp/scope.rs` (`RunScope`,
  `Tool::ALL`, `run_access`, `authorize`, `RunHandles::grant`, `RunGrant`, `resolve`),
  `mcp/server.rs` (`get_info`, the handlers), `mcp/requests.rs` and `mcp/responses.rs`
  (`RunView`).
- `crates/core/src/runner/process.rs`: `RIMAIA_TOOL_SURFACE` and its trap comment,
  `forbidden_operations`.
- `crates/core/src/runner/provider/claude.rs`: `tool_handle`, `rimaia_tool_surface`,
  `plan_spawn`'s `required_tools` loop, and the `mcpServers` document.
- `crates/core/src/runner/provider/mod.rs`: `negotiate`. `provider/intent.rs`:
  `RimaiaHandle`.
- `crates/core/src/runner/strategy.rs`: the header, and the two `MCP_SERVER_NAME` sites.
- `crates/core/src/testing/`: a new `runs.rs` for `close_run`.
- Tests: `tests/mcp_scope.rs`, `tests/mcp_tools.rs`, `tests/prompt.rs`,
  `tests/runner_strategy.rs`, `tests/runner_process.rs`, `tests/provider_seam.rs`,
  `tests/analytics.rs`, `tests/store.rs`, `tests/scheduler.rs`, and 034's review tests.

**The recorded fixtures are not edited.** `strategy-proposal.jsonl` calls
`mcp__rimaia__set_task_strategy`, because that was the name when it was recorded. It stays
byte-identical. The test that replays it asserts argv and the store, not the tool name in
the stream. If some test turns out to read that name out of the stream, stop and ask
rather than editing the recording.

**What the previous tasks provide.**
- Task 028 provides the fixture mode, its typed seed in `src/dev/fixtures/` and the
  coverage test that fails on a wrapper with no row. Its rule binds every task that adds a
  command after it; this task adds one row, and changes no test of 028's.
- Task 033 provides `runs.head_sha`, `runs.base_sha` and `review_bundles`, and the
  `20261001120000` file this one sorts after. A review row's `head_sha` is written by the
  same `finish_run` code as an implementation's (D29 point 5), so this task adds nothing
  there. 033 may also have added columns to `Run` and its explicit `query_as!` lists; add
  `kind` beside them.
- Task 034 provides `crates/core/src/review/` with its digest, the six review tools this
  task keeps `Refused` for every grant, `src-tauri/src/commands/review.rs`, and the "added
  after 728a049" sub-heading in D32's appendix.

**What the next tasks expect.**
- **036** puts `start_run`, `finish_run`, the claim's `resume` and
  `record_review_findings` behind `BoardPort`. It expects `RunKind`, `ResumePoint` and
  `NewReviewFinding` to exist with the names above, and `review::findings::record` to be
  the one function its in-process adapter calls. It keeps this task's order: the
  in-process `claim` reads `resume_point` and applies `resume_as_implementation` before
  `claim_retry`, so a refused resume is refused before anything is claimed and there is
  nothing to release.
- **021** wires the `Review` and `Fix` arms at both resume callers and in `finish_run`,
  mints `Grant::Review` and `Grant::Fix`, gives each intent its
  `required_tools` (D30 point 7), reads `review::findings::recorded_at` to tell a clean
  review from a failed one, and decides `fingerprint`.
- **037** renders every `kind` field, the kind filter, the digest's loop fields and
  `list_review_findings`.
- **038** redeclares `runs.kind`, `runs.findings_recorded_at` and `idx_runs_task_kind` in
  the rebuild. D28 part 6's 038 block already does.

No later task copies `review_findings`. Task 051's copy to a team carries the title, plan
and extra instructions only, and leaves findings behind with the rest of the task's
history (ADR-0029 point 5). D28's 2026-09-30 amendment said otherwise until Phase 0
corrected its example; `ordinal` is still needed, for the reasons the amendment gives.

**Size.** This is an L, and at the top of what one session carries: roughly 3.5–4k lines,
about half of which is tests and exact-string updates. It is not split, because the second
half cannot ship without the first's store, and the first cannot ship its tools against the
old server name: a review grant served as `rimaia` is exactly the trap this task exists to
close. Land the halves as separate runs of commits (Scope, top), so the diff reads as two
reviews. Do not stop at a line count; the only stops are the ones named above (the two
fixture outcomes D30 does not cover, and a missing column).
