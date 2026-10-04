---
id: "069"
title: Runners in the interface
milestone: v0.5
status: ready
depends_on: ["061"]
adrs: ["0031", "0033", "0034", "0032", "0024", "0012"]
size: L
---

# Runners in the interface

## Goal

Make the machines visible where a person plans and reviews work. ADR-0031's Consequences
say leases make "which runner holds a task and when it was last heard from" visible, and
ADR-0033 point 2 says a team repository shows "which members' runners can serve it". After
this task, in the connected desktop and in the browser:

- a card says **which runner holds it** and when that runner was last heard from, and a
  card **pinned** to a runner says so and offers **run elsewhere**, to a runner the person
  names;
- **Run now** and **Retry now** name a runner wherever the board needs one;
- the owner of a machine a task moved away from **is told**, where they look (ADR-0031
  point 4);
- each repository shows its remote, this computer's mapping, the team ceiling, and the
  runners that report it with their consent and push state; the board says when nobody can
  run a repository's ready work; the browser registers a repository by remote;
- each of the viewer's runners shows its last **doctor result**, and an archived task shows
  its cleanup outcome;
- **Settings** hold this machine's model and effort ceiling and run limits.

This is the runner-facing half of what tasks 042–059 left to "061", split from 061 before
either was started (061's Goal). It adds no rule. Leases and pins are 043's and 053's, who
may start a run is 052's and 067's, mappings and push checks are 054's, run elsewhere and
fencing are 057's, and the ceiling is 045's. This task renders them, and adds the reads a
board of fifty cards needs to render them without fifty requests.

**Solo changes in two places only:** `This machine's limits` in Settings, because a cost
control is useful on one machine too, and the remote and mapping lines under Repositories,
which 054 stored for every repository. No card line, picker or runner list renders in solo,
and every existing frontend test passes without an edit to its assertions.

## Why now

Every source this task renders has landed and has no screen. 043's Out of scope leaves
"pinned to Alice's laptop" here, so a pinned card waits for a sleeping laptop with no sign
of which one. 052 left Run now and Retry gated in the browser, whose only answer without a
runner picker is `name the runner to start this task on`. 057's run elsewhere has no
button, and the owner of a fenced worktree is told nothing. 054 moved its screens out
whole. It lands after 061 because it extends 061's batched reads and fixture scenarios,
and before 062–064 because the end-of-M4 smoke run ("a sleep/wake resume", chaining across
two machines) needs to see where each card is.

## Scope

**1. The holder and the pin on the board read.** A D12 amendment beside 061's, in the same
pattern: batched reads keyed by task id, then Rust functions per task, and no statement
inside a per-task loop. 053 moved these shapes here ("one task owns the shapes"), under the
names it gives. `TaskSummary` and `TaskDetail` (Rust and `src/types.ts`) gain:

```rust
pub struct RunnerRef {
    pub id: RunnerId,
    pub label: String,
    pub owner: UserRef,                       // 061's; runners.user_id is NOT NULL (D28)
    pub owner_is_member: bool,                // of the task's team, as of this read
    pub last_seen_at: Option<DateTime<Utc>>,  // as 053's heartbeat wrote it; None: never seen
}

pub struct Holder {
    pub runner: RunnerRef,
    pub purpose: LeasePurpose,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,    // None for a solo lease
}

// on TaskSummary and TaskDetail:
pub holder: Option<Holder>,           // the live runner_leases row, if any
pub pinned_runner: Option<RunnerRef>, // tasks.pinned_runner_id
```

- **Two batched reads:** `runner_leases` joined to `runners` and `users`, and the pinned
  runners joined the same way, each with a left join to the task team's memberships for
  `owner_is_member`. A runner's owner can leave the team while a pin to their runner
  remains; that is the only way a card names a non-member's machine.
- **Runners are visible only through the team.** A `RunnerRef` appears in a team's read
  only because that runner holds a lease on one of the team's tasks, is pinned to one, or
  reports one of its repositories (Scope 6). That is what ADR-0031's Consequences and
  ADR-0033 point 2 publish, and nothing more. A runner's other teams, pool choices and
  settings never appear. 050's `list_runners` stays the owner's only list of their own.
- `last_seen_at` is as of the board read. No event is added for heartbeats: ADR-0018's
  event list and D8 do not grow, and a stale value errs towards "longer ago", the safe
  direction for a person deciding whether to wait.
- 050's `TaskDetail.worktreeRunner` stays. It says where the worktree is; `holder` says
  who is running now.

**2. Naming a runner.** Pure functions in a new `src/lib/runners.ts` produce every string,
and one component, `src/components/RunnerName.tsx`, renders a name, so every screen names a
runner the same way. `runnerName(runner, capabilities, viewerId)` gives `this
machine` for 049's `localRunnerId`, `your Mac mini` for another of the viewer's runners,
`@bob's Mac mini` for a teammate's, and `a former member's Mac mini` when
`owner_is_member` is false. `runnerLine(task, capabilities, viewerId, now)` returns one
line or `null`, with `relativeTime` from `src/lib/board.ts`:

| Case | Line |
| --- | --- |
| solo mode | `null` |
| held by the local runner | `On this machine` |
| held by another of the viewer's runners | `On your Mac mini · last seen 5m ago` |
| held by a teammate's runner | `On @bob's Mac mini · last seen 5m ago` |
| pinned, not held, the viewer's runner | `Waiting for your Mac mini · last seen 9h ago` |
| pinned, not held, a teammate's runner | `Waiting for @bob's Mac mini · last seen 9h ago` |
| pinned, not held, a former member's runner | `Waiting for a former member's Mac mini · last seen 2d ago` |
| a runner never seen | `… · never seen` in place of the time |

The card shows the line under the badge, which already says `Running`, `Reviewing` or
`Fixing` (037), so the line names only the machine. A pinned task assigned to someone other
than the pin's owner is claimable by nobody until the pin is released (045). Its panel
says, exactly: `Pinned to @bob's Mac mini, but assigned to @carol. Run it elsewhere so
@carol's runners can take it.`

**3. Run elsewhere.** A task's panel offers `Run elsewhere` when it is in `ready`, has a
`pinned_runner` and has no `holder`, which is where 057's command can succeed. It opens an
inline confirmation in the pattern of `OnArchiveFields`' confirm block:

- **The target picker** lists 057's `list_run_elsewhere_targets`, without the row for the
  pinned runner, each row named by `RunnerName` with its last seen time. A row with a
  `refusal` is disabled and shows that sentence under it, unchanged. `list_run_elsewhere_
  targets` gains `ownerId` additively, because logins are not unique (060's resolver) and
  `RunnerName` needs to tell "your" from "@bob's".
- **The text** names both machines once a target is chosen, exactly: `The next attempt
  starts a new session on @carol's MacBook, in a fresh worktree from origin. The agent loses
  its conversation. @bob's Mac mini keeps its worktree and will not push to this branch
  again, so any commit it never pushed stays on that machine.` That last clause is 057's
  "unpushed commits stay behind", which it says this copy has to carry.
- **The buttons** are `Run elsewhere`, disabled until a row without a refusal is chosen,
  and `Cancel`. `Run elsewhere` sends exactly one `run_task_elsewhere` with `{ taskId,
  runnerId }`. A refusal renders as the one error type (D8).

**4. Run now and Retry now name a runner.** 052 made both board commands take an optional
`runnerId`, refused over HTTP without one, and left 050's browser gates on them. This task
deletes both gates.

- **Solo:** unchanged, `{ taskId }`.
- **A pinned task:** no menu. The command is sent with the pinned runner's id, which 052
  defaults a retry to anyway. A pin on a teammate's runner is refused with 067's sentence,
  shown as the one error type: ADR-0031 point 7 lets only the owner start a run there, and
  the panel already offers run elsewhere.
- **Connected desktop:** the local runner is the default. A viewer with one runner sends
  `{ taskId, runnerId: localRunnerId }` with no menu. With more, the button opens a menu
  with this machine first.
- **Browser:** the button always opens the menu.

The menu lists the viewer's own runners from 050's `list_runners`, never a teammate's, each
named by `RunnerName` with its last seen time. A runner whose latest doctor report (Scope 7)
has a failing check shows ` · a doctor check is failing`. That is how a relayed Run now that
the runner refuses after the claim surfaces its reason: 052 lands such a card on `failed`
with no run row, and forbids adding a column for it. Choosing a runner sends exactly one
`start_task_run` or `retry_task_now` with `{ taskId, runnerId }`. A refusal, 052's
offline-runner sentence included, renders as the one error type.

**5. The machine a task left, and the push error.**

- **The fenced-worktree notice.** `get_worktree_status` gains `fencedAt` additively, read
  from the same `runner.db` row 057 adds it to the worktree inventory from. When it is set,
  `WorktreeSection` shows, exactly: `This machine's worktree for "{title}" was left behind
  when the task moved to {runner}. It is kept for you to look through, and Rimaia never
  pushes from it, so a commit that was never pushed is only here. Remove it to run the task
  here.` `{runner}` is the task's `pinned_runner` through `RunnerName`, or `another runner`
  once no pin remains. The first and last sentences are 057's starter refusal; the middle
  one is ADR-0031 point 4's "never pushed from". The worktree list in Settings → Storage
  shows a fenced entry with the state `Left behind`, as a dot and a word.
- **The push error** is the run's `error_message` with 057's sentences, and reaches the
  card and `RunOutcomeSection` through the existing error rendering, unchanged. This task
  only asserts that.

**6. Repositories.** 054 stored all of this and left every screen here. `RepositoriesSection`
gains, per repository:

- **The remote**, in every mode, or `No remote — this computer only`.
- **This computer's mapping**, where the capabilities name a local runner: the clone path
  from `list_checkouts`, or `Not set up on this computer` with a **Map a clone…** action
  that picks a folder through 049's local command and calls `map_repository_checkout`.
  **Unmap** calls `unmap_repository_checkout`. Either refusal is shown as its sentence.
- **The team ceiling**, in every mode but solo, 045's `set_repository_unattended_ceiling`.
  An owner sees a checkbox, `The team allows unattended runs in this repository`, with the
  sentence `Each runner still needs its owner's consent on that machine.` It is not behind
  ADR-0012's dialog, because it cannot make any machine run anything (ADR-0032 point 4).
  A member sees the state as text. A personal team shows no ceiling, because 045 does not
  consult it there.
- **This machine's consent** stays today's local toggle behind ADR-0012's dialog,
  `UNATTENDED_RUNS_GRANT` word for word, labelled `On this machine`, and rendered only where
  the local runner has a checkout of the repository.
- **Runners**, in every mode but solo: one row per row of 054's `list_repository_runners`,
  read through a new `src/hooks/useRepositoryRunners.ts` that re-reads on
  `repositories:changed` (D7). The read gains `ownerId` and `lastSeenAt` additively, so each
  row is named by `RunnerName` with its last seen time. 054 already drops unpaired runners
  and former members' runners. Each row's state comes from `servingRunnerState(row,
  ceiling, personalTeam)` in `src/lib/runners.ts`, first matching line wins:

  | Push | Team ceiling | Runner consent | State |
  | --- | --- | --- | --- |
  | `connected` and `pushError` set | any | any | `Cannot push: <pushError>` |
  | — | personal team | yes / no | `Runs unattended` / `Not consented on this runner` |
  | — | allowed | yes | `Runs unattended` |
  | — | allowed | no | `Not consented on this runner` |
  | — | forbidden | yes | `Consented, but the team forbids it` |
  | — | forbidden | no | `The team forbids it` |

  A push error on the solo runner (`connected` false) is never a state here: 054 makes it a
  dismissable doctor warning there, not a reason a run fails. The state is a dot and a word
  (ADR-0024 rule 3); the word carries the meaning, and the colour only repeats it.
- **Nobody maps it.** A repository with no `list_repository_runners` row reads, exactly:
  `No computer can run tasks in this repository yet.` A repository with a row is mapped,
  whatever its consent or push state.
- **The board's line.** For any repository with at least one `ready` task and no row,
  `Board.tsx` shows one line above the columns, exactly: `No computer can run tasks in
  <name> yet — map a clone in Settings → Repositories.` This is ADR-0033's "a task that
  nobody can run is visible before the night", shown only where it matters.
- **Add repository in the browser.** With no local runner, 050's gate on Add repository is
  replaced by a form for owners: remote URL, optional name and default branch, calling 054's
  board `register_repository`. Its refusals render as their sentences. With a local runner,
  `RepositoryAddForm`'s clone flow stays as 054 left it.

**7. Each runner's doctor result.** In 050's `RunnersSection`, a runner with a row in 054's
`list_runner_doctor_reports` shows it in place of 050's "has not reported" sentence: the
reported-at time, relative as last seen is, then one line per check with its label and its
status as a dot and a word, and, for a per-repository check, the repository's name. Labels
come from `DOCTOR_CHECK_LABELS` in `src/lib/doctor.ts`, spelling `Check::label` for every
`DoctorCheck`. An unknown check id is shown as the id. The summary carries no prose by
design (054), so no detail or remediation is shown.

**8. An archived task's cleanup.** `TaskDetailPanel` shows an archived task's
`archiveOutcome` through 030's `describeCleanup`, and nothing when it is `null`.

**9. This machine's limits.** A new `src/views/settings/RunnerLimitsSection.tsx`, titled
`This machine's limits`, rendered whenever the capabilities name a local runner, solo
included. In the browser it is not shown.

- **Strategy ceiling** (045's `get_strategy_ceiling` / `set_strategy_ceiling`). `Models` is
  `Any model` or a checked subset of the team's catalogue models, in catalogue order.
  `Highest effort` is `No limit` or one of the catalogue's efforts, cheapest first. `Any
  model` stores `models: null`, and `No limit` stores `max_effort: null`. Under them,
  `ceilingNote(ceiling, catalogue)` in `src/lib/runners.ts`, because 045's `judge` fills
  only what a ceiling names:

  | Models | Highest effort | Note |
  | --- | --- | --- |
  | any | no limit | `This machine runs any model at any effort.` |
  | a subset | no limit | `A card that names a model not checked here is not run on this machine. Nothing is changed for it. A card that names no model runs with Opus.` |
  | any | an effort | `A card that names an effort above High is not run on this machine. Nothing is lowered for it. A card that names no effort runs at High.` |
  | a subset | an effort | `A card that names a model not checked here, or an effort above High, is not run on this machine. Nothing is lowered for it. A card that names neither runs with Opus at High.` |

  `Opus` is the first checked model's catalogue label and `High` the chosen effort's.
- **Run limits** (042's `max_turns` and `disallowed_tools` runner keys, which 042 left with
  no control). Two new local commands, `get_runner_limits` and `set_runner_limits`: `local`
  registry rows (D32) with thin handlers over 042's typed readers and a writer beside them,
  and local MCP tools of the same names through 041's host-injected surface. `Max turns` is
  a positive integer or empty (no override). `Blocked tools` is a textarea, one pattern per
  line. The sentence: `These apply on top of the team's limits. The stricter value wins.`
  A `0` or a non-integer is refused by the service as `invalid`, which is 042's reading
  rule made a write rule, not a new one.
- **A headless runner's settings have no control.** The board holds no runner settings
  (050's Out of scope, and no D28 column), and a headless runner has no window. Its run
  environment, concurrency, schedules, limits and ceiling stay on its command line and in
  its `runner.db` through the sqlite3 CLI, as 042 and 058 set the precedent. A browser
  control would need runner settings reported to the board, which is a D28 amendment
  first. 058's Out of scope says the same.

**10. Doors.** Two new commands, both `local`: `get_runner_limits` and `set_runner_limits`,
with `local<T>` wrappers, local MCP tools refused to runs (a run that could lift its own
machine's limits would choose its own budget), and D32 appendix rows. No board command is
added. The additive fields are Scope 1's two, `ownerId` and `lastSeenAt` on
`list_repository_runners`, `ownerId` on `list_run_elsewhere_targets`, and `fencedAt` on
`get_worktree_status`. Every other command called here exists already.

**11. Fixtures and screenshots** (028's mechanism). The fixture table gains a row for every
command this task adds or starts calling, including `list_run_elsewhere_targets`,
`list_repository_runners`, `list_runner_doctor_reports` and `list_checkouts`. 061's `team`
scenario gains: `@bob` runs "Mac mini" (seen 5 min ago) and "build-box" (never seen); one
card held by the local runner; one held by `@bob`'s Mac mini; one pinned to it and waiting;
one pinned to `@bob` and assigned to `@carol`; a fenced worktree on this machine; a
repository nobody maps with a `ready` task; and rows for every state in Scope 6's table.
Views: the board; a panel with the run-elsewhere confirmation and a refused target; a
panel with the fenced notice; Settings → Repositories; Settings → This machine's limits.
`team-browser` adds: the board with the Run now menu open, Settings → Runners with doctor
results, and the register form.

Run `npm run screenshot -- --label before` before the first frontend commit and compare
after the last: every `busy` capture is unchanged in layout except Settings, which gains
only `This machine's limits` and Repositories' remote and mapping lines. Look at the new
captures before finishing, as 028 requires: overflow of long logins and labels, contrast in
both schemes, and whether every state in Scope 6's table reads without colour.

**12. Records.**

- Seam contract: a D12 amendment, "the summary carries the holder and the pin", beside
  061's, stating the two fields, the batched reads and `owner_is_member`.
- Seam contract: a new entry under the next free D number, "Task 069's cross-cutting
  choices", in the four-part shape: runners are visible to a team only through its leases,
  pins and mappings; no heartbeat event; run elsewhere names its target in the
  confirmation; the Run now runner defaults (pin, then local runner); a headless runner's
  settings have no control, and why.
- D32's appendix gains `get_runner_limits` and `set_runner_limits`. The Binds lines of
  D12, D28 (whose lease, pin and runner tables this task reads) and D32 name 069, and "How
  to use this" gains 069's row.

## Out of scope

- **Any rule.** Leases, pins and their release, who may start or move a run, mappings,
  push checks and the ceiling are 043's, 045's, 052's, 053's, 054's, 057's and 067's. If a
  refusal's wording is wrong, change it in the service with its test, never in the view.
- **A per-card reason for a relayed Run now the runner refused after the claim.** 052
  forbids a run row, a column or a message field for it, and D28 has none. Scope 4's doctor
  mark is the surface; a per-card reason would need a D28 amendment, and the PR records it
  as a known gap.
- **Run now on a teammate's runner.** ADR-0031 point 7 forbids it.
- **Controls for a headless runner's settings** (Scope 9).
- **Cloning on demand** (ADR-0033 point 2) and **stale runner versions** (063).
- **Any migration**, and any dependency (D4, D28's D4 amendment, D34).

## Acceptance criteria

Rust tests use the real SQLite harness, 039's `TwoTeams` and 038's builders, and the fake
`Clock`. They contain no `sleep`. Frontend tests are vitest, mock at
`@tauri-apps/api/core` and at 049's HTTP mock, never at the wrappers, and assert exact
command names, arguments and strings.

- **The board read**, in 061's `crates/core/tests/team_board.rs`:
  - `the_card_names_its_holder_and_its_pin_with_last_seen_as_the_heartbeat_wrote_it`, with
    `last_seen_at` set from the fake clock;
  - `a_pin_to_a_former_members_runner_says_so`: the owner leaves the team (051) and
    `owner_is_member` is false;
  - `a_runner_is_visible_to_a_team_only_through_its_leases_pins_and_mappings`: `@bob`'s
    runner holds a task in team B, and nothing in team A's `list_tasks`, `get_task` or
    `list_repository_runners` names it;
  - 061's `list_tasks_issues_as_many_statements_for_fifty_cards_as_for_five` still passes
    with the two new reads.
- **Limits:** `runner_limits_round_trip_through_the_runner_store` and
  `zero_max_turns_is_refused_as_invalid`.
- **Doors:** `every_registered_tool_has_a_run_scope_decision` covers both new tools, and
  `the_runner_limits_are_refused_to_every_run_grant` is in `crates/core/tests/mcp_scope.rs`.
  `./scripts/check-command-wiring.sh` passes.
- **Strings.** `src/lib/runners.test.ts` asserts every row of `runnerLine`'s table and of
  `servingRunnerState`'s, every `runnerName` form, each of `ceilingNote`'s four rows, and
  the reassigned-pin, confirmation and fenced-notice sentences, with an injected `now`.
- **The card** (`TaskCard.test.tsx`): `it("names the holder and the machine a pinned card
  waits for")` and `it("renders no runner line in solo mode")`.
- **Run elsewhere:** nothing is sent until the confirmation; the pinned runner is not
  listed; a refused target is disabled with its exact sentence and cannot be chosen; the
  text names the chosen target exactly; `Run elsewhere` sends exactly one
  `run_task_elsewhere` with `{ taskId, runnerId }`, and `Cancel` sends nothing; the button
  is absent on a held or unpinned task.
- **Run now and Retry now:** in the browser, choosing from the menu sends exactly one
  `start_task_run` (or `retry_task_now`) with `{ taskId, runnerId }`, every call carries a
  `runnerId`, and the menu offers only the viewer's runners; a runner with a failing check
  shows the doctor mark; in connected mode with one runner, no menu renders and the payload
  is `{ taskId, runnerId: localRunnerId }`; a pinned task sends the pin's id with no menu;
  in solo the payload is `{ taskId }`; 050's two gates are gone.
- **The machine a task left:** with `fencedAt` set, `WorktreeSection` renders the notice
  exactly, naming the new runner, or `another runner` with no pin; the Storage list shows
  `Left behind`. A failed run whose `error_message` is 057's push sentence shows it
  unchanged in `RunOutcomeSection` and on the card.
- **Repositories** (`RepositoriesSection.test.tsx`): mapped, unmapped and nobody-maps
  states, with Map a clone… and Unmap each sending one command; a member sees the ceiling
  as text and an owner as a checkbox; a personal team shows no ceiling; turning the ceiling
  on opens no dialog; the local consent toggle still opens ADR-0012's dialog with
  `UNATTENDED_RUNS_GRANT` unchanged; every row of Scope 6's table renders its exact state;
  no nobody-maps line for a solo repository that is mapped, not consented and failing its
  push check. The board's line renders only for a repository with a `ready` task and no
  row. The browser's register form sends `register_repository` and renders its refusal.
- **Doctor and archive:** `RunnersSection` renders a doctor summary, an unknown check id
  included; an archived task renders its `archiveOutcome`.
- **This machine's limits** (`RunnerLimitsSection.test.tsx`): `Any model` and `No limit`
  send `null`; a subset sends the checked ids in catalogue order; the note follows the
  choices; limits send `set_runner_limits`; an `invalid` answer renders as the one error
  type; the section is absent in the browser.
- **Nothing else moves.** Every pre-existing frontend test passes without an edit to its
  assertions, and 050's `it("sends no local command from any view in browser mode")`
  passes with the new sections rendered. No component checks the platform.
- **Screenshots.** Scope 11's views exist in both schemes and both widths. The PR body lists
  the captures inspected and the before/after comparison of `busy`, with a human checklist:
  long logins and labels at the narrow width, and Scope 6's states told apart in greyscale.
- **Records.** The D12 amendment, the new D entry, D32's two appendix rows, the three Binds
  lines and the "How to use this" row exist.
- **No migration**, and `package.json` and every `Cargo.toml` gain no dependency. Every new
  or changed query has its entry regenerated in the matching offline cache with D33's
  recipe, in the same commit.
- Every CI check passes, exactly as CLAUDE.md lists them at the time this lands.
- **Needs a person; the PR body carries it as a checklist:** against a local
  `rimaia-server` with two users and one runner each, forbid a repository as owner and see
  both runners' rows change; start a run from the browser on a chosen runner; sleep one
  machine mid-run and see its card say `Waiting for …`, run it elsewhere, and on the slept
  machine, once awake, read the fenced notice.

## Notes

**Read first.** ADR-0031 points 4, 6 and 7 and its Consequences, ADR-0033 points 2 and 4,
ADR-0034 point 5, ADR-0024 (every rule applies to every new line and control) and
ADR-0012's dialog. Then the seam entries:

- **D12** with 061's amendment, which this task's sits beside.
- **D28**: 043's `runner_leases` and `tasks.pinned_runner_id`, 038's `runners` and `users`,
  and 054's `runner_repositories`.
- **D31** and its 2026-10-04 amendment: what 054's report stores and leaves out.
- **D32**: the registry, `local` rows, the appendix, and point 8's local-handler rule.
- **D33**: the caches. **D7**, **D8**, **D10**, and **D4** and **D34** as prohibitions.
- 045's, 050's, 054's, 057's and 058's own seam entries, whatever numbers they took. 058's
  says which runner settings still have no control.

**Files to start from.** 061's `consent/board.rs` and `team_board.rs`;
`crates/core/src/tasks/service.rs`; 046's registry; `src/lib/board.ts` (`relativeTime`),
`src/lib/archive.ts` (`describeCleanup`), `src/lib/commands.ts`, 049's capabilities;
`src/components/board/TaskCard.tsx`, `TaskDetailPanel.tsx`, `Board.tsx`;
`src/components/panel/WorktreeSection.tsx`, `RunOutcomeSection.tsx`, `RetrySection.tsx`;
in `src/views/settings/`: `RepositoriesSection.tsx` (`UNATTENDED_RUNS_GRANT`),
`RepositoryAddForm.tsx`, `OnArchiveFields.tsx` (the inline-confirmation pattern),
`StrategyDefaultsFields.tsx` (the catalogue vocabulary), `StorageSection.tsx` and 050's
`RunnersSection.tsx`.

**Migration:** none.

**What the chain provides.** 043 and 053: the lease, the pin and `runners.last_seen_at`.
045: the strategy ceiling commands and `set_repository_unattended_ceiling`. 042: the runner
limit keys and their readers. 050: `list_runners`, `RunnersSection` and its gates. 051:
memberships. 052: `runnerId` on Run now and Retry. 054: `list_repository_runners`,
`list_runner_doctor_reports`, `Repository.remote`, `TaskDetail.archiveOutcome`, the mapping
commands and the board `register_repository`. 057: `run_task_elsewhere`,
`list_run_elsewhere_targets` and the inventory's `fencedAt`. 059: `localRunnerId`. 061: the
batched-read pattern, `UserRef` and the `team` scenarios. `depends_on` names 061, which
reaches all of them. If one is not where this file says, stop and ask rather than build a
second copy.

**References to 061 that are this task's.** Written before the split: 042 (the limits
control), 043 and 053 (holder and pin), 045 (the ceiling control, and its doors if 045's
second cut fired), 050 (doctor results in `RunnersSection`, the browser register form),
052 and 067 (the runner picker, a relayed refusal's reason), 054 (every screen), 057 (the
pinned card, targets, the fenced notice, the push error). Read each as 069. 058 already
names 069.

**What the next tasks expect.**

- **063** adds a currency line per runner to `RunnersSection`, beside Scope 7's doctor
  result, and leaves `RunnerName` and `src/lib/runners.ts` as this task leaves them.
- **064** documents the screens in the final pass.
- **065** drops retired columns. Nothing here reads `repositories.allow_unattended_runs`
  except through 045's ceiling function.

**Size.** L: roughly 300 lines of core reads and their tests, 200 for the two doors and the
additive fields, 1,500 of components, 1,300 of frontend tests and 250 of fixtures, about
3,550 lines before `.sqlx/`. If it runs over, stop and propose moving Scopes 7 and 8 (doctor
results and the archive outcome) and the browser register form into a task placed directly
after this one, rather than trimming tests. Never cut the holder and pin lines, run
elsewhere or the runner picker: without them a pinned card waits in silence and the browser
cannot start a run.
