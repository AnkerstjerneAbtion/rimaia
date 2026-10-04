---
id: "037"
title: The review loop in the interface
milestone: v0.4
status: ready
depends_on: ["021"]
adrs: ["0017", "0013", "0021", "0022", "0024"]
size: M
---

# The review loop in the interface

## Goal

Make task 021's review-and-fix loop readable and controllable from the app. The morning
should open on "here is what the reviewer could not fix", not on a loop that ran out of
sight. Concretely:

- every surface says which **kind** of run it is showing;
- the task carries its **review history**: each loop's findings, what was fixed, what was
  rejected and why, and what is still open;
- the run view and the morning review show the **final diff plus the unresolved findings**;
- the loop's verdict, and a loop that **may be going in circles**, are signals on the card
  rather than something the budget quietly absorbed;
- `review_instructions` and the loop configuration can be edited at every level, and
  **turning the loop on requires acknowledging what it costs**, with the cost stated in
  dollars measured on this installation.

This task is the interface half of ADR-0017. It adds no engine behaviour. What a review
does, when a fix runs, where a finished loop leaves the task, the verdict, which finding
blocks, the ping-pong rule and the acknowledgement's spelling are all task 021's, and this
task renders them.

## Why now

Task 021 runs the loop and ships no screen for it, by design: the engine was split from its
interface so it could be written against task 036's board port without also carrying a
frontend. Until this task lands, a loop's findings sit in `review_findings` and nobody
reads them. That is the exact failure ADR-0017 names first, **false confidence**: a card in
`in_review` with no visible sign that a reviewer found three problems and a fixer resolved
one.

It also lands before team mode starts moving the board (task 038 onwards). The editors built
here are the ones task 061 later extends with consent for `review_instructions`. Extending a
finished screen is cheaper than designing two halves of one at once.

## Scope

**1. The run's kind, everywhere D29 lists.** Seam-contract D29 point 9 puts `kind` on
`Run`, `LastRunSummary` and `RunListEntry` (task 035), and names this task as the one that
renders it in `TaskCard`, `RunStateBadge`, `RunHistorySection`, `RunDetailOverlay`,
`RunsView` and `AnalyticsView`. This task adds two surfaces D29 did not list, because each
shows a review row as if it were an implementation: the panel's `RunOutcomeSection` and the
Runs view's `ActiveRunCard`.

- **The badge.** `cardBadge` in `src/lib/board.ts` is unchanged: it still returns a state
  key, because that key also feeds the `run-badge-${badge}` class, the card rail
  (`cardRunState` in `TaskCard.tsx`) and `Column.tsx`'s header counts. The words move out
  of `LABELS` in `RunStateBadge.tsx` into a new pure `badgeLabel(badge, kind)` in
  `src/lib/board.ts`, which `RunStateBadge` renders. `RunStateBadge`'s structural `lastRun`
  type gains `kind`. An implementation row keeps today's labels unchanged. A review or fix
  row reads:

  | Badge | `review` | `fix` |
  | --- | --- | --- |
  | `running` | Reviewing | Fixing |
  | `waiting_retry` | Review waiting for retry | Fix waiting for retry |
  | `interrupted` | Review interrupted | Fix interrupted |
  | `failed` | Review failed | Fix failed |
  | `cancelled` | Review cancelled | Fix cancelled |

  Only the first three rows are reachable today. 021's exits table lands a failed, fatal
  or cancelled review or fix, and one whose retry budget ran out, in `in_review` with
  `idle`, and `cardBadge` returns `null` for `idle`. The verdict line (`Not reviewed — the
  review run failed`) reports that case instead. The `failed` and `cancelled` rows exist so
  `badgeLabel` is total over its inputs; they are not seeded as card states (Scope 7).
  `interrupted` is reachable through crash reconciliation, which reads it off the run (D9).

  `queued` and `blocked` do not depend on kind, because no row exists yet. This is D29
  point 4's "the card can tell reviewing from running". The dot and the word stay together
  (ADR-0024 rule 3), and the resume time on `waiting_retry` is kept.
- **"Attempt" retires** (D29 point 2). `RunHistorySection` and `RunDetailOverlay` render
  the kind and the number, for example `Review · #4`, with a header of
  `Run detail — Review · #4`. The kind labels `Implementation`, `Review` and `Fix` live in
  one exported map.
- **The panel's last outcome.** After a loop, the newest row is usually a review, and
  `RunOutcomeSection` shows it as the task's last outcome. Its heading names the row:
  `Last run outcome — Review · #7`.
- **The Runs view** gets a kind control beside its status filter, with the options
  `All kinds`, `Implementation`, `Review` and `Fix`. It maps to `RunFilter::kind` (task
  035). `All kinds` omits the field. Each row shows its kind label, and so does
  `ActiveRunCard`, beside the task title, so a running review does not look like an
  implementation.
- **Analytics** renders task 035's `reviewLoopSpendUsd` and `reviewLoopOutcomes` as a
  "Review loops" group, in the ADR-0022 view. The group appears only when the range
  contains a review or fix row. The outcome, failure-rate and median figures are labelled
  as implementation runs, because D29 point 7 made them that. The invariant
  `spend = implementation + review loops` is asserted in Rust by 035, and the view does not
  recompute it.

**2. The loop on the card: a D12 amendment.** Task 021 gives `TaskDetail` a
`review_loop: Option<ReviewLoopSummary>` holding:

- the effective `enabled` and `max_review_loops`;
- the fixes spent;
- the verdict: `Clean`, `FindingsRemain { open_blocking }` or `Unreviewed { reason }`;
- `open_blocking`;
- `ping_pong`.

It is derived from rows and never stored. This task puts the **same field, built by the
same function**, on `TaskSummary`, so the card and the panel cannot disagree about a loop.

The verdict is a Rust function over rows, findings and configuration, not a SQL
expression. So the summary cannot be one correlated subquery, and a SQL copy of 021's rule
would be a second implementation that drifts. The read therefore works like this:

- `list_tasks` runs `TASK_SUMMARY_SELECT` as today, with the task's own `review_config`
  column added to it;
- **one** read of the global review settings;
- **one** batched read of the listed tasks' repositories' configuration:
  `SELECT id, review_config FROM repositories WHERE id IN (…)`, or the column joined into
  `TASK_SUMMARY_SELECT`;
- **one** batched read of the runs rows 021's verdict needs and **one** of their findings,
  each keyed by task id. They cover **every** listed task, not only tasks with loop rows,
  because 021's `not_reviewed` case is a succeeded implementation with no loop rows after
  it, read against the current configuration;
- then 021's resolution and verdict per task, in memory.

That is a fixed number of statements per board read, whatever the number of cards and
whatever the number of repositories. D12's argument was against N+1, not against a second
statement, and its 2026-08-28 amendment already added settings reads on the same
reasoning. Do not copy `apply_effective_strategy` in `crates/core/src/tasks/service.rs`
here: it calls `defaults_for_repository` once per distinct repository, which is N+1 over
repositories. The amendment this task writes says so.

This only works if 021's verdict is a pure function. 021 says `review_loop::verdict` "reads
the current loop's rows and findings", which leaves open whether it queries on its own. If
`verdict` or the `ReviewLoopSummary` builder takes a `ServiceContext` and runs its own
statements, split it into a loader and a pure function over the loaded rows, findings and
configuration. `get_task` then calls the same pure function with its own single-task load,
and 021's tests keep passing against the pure function.

The card renders one line from a pure function, `reviewLoopText`, added to the
`src/lib/review.ts` that task 017 created, using exact strings. The prefix counts fixes,
because `max_review_loops` counts fixes (021):

| Verdict | Text |
| --- | --- |
| `None`, or `review_loop` absent | no line (`reviewLoopText` returns `null`) |
| `Clean`, 0 fixes | `Reviewed once · nothing blocking` |
| `Clean`, 2 fixes | `Reviewed after 2 fixes · nothing blocking` |
| `FindingsRemain { 1 }`, 0 fixes | `Reviewed once · 1 blocking finding open` |
| `FindingsRemain { 3 }`, 2 fixes | `Reviewed after 2 fixes · 3 blocking findings open` |
| `Unreviewed { not_reviewed }` | `Not reviewed` |
| `Unreviewed { review_failed }` | `Not reviewed — the review run failed` |
| `Unreviewed { nothing_recorded }` | `Not reviewed — the reviewer recorded nothing` |
| `Unreviewed { review_changed_branch }` | `Not reviewed — the reviewer changed the branch` |
| `Unreviewed { fix_not_reviewed }` | `Not reviewed since the last fix` |

Singular forms are `1 fix` and `1 blocking finding`. When `ping_pong` is true, a second
element reads `May be going in circles`. That is 021's wording, chosen because a fresh
reviewer may simply have noticed something the first one missed, and the card must not
claim the fix broke it.

The first row covers a task with the loop effectively on that has never been implemented,
or whose newest implementation did not succeed: there are no loop rows and no succeeded
implementation to be unreviewed, so whatever 021 returns for it (`None`, or no summary at
all), the card renders no line.

- **No tick, no "clean", no "passed", no success colour.** ADR-0017 asks for "loop count
  and findings history rather than a green tick". A loop with nothing blocking is still a
  statement about one reviewer's pass, not a verdict on the work.
- **Only a finished loop has a line.** The rule is on `runState`, not on the badge's words:
  no loop line while `runState` is `queued`, `running` or `waiting_retry`, whatever the
  last row's kind. That covers an implementation re-run (`Running`), a review or fix in
  flight, and the window between phases, when `runState` is `running` and the newest row
  is a finished review. A verdict on a loop in progress describes an unfinished pass, and
  the badge already says what is happening.
- **One wording, three surfaces.** The same function feeds 017's digest entry and 017's
  review item. Task 035 put a loop summary on the digest entry (D29 point 8), and 021
  extended it to `ReviewLoopSummary` rather than writing a second one.

**3. The review history on the task.** This part adds a command and an MCP tool,
`get_review_history(task_id)`, as thin adapters over 021's `review_loop::history`
(ADR-0021 parity, ADR-0006). The command goes in `src-tauri/src/commands/review.rs`, beside
021's four configuration commands. The tool is refused to every grant, which is D30 point
5's "everything else" row.

A new `ReviewHistorySection` in the task panel (`src/components/panel/`) renders it, placed
above `RunHistorySection`, and only when the history is non-empty.

- **The grouping is 021's, not the view's.** D29 point 6 expected the view to group loops
  from `list_runs_for_task`. Task 021 then placed grouping in core, where its phase rule
  lives: contiguous rows sharing `(kind, session_id)`, so a review retried after a usage
  limit counts once. Render `ReviewHistory` as returned and **do not group again in
  TypeScript**. This task appends a dated one-line note to D29 point 6 saying so, rather
  than deviating from it silently.
- **Blocking is 021's, not the view's.** 021 defines a blocking finding as
  `severity >= blocking_severity` against the effective configuration. The view reads a
  per-finding `blocking` flag and never compares severities (see Notes if 021 did not ship
  the flag).
- **Each review phase** shows its run(s) as `Review · #4` and its findings. Each finding
  shows:
  - its severity, as a word;
  - its title;
  - its location as copyable monospace `file:line` (ADR-0024 rule 2);
  - its body, on expansion;
  - its status: `Open`, `Fixed in #5`, or `Rejected in #5 — <resolution>`. A finding
    021 carried over as already rejected shows its stored resolution ("Rejected earlier as
    …") verbatim.

  A rejection's reason is always visible and never behind a disclosure. An open finding
  whose `blocking` flag is false is marked `Advisory`, because it did not start a fix
  (021).
- **Ping-pong, per phase.** 021 reports `regressed` and `new_after_fix` for each review
  after the first. The findings in them are marked `Came back after a fix` and
  `New after a fix`, respectively.
- **The head of the section** states what remains in the current loop. It comes from a
  pure `reviewHistoryHeadText` in `src/lib/review.ts`, fed by core's counts
  `open_blocking` and `open_advisory`, never by counting findings in TypeScript. It uses
  the card's prefix. Exact strings:

  | Current loop | Text |
  | --- | --- |
  | 3 blocking, 1 advisory, 2 fixes | `Reviewed after 2 fixes · 3 blocking findings open, 1 advisory` |
  | 1 blocking, 0 advisory, 0 fixes | `Reviewed once · 1 blocking finding open` |
  | 0 blocking, 2 advisory, 0 fixes | `Reviewed once · nothing blocking, 2 advisory` |
  | 0 blocking, 0 advisory, 1 fix | `Reviewed after 1 fix · nothing open` |
  | `Unreviewed { reason }` | the card's text for that reason, unchanged |

- **Earlier loops**, which 021 marks as earlier (before a re-run implementation), are
  collapsed by default.
- **Reloading.** The section reloads when a finding is recorded or resolved and when a
  run of the task changes. It subscribes through `src/lib/events.ts` (D7) to the events
  035's writers and `finish_run` publish, and a test fires them.

The operator **cannot** mark, dismiss or reject a finding from the interface. D30 point 5
refuses the findings writers on `RunScope::Operator` ("a finding the operator wrote would
look exactly like a reviewer's in the morning"), and no command writes one. A human who
disagrees says so through 017's "needs changes" note, which is where a human's instruction
to the next run belongs.

**4. The final diff plus the unresolved findings.** This is ADR-0017's "what the morning
sees".

- **`RunDetailOverlay`** for a task's **newest** row shows the current loop's open
  findings, those flagged `blocking` first, then advisory. The diff is 033's bundle of that
  row, which is the branch as it actually is (D29 point 5). For an older review row, the
  overlay lists the findings that review raised, each with its current status. Both come
  from `get_review_history`.
- **017's morning review item** shows the same list through the same component,
  `OpenFindingsList`, in the slot 017 left for it after the PR link.
- **Each surface keeps its own order**, because they differ: 017's review item has no
  prompt or transcript, and its last section is the patch.
  - `ReviewView`: outcome with the verdict line, diff summary, commits, PR link,
    `OpenFindingsList`, patch.
  - `RunDetailOverlay`: outcome with the verdict line, diff, commits, PR link,
    `OpenFindingsList`, prompt, transcript.

  In both, the list sits directly after the PR link. ADR-0013's order stays whole, and a
  finding's `file:line` is read against the diff that sits above it. The loop's verdict
  line from Scope 2 joins the outcome block, so the human still "starts from" what the
  reviewer could not fix (ADR-0017) before scrolling to the list.

**5. Configuration, at the task and repository levels ADR-0017 names, plus the global
level 021 added.** The fields are 021's `ReviewConfig`:

- `enabled`;
- `max_review_loops` (`0..=5`);
- `blocking_severity`;
- `review_model` and `review_effort`;
- `fix_session` (`fresh` or `resume`).

Every field is optional and inherits when absent, field by field: task, then repository,
then global, then the built-in defaults. The doors are 021's four commands:
`get_review_settings`, `set_review_settings`, `set_repository_review_config` and
`set_task_review`. This task writes their wrappers in `src/lib/commands.ts`, which 021
left to it. If 021 cut them under its own size rule, this task builds them first (Notes).

- **Settings → Review**, a new `src/views/settings/ReviewSection.tsx`:
  - the global `review_instructions` textarea, saved the way `InstructionsSection` saves
    base instructions;
  - the global value of every `ReviewConfig` field.
- **Per repository**, a `ReviewConfigFields` component inside `RepositoriesSection`, in the
  shape of `OnArchiveFields` and `StrategyDefaultsFields`.
- **Per task**, a `ReviewLoopSection` in the panel, holding:
  - the task's `review_instructions` override, in an editor shaped like
    `ExtraInstructionsEditor`, with the sentence `Replaces the global review instructions
    for this task. Leave empty to use them.` (021's override-replaces rule);
  - the same `ReviewConfigFields`.
- **Inherit.** At the repository and task levels, every field offers
  `Inherit (<inherited value>)`. The inherited value is what the level above resolves to,
  which is what the field becomes if this level stops setting it. It comes from the
  backend's resolution and never from a TypeScript copy of the precedence chain, for D12's
  2026-08-28 reason.
- **Model and effort** offer the vocabulary `StrategyDefaultsFields` offers, which is task
  020's catalogue (D17). `max_review_loops` offers `0` to `5` and nothing else. `0` is
  labelled `Review only, no fixes`.
- **The stored value wins over the form.** Another window, or an MCP client, is a
  supported writer (ADR-0006), as `OnArchiveFields` documents.

**6. Turning the loop on: the acknowledgement, and what it costs.** 021 made the
acknowledgement a spelling, following D20's `on_done_acknowledged`. The only "on" value of
`enabled` is `"on_cost_acknowledged"`, and every door refuses `true` or `"on"`. The rule and
its core tests are 021's. This task builds the moment the user gives it.

- **The control.** The global `enabled` is a checkbox, `Review each task after it is
  implemented`. At the repository and task levels it is three radios: `Inherit (…)`,
  `Off`, `On`.
- **Any choice that makes the loop effectively on at that level does not write.** That is
  checking the global box, choosing `On`, and choosing `Inherit` when the inherited value
  is on (a task that was explicitly `Off` under a repository or global that is on). Each
  opens an inline confirmation in the pattern of `OnArchiveFields`' confirm block (not a
  modal). The block states the cost and offers `Turn on review loop` and `Cancel`. Only the
  first writes: `enabled: "on_cost_acknowledged"` for `On` and the global box, and the
  field cleared for `Inherit`. Until then the control shows the stored value. `Cancel`
  writes nothing.

  The acknowledgement recorded at the inherited level was given for that level's scope,
  not as a promise about a task someone later singled out as `Off`. Returning such a task
  to the loop is a new decision to spend, so it gets the same moment.
- **Any choice that leaves the loop effectively off writes at once**: unchecking the
  global box, choosing `Off`, and choosing `Inherit` when the inherited value is off.
  Nothing about those costs more. This is the asymmetry `StorageSection` applies to
  `worktree_auto_cleanup`.
- **The sentence** comes from a pure `reviewLoopCostNote(maxReviewLoops, summary)` in
  `src/lib/review.ts`, modelled on `environmentOverheadNote` in
  `src/lib/runEnvironment.ts`. It is fed by the existing `get_run_cost_summary`, whose
  median is `observed_run_cost` over runs of every kind (D29 point 6). With N fix loops, a
  task runs at most `2N + 1` more sessions before retries: one review, then a fix and a
  review per loop (021). Exact strings:
  - N = 2, median $0.84 over 12 runs: `With up to 2 fix loops, each task runs up to 5 more
    sessions: a review, then a fix and another review per loop. At your median run so far
    ($0.84 across 12 runs), that is up to about $4.20 more per task.`
  - N = 0, same median: `With no fix loops, each task runs 1 more session: a review that
    reports findings and fixes nothing. At your median run so far ($0.84 across 12 runs),
    that is about $0.84 more per task.`
  - No median yet: the first sentence, then `There is no finished run with a cost yet to
    put a price on that.`
  - Singular forms: `1 fix loop` and `1 run`.

  N is the `max_review_loops` that will be **effective** at that level once the loop is
  on. The note never states a figure it did not measure, which is `runEnvironment.ts`'s
  rule.

**7. The fixture seed and the screenshots.** Task 028's dev fixture mode gains the states
this task renders. Its fixture table in `src/dev/fixtures/` answers `get_review_history`
and 021's four commands, typed against `src/types.ts` as 028 requires. Every seeded card is
one a real loop can produce: no `Review failed` or `Review cancelled` badge (Scope 1).

- **Cards:** one per verdict row in Scope 2's table (at least `Clean`, `FindingsRemain`,
  `review_failed` and `fix_not_reviewed`), one with `May be going in circles`, one whose
  badge reads `Reviewing`, and one whose badge reads `Review waiting for retry`.
- **The task panel:** Scope 3's two-loop history (the acceptance fixture below), containing
  an open, an advisory, a fixed, a rejected, a carried-over rejection, and a came-back
  finding.
- **The overlay:** a newest review row with open findings between its bundle diff and its
  prompt.
- **Views:** the Runs view filtered to `Review` with a running review in `ActiveRunCard`,
  and Analytics with the review-loop group.
- **Settings:** Settings → Review with the cost confirmation open, and a repository's
  review fields showing `Inherit (…)`.

028 reaches a view by clicking the sidebar, and 017 added an optional non-mutating key
sequence per row. These states also need a click after the sidebar. This task adds an
optional **click list** to the same table in `screenshots/views.shot.ts`: each click names
an element by accessible role and name, runs after the sidebar click and before any keys,
and is never a control that writes. The seed is one, exported under one scenario name per
capture, so 028's `<scenario>--<view>--<scheme>--<width>.png` names stay unique (for
example `review-loop-panel--board--dark--1440.png`):

| Scenario | View | Clicks | State captured |
| --- | --- | --- | --- |
| `review-loop` | board | — | The cards above |
| `review-loop-panel` | board | the history card | The task panel with its review history |
| `review-loop-overlay` | board | the history card, then its newest run in `RunHistorySection` | The overlay with open findings |
| `review-loop-runs` | runs | the kind control, then `Review` | The Runs view filtered to reviews |
| `review-loop-analytics` | analytics | — | The review-loop group |
| `review-loop-settings` | settings | `Review each task after it is implemented` | Settings → Review with the confirmation open |
| `review-loop-repository` | settings | — | A repository's fields showing `Inherit (…)` |

Checking the global box writes nothing (Scope 6), and opening a card, opening a run and
choosing a filter are reads, so every click above qualifies under 028's rule.

`npm run screenshot` covers each of these. The implementing run looks at the screenshots
before it finishes, as 028 requires: contrast, overflow, wrapping, and whether open,
advisory, fixed, rejected and came-back can be told apart **without colour**.

## Out of scope

- **Everything the loop does.** Phases, prompts, the budget, the exits, the verdict, which
  finding blocks, the fingerprint, the ping-pong rule and `finish_run`'s `Continue` are
  021's. So are the storage of the configuration and its refusal of an unacknowledged
  "on".
- **Findings written or edited by a human** (Scope 3, D30 point 5).
- **A preview of the composed review prompt.** If 021 extended `preview_composed_prompt`
  to review intents, show it in `ReviewSection` the way `InstructionsSection` shows its
  preview. Otherwise it is not this task's to build.
- **Re-running only the review.** 021 decided that Run now starts from implementation.
- **Kind in `RunInfoSection`'s `Last run` row.** `RunOutcomeSection`'s heading names the
  row, one section above; saying it twice adds nothing.
- **Consent for `review_instructions`** (ADR-0032 point 3). Tasks 045 and 061 add
  revisions and acceptances to the editors built here.
- **The runner's model and effort ceiling** on the review phase (045, 061).
- **Notifications**, and any change to column semantics. The loop never sends a card to
  `done`, and nothing here does either.
- **Any migration, and any new npm or Cargo dependency** (D4, D6, and D34's list, which
  adds nothing this task needs).

## Acceptance criteria

- **Kind on every surface.**
  - `cardBadge`'s return values are unchanged, and its existing tests pass unedited.
  - `badgeLabel(badge, kind)` returns every cell of Scope 1's table for review and fix
    rows, and today's labels for implementation rows. `src/lib/board.test.ts` asserts every
    cell as an exact string. `RunStateBadge` renders `badgeLabel` and holds no label map of
    its own.
  - No user-visible `Attempt` or `attempt N` remains in `RunHistorySection` or
    `RunDetailOverlay`. Both render `<Kind> · #<attempt>`.
  - `RunOutcomeSection.test.tsx` finds `Last run outcome — Review · #7` for a review row,
    and `ActiveRunCard.test.tsx` finds the `Review` label on a running review.
  - The Runs view's kind control calls `list_runs` with `kind: "review"` when `Review` is
    chosen, and with no `kind` field for `All kinds`. The test in
    `src/views/RunsView.test.tsx` goes through the mocked `@tauri-apps/api/core`, never by
    mocking `commands.ts`.
  - `AnalyticsView` renders the review-loop group only when the range contains a review
    or fix row. A test covers both cases.
- **The summary.**
  - `TaskSummary.review_loop` and its TypeScript mirror exist.
  - The loop inputs for all listed tasks are read by batched statements: one global
    settings read, one repositories read keyed by id (or a join), and the runs and findings
    reads keyed by task id. No statement runs inside a per-task or per-repository loop, and
    a reviewer can check that in the diff.
  - `get_task` and `list_tasks` build `review_loop` with the same pure function.
  - Tests in `crates/core/tests/tasks.rs`:
    - `the_board_summary_and_get_task_agree_on_the_review_loop`, across every verdict
      021's tests produce;
    - `a_task_with_the_loop_off_and_no_loop_rows_has_no_review_loop_on_the_card`;
    - `a_task_whose_loop_turned_on_after_it_was_implemented_reads_not_reviewed`;
    - `a_loop_on_task_that_was_never_implemented_has_no_verdict_on_the_card`, for a task
      in `ready` with no runs;
    - `a_board_mixing_tasks_with_and_without_loops_keeps_each_tasks_own_summary`, which
      catches a batched read keyed by the wrong id;
    - `a_board_across_repositories_reads_each_repositorys_review_config_once`, for two
      repositories with different configurations.
- **The card.**
  - `reviewLoopText` returns every string in Scope 2's table exactly, including the
    singulars, and `null` for a `None` verdict and an absent summary. Matched
    case-insensitively, it never returns text containing `✓`, `✔`, `clean` or `passed`.
  - `TaskCard.test.tsx` covers:
    - a ping-pong card is found by `May be going in circles`;
    - no loop line while `runState` is `queued`, `running` or `waiting_retry`, with the
      badge reading `Queued`, `Running` (an implementation re-run), `Reviewing`, `Fixing`
      and `Review waiting for retry`;
    - a loop-on task in `ready` with no runs renders no loop line;
    - no element inside any rendered card has a class matching `/success/`.
- **Blocking stays in core.**
  - Every finding in `ReviewHistory` carries `blocking`, and `ReviewLoopSummary` and the
    history's current loop carry `open_advisory` beside `open_blocking`, all computed in
    rimaia-core against the effective `blocking_severity`.
  - No function in `src/` compares `FindingSeverity` values, orders severities, or reads
    `blocking_severity` to classify a finding, and no function in `src/` counts open
    findings. A reviewer can check both in the diff.
- **The history read.**
  - `get_review_history` exists as a Tauri command in `src-tauri/src/commands/review.rs`,
    a `commands.ts` wrapper and an MCP tool. Both doors reach `review_loop::history`,
    shown by a parity test.
  - `crates/core/tests/mcp_scope.rs` has
    `getting_review_history_is_refused_to_every_run_grant`.
  - `crates/core/tests/mcp_tools.rs` has
    `the_operator_reads_a_tasks_review_history_with_every_finding_status`.
  - `every_registered_tool_has_a_run_scope_decision` still passes, and so does
    `./scripts/check-command-wiring.sh`.
- **The history on screen.** `ReviewHistorySection.test.tsx` renders a `ReviewHistory`
  fixture with two loops. `attempt` is one ascending sequence per task (D29 point 2), so:
  - the earlier loop is implementation #1, review #2 and fix #3;
  - the newest loop is implementation #4, review #5 (three blocking findings), fix #6 (two
    fixed, one rejected with a reason), and review #7, which raises one finding that came
    back after #6 fixed it, one blocking finding new after the fix, one advisory finding,
    and one that 021 carried over as rejected.

  The test:
  - finds `Fixed in #6`, `Rejected in #6 — <the reason>`, `Came back after a fix`,
    `New after a fix`, `Advisory`, `Open`, and the carried-over finding's `Rejected
    earlier as …` resolution verbatim, by their text;
  - finds the head string exactly, from the fixture's `open_blocking` and `open_advisory`;
  - finds the earlier loop collapsed;
  - reloads on each change event it subscribes to;
  - renders nothing for an empty history.

  `src/lib/review.test.ts` asserts every string in Scope 3's head table exactly.
  No function in `src/` groups runs into loops, and a reviewer can check that in the diff.
- **The final diff plus the unresolved findings.**
  - `RunDetailOverlay` for a task's newest row lists the current loop's open findings,
    blocking before advisory by the `blocking` flag. For an older review row it lists that
    review's findings.
  - 017's review item shows the same `OpenFindingsList`.
  - Asserted with `compareDocumentPosition`, as 017 does:
    - in `ReviewView`: outcome with the verdict line, diff summary, commits, PR link,
      `OpenFindingsList`, patch;
    - in `RunDetailOverlay`: outcome with the verdict line, diff, commits, PR link,
      `OpenFindingsList`, prompt, transcript.
- **Configuration.**
  - `review_instructions` edits round-trip at the global and task levels, and the task
    editor shows the override-replaces sentence exactly.
  - Every `ReviewConfig` field can be set, and cleared back to inherit, at the global,
    repository and task levels, through 021's commands.
  - Each inherit option names the backend's inherited value, and no TypeScript function
    resolves precedence.
  - `max_review_loops` offers exactly `0` to `5`.
- **The acknowledgement, in the interface.**
  - Checking the global box, choosing `On`, and choosing `Inherit` whose inherited value is
    on each make **no** command call and open the confirmation.
  - `Turn on review loop` makes exactly one call: carrying
    `enabled: "on_cost_acknowledged"` after `On` or the global box, and clearing the field
    after `Inherit`.
  - `Cancel` makes none, and the control reverts to the stored value.
  - Unchecking the global box, choosing `Off`, and choosing `Inherit` whose inherited value
    is off each write at once, with no confirmation.
  - No code path in `src/` sends `enabled: true` or `enabled: "on"`.
  - `reviewLoopCostNote` returns Scope 6's strings exactly, and uses the effective
    `max_review_loops` at the level being edited.
- **Screenshots.**
  - Task 028's seed contains every state in Scope 7, the click list is in
    `screenshots/views.shot.ts`'s table with only non-mutating clicks, and
    `npm run screenshot` produces every row of Scope 7's table in both colour schemes and
    at both widths.
  - 037's section of the shared PR's body lists the files that were inspected and carries
    a human checklist: a narrow and a wide width, light and dark, and open, advisory,
    fixed, rejected and came-back told apart in greyscale. If the run cannot edit the PR
    body, both go in the message of 037's last implementation commit instead.
- **The seam contract.**
  - D12 gains a dated amendment, "the summary carries the review loop", in the voice of
    its 2026-08-28 amendment. It states the field, the batched reads (and that they do not
    follow `apply_effective_strategy`'s per-repository read), the pure verdict function
    both reads share, and why a SQL copy of the verdict was refused.
  - D29 point 6 gains the dated one-line note from Scope 3.
  - D32's appendix gains the `get_review_history` row: module `review`, board, Read, 046,
    with a note that every grant refuses it. It is appended under 034's dated sub-heading
    ("added after 728a049"), as D32 point 8 requires of every task from 033 to 045. If this
    task built 021's four configuration commands (Notes), their rows go there too.
  - The "How to use this" table gains a row for 037: D4 · D5 · D6 · D7 · D8 · D9 · D12 ·
    D17 · D20 · D28 · D29 · D30 · D32 · D34.
- **No migration** is added, and `package.json` and `Cargo.toml` gain no dependency. Every
  new or changed query macro has its `.sqlx/` entry regenerated with `--all-targets` in the
  same commit (D5).
- Every CI check passes: `npm run typecheck`, `npm run test`, `npm run build`,
  `cargo test -p rimaia-core`, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`.

## Notes

**Seam entries to read first:**

- D4 and D6, as prohibitions, with D34's amendment to D6: the approved list names nothing
  this task needs.
- D5: the `.sqlx/` cache, regenerated with `--all-targets`.
- D7: the event seam.
- D8: no new error code.
- D9: `interrupted` is read off the run's `exit_class`, never off `run_state`.
- D12 and all its amendments.
- D17: the model and effort catalogue the review fields offer.
- D20 point 3: the `…_acknowledged` spelling this UI writes.
- D28: 035's block is the DDL of `review_findings` and `review_config`.
- D29, all of it, especially points 2, 4, 6, 7, 8 and 9, and its closing list of frontend
  files.
- D30 point 5: the grant table, and why operators do not write findings.
- D32 point 8 and its appendix, including 034's dated sub-heading.

**What the chain hands this task.**

- **033:** `runs.head_sha` and a review bundle for every finished row of every kind. The
  overlay's diff is that bundle, not `worktree::diff_summary`.
- **017:** the morning review item and the digest. This task adds a line and a list to
  each and changes nothing else about them.
- **028:** the fixture mode and `npm run screenshot`.
- **035:**
  - `RunKind` on every DTO and in `src/types.ts`;
  - `RunFilter::kind`;
  - the analytics fields;
  - the digest's loop summary;
  - `review_findings` and its writers.
- **021:**
  - `ReviewConfig` and its field-by-field resolution;
  - the `"on_cost_acknowledged"` spelling and its refusal;
  - `review_loop::verdict` and `ReviewLoopSummary` on `TaskDetail`;
  - `review_loop::history`, with the `regressed` and `new_after_fix` lists;
  - the four configuration commands and their MCP tools, unless 021 cut them (below);
  - the fingerprint and the rejected carry-over.

035 also ships `list_review_findings`, with its command and wrapper. The panel and the
overlay do not use it, because `get_review_history` already carries every finding with the
phase it belongs to. Do not add a third read of the same table.

**Where a read this task needs is missing, add it in `rimaia-core` with its command and MCP
tool, never in the frontend, and say so in the PR.** Plausible gaps:

- the per-field inherited value at the repository and task levels, if 021's resolution
  returns only the final values;
- a repository's stored `review_config`, if no DTO carries it;
- a per-finding `blocking: bool` on `ReviewHistory`, and an `open_advisory` count on
  `ReviewLoopSummary` and on the history's current loop, if 021 did not ship them. Compute
  them in `review_loop::history` and the `ReviewLoopSummary` builder against the effective
  `blocking_severity`, with core tests beside 021's
  (`findings_below_the_blocking_severity_do_not_start_a_fix` is the model). The view needs
  both to mark `Advisory`, to order the overlay's list and to write the section head, and
  without them the only way to do any of that is a TypeScript copy of 021's severity rule;
- a pure verdict function, if 021's reads its own rows (Scope 2).

None of these is a license to recompute a rule in TypeScript. If 021's budget turns out to count
sessions differently from `2N + 1`, **stop**: the cost sentence must never promise a bound
the engine does not keep.

**If 021 cut its configuration doors.** 021's size rule lets it cut "the four
configuration doors (commands, tools and their parity tests)" and move them here, keeping
the service functions. If 021 landed without `get_review_settings`,
`set_review_settings`, `set_repository_review_config` and `set_task_review` as commands
and tools, build them first, as thin adapters over 021's service functions: the commands in
`src-tauri/src/commands/review.rs`, registered in both handler lists in
`src-tauri/src/lib.rs`; the tools in `mcp/server.rs`, `requests.rs` and `responses.rs`,
each `Refused` for every grant in `mcp/scope.rs`'s `Tool::run_access` table (021's rule:
a run that could enable its own loop would be spending on its own authority); a parity
test per command; a scope test per tool; and four D32 appendix rows under the same
sub-heading. No rule moves into the adapters: the refusal of an unacknowledged "on" stays
in 021's service. Count this against Size.

**Files to start from.**

- Core:
  - `crates/core/src/tasks/service.rs`: `TASK_SUMMARY_SELECT`, which is a runtime query,
    `list_tasks`, where the batched loop reads go, and `apply_effective_strategy`, the
    pattern not to copy;
  - `crates/core/src/review_loop/`, which 021 creates;
  - `crates/core/src/mcp/scope.rs`, `server.rs`, `requests.rs` and `responses.rs`.
- Shell: `src-tauri/src/commands/review.rs`, which 021 creates (or this task, if 021 cut
  its doors), and both handler lists in `src-tauri/src/lib.rs`.
- Frontend logic:
  - `src/lib/board.ts` (`cardBadge`, and the new `badgeLabel`) and `src/lib/review.ts`
    (017's);
  - `src/lib/runEnvironment.ts`, the model for `reviewLoopCostNote`;
  - `src/lib/commands.ts`. Every call goes through its private `call<T>()`, which task
    049 later swaps per command kind;
  - `src/lib/events.ts`;
  - `src/types.ts`.
- Board and panel:
  - `src/components/board/RunStateBadge.tsx`, `TaskCard.tsx`, `Column.tsx` and
    `TaskDetailPanel.tsx`;
  - `src/components/panel/RunHistorySection.tsx`, `RunOutcomeSection.tsx` and
    `ExtraInstructionsEditor.tsx`;
  - `src/components/runs/RunDetailOverlay.tsx`, `RunReviewSections.tsx` (017's) and
    `ActiveRunCard.tsx`.
- Views:
  - `src/views/ReviewView.tsx` (017's), `src/views/RunsView.tsx`,
    `src/views/AnalyticsView.tsx` and `src/views/SettingsView.tsx`;
  - in `src/views/settings/`: `InstructionsSection.tsx`, `RepositoriesSection.tsx`,
    `StrategyDefaultsFields.tsx`, `OnArchiveFields.tsx` and `StorageSection.tsx`.
- Screenshots: `screenshots/views.shot.ts` and `src/dev/fixtures/` (028's, with 017's
  rows).
- Tests: `crates/core/tests/tasks.rs`, `mcp_scope.rs` and `mcp_tools.rs`.

`OnArchiveFields.tsx` and `StorageSection.tsx` are the codebase's two acknowledgement gates.
Follow them rather than inventing a third.

**Migration:** none. Every column this task reads is in
`20261001120000_run_head_and_review_bundles.sql` (033) or
`20261001120100_run_kinds_and_review_findings.sql` (035).

**What the next tasks expect.**

- **038** rebuilds `tasks` and `repositories` from D28's DDL. This task adds no column and
  no settings key, so D28 needs no change.
- **039** filters `get_review_history` and the summary's batched reads by team, as it
  does every board read.
- **045 and 061** make `review_instructions` consent-gated content and show its revisions
  in the editors built here. Keep those editors as components that can take an extra row.
- **046** registers `get_review_history` (and 021's four doors, wherever they were built)
  from the appendix rows.

**Size.** Expect 2,500 to 3,500 lines of diff, most of it components and their tests,
plus roughly 500 more if 021 cut its configuration doors. If it runs over, or if the doors
are missing and building them would push it over, the cut is between reading the loop and
configuring it:

- **037 keeps Scopes 1 to 4 and 7:** kind everywhere, the card, the history and the
  overlay, and the states among them.
- **Scopes 5 and 6 go to a follow-up:** the settings, the per-task section, the
  acknowledgement, and 021's doors if they are missing.

This workflow runs a fixed task list on one branch, so a follow-up nobody inserts never
runs, and the acknowledgement UI would silently drop out of v0.4. Do not split quietly.
Land Scopes 1 to 4 and 7 as commits, then return `blocked`, naming the follow-up task to
insert before 038 and listing what it carries, so a person decides.

The first half is useful alone, because 021's MCP tools can already turn a loop on (unless
021 cut them, in which case the follow-up is the only way). The second half is not,
because a toggle is no use without a way to see what it did.
