---
id: "061"
title: Assignment, consent and runners in the interface
milestone: v0.5
status: ready
depends_on: ["045", "059"]
adrs: ["0032", "0031", "0024"]
size: L
---

# Assignment, consent and runners in the interface

## Goal

Make the rules of [ADR-0032](../docs/adr/0032-assignment-and-consent-to-run-on-a-machine.md)
and [ADR-0031](../docs/adr/0031-runners-claim-work-with-leases.md) visible where a person
plans and reviews work, so that a shared board never skips a card in silence. After this
task, in the connected desktop and in the browser:

- a card can be **assigned** to a teammate, or returned to the team's pool, from the card
  and from the panel;
- a card that the viewer's runners will not run **says why**, naming who changed what, and
  the viewer accepts it in **one click, next to the content being accepted**;
- a change to the team's **base or review instructions** that the viewer has not consented
  to is a banner above the board, not a column of cards that never start;
- **Settings** hold the viewer's trust list, each of their runners' eligibility policy,
  and this machine's model and effort ceiling and run limits;
- a card says **which runner holds it** and when that runner was last heard from, and a
  card **pinned** to a runner says so and offers "run elsewhere";
- each repository lists the **runners that serve it**, with the team ceiling and each
  runner's own consent shown side by side, and the stricter one visibly winning.

This task is the interface half of task 045 and of ADR-0031's "leases make who has this
visible". It adds no rule. Who may run what, what counts as consent, when a pin is set and
released, and what a refusal says are all 043's, 045's and 057's. This task renders them,
adds the reads a board of fifty cards needs to render them without fifty requests, and
takes screenshots.

**Solo does not change.** A solo board has one person, one team and one machine. None of
the new card lines, the banner, the trust list or the eligibility controls render there,
and every existing frontend test passes without an edit. The one visible addition in solo
is this machine's ceiling and limits in Settings, because a cost control is useful on one
machine too.

## Why now

Task 045 made the claim refuse unconsented content and shipped no screen, by design (its
Out of scope: "Any interface … are all 061's"). Since then a shared board works exactly
the way ADR-0032's Consequences forbid:

> Editing a teammate's plan, or the team's base instructions, makes affected tasks not
> runnable for members who have not accepted it or who do not trust the editor. That is
> the consent model working. The card, and a banner for base instructions, must make the
> reason obvious instead of leaving tasks silently skipped.

Today the reason exists only in `get_task_consent`'s answer and in the queue's
`SkipReason`, and nothing renders either. The same holds for ADR-0031's pin: task 043's
Out of scope leaves "pinned to Alice's laptop" to this task, so a pinned card waits for a
sleeping laptop with no sign of which one, and 057's "run elsewhere" has no button.

It lands after 059 because "your runner" and "someone else's runner" only mean something
once a desktop can be connected. 049's `localRunnerId` is what tells them apart, and
059 is the first task where it is not always the solo runner. It lands before 062–064
because the end-of-M4 smoke run in the plan ("assignment routing, consent blocks and
acceptance") has nothing to click without it.

## Scope

**1. The board read carries assignment, holder, pin and the viewer's consent.** This is a
D12 amendment, and it follows 037's pattern exactly: `list_tasks` runs
`TASK_SUMMARY_SELECT`, then a fixed number of batched reads keyed by task id, then Rust
functions per task in memory. No statement runs inside a per-task loop.

`TaskSummary` and `TaskDetail` (Rust and `src/types.ts`) gain four fields, built by the
same functions for both, so the card and the panel cannot disagree:

```rust
pub struct UserRef { pub id: UserId, pub login: String, pub avatar_url: Option<String> }

pub struct RunnerRef {
    pub id: RunnerId,
    pub label: String,
    pub owner: Option<UserRef>,        // None: the owner's account was deleted
    pub last_seen_at: Option<String>,  // runners.last_seen_at, as 053's heartbeat wrote it
}

pub struct Holder { pub runner: RunnerRef, pub purpose: LeasePurpose }

pub struct CardConsent {
    pub missing: Vec<MissingPiece>,    // task-level pieces only; see below
    pub forbidden_by_team: bool,       // 045's team ceiling, never true in a personal team
}

pub struct MissingPiece {
    pub kind: ContentKind,             // 045's enum
    pub task_id: Option<TaskId>,       // the dependency, for BaseCommit
    pub revision: String,
    pub author: Option<UserRef>,       // None: a former member
    pub written_during_run: bool,
    pub dependency_title: Option<String>, // BaseCommit only
}

// on TaskSummary and TaskDetail:
pub assignee: Option<UserRef>,
pub holder: Option<Holder>,           // the live runner_leases row, if any
pub pinned_runner: Option<RunnerRef>, // tasks.pinned_runner_id
pub consent: Option<CardConsent>,
```

- **The batched reads.** One join of `tasks.assignee_id` to `users` for the listed ids.
  One read of `runner_leases` joined to `runners` and `users`. One read of the pinned
  runners. For consent: the viewer's acceptances and trust rows in the team, the viewer's
  runners' `runner_pool_teams`, the team ceiling of every listed task's repository, and
  one read of each listed task's dependency candidates with their latest successful
  implementation `head_sha` and the owner of the runner that produced it. That last read
  feeds 044's choice of base as a function over rows. If 044's choice is not separable from
  git, extract the row-level half in 044's module with no behaviour change, and do not
  write a second copy of it here.
- **Consent is 045's rule, called, never restated.** `consent::board::card_consent` builds
  each task's inputs from the batched rows and calls 045's `pieces_for` and `consents`
  for purpose `implementation`, plus the task's own review-instructions override when
  037's `review_loop.enabled` is true for that task. A TypeScript or SQL copy of either
  rule is a finding.
- **`consent` is the viewer's, and only when the viewer's runners would run the task.**
  It is `Some` when the task is assigned to `ctx.actor`, or unassigned and either in the
  actor's personal team or in a team one of the actor's runners takes pool work from. It
  is `None` otherwise. Whether a teammate has accepted or trusts something is theirs:
  045's `list_trusted` returns the actor's own list and nobody else's, and a card that said
  "waiting for Bob to accept Alice's plan" would publish Bob's trust list one card at a
  time.
- **Team-wide pieces are not on the card.** Base instructions and the team's review
  instructions reach every task in the team at once. On each card they would turn a
  column into a wall of one repeated sentence. They go to the banner (Scope 3), and
  `CardConsent.missing` never contains `BaseInstructions` or `ReviewInstructions`.
- **Runners are visible only through the team.** A `RunnerRef` appears in a team's read
  only because that runner holds a lease on one of the team's tasks, is pinned to one, or
  maps one of its repositories (Scope 6). That is what ADR-0031's Consequences ("which
  runner holds a task and when it was last heard from") and ADR-0033 point 2 ("which
  members' runners can serve it") publish, and nothing more. A runner's other teams, pool
  choices and settings never appear. 050's `list_runners` stays the owner's only list.
- `last_seen_at` is as of the board read. No event is added for heartbeats: ADR-0018's
  event list and D8 do not grow, and a stale value errs towards "longer ago", which is the
  safe direction for a person deciding whether to wait.

**2. The card and the panel.** Pure functions in two new modules, `src/lib/consent.ts` and
`src/lib/runners.ts`, produce every string, so the wording is tested once and rendered in
three places (card, panel, Settings).

- **The assignee.** A card whose task is assigned shows the assignee's avatar and
  `@login`, as plain text, not a pill (ADR-0024 rule 3's spirit: a name is not a state). An
  unassigned card shows nothing. The picker, `src/components/board/AssigneePicker.tsx`, is
  a menu listing `Unassigned (team pool)` first, then the team's members from 051's
  `list_team_members` by login, the viewer marked `(you)`. It opens from the card's row
  actions, revealed on hover and on `:focus-within` (ADR-0024 rule 5), and from an
  `Assignee` field in `TaskDetailPanel`. Choosing calls 045's `assign_task` once. The
  picker and the field do not render when the task's team has one member.
  - When 060's `create_task` accepts an assignee, the new-task form gains the same field.
    If it does not, the form is unchanged. Adding the argument is not this task's.
- **Why the viewer's runners will not run it.** `consentReasonText(piece)` in
  `src/lib/consent.ts`, exact strings, with `@alice` standing for the author's login and
  `a former member` replacing it when `author` is `None`:

  | Kind | Text |
  | --- | --- |
  | `plan` | `@alice changed the plan (revision 4)` |
  | `task_review_instructions` | `@alice changed this task's review instructions (revision 2)` |
  | `base_commit` | `Starts from @carol's commit 1a2b3c4 on "Add the parser"` |
  | `review_findings` | `Review findings from @carol's runner` |
  | `base_instructions` | `@alice changed the team's base instructions (revision 7)` |
  | `review_instructions` | `@alice changed the team's review instructions (revision 3)` |

  A piece with `written_during_run` inserts ` during a run` after the verb phrase, for
  example `@bob changed the plan during a run (revision 5)`. The commit is its first seven
  characters, and the title is the dependency's. `forbidden_by_team` reads
  `The team does not allow unattended runs in this repository`.
- **On the card**, only in the `ready` column (the only column the queue reads, ADR-0010),
  one line: `Not runnable for you: ` followed by the first reason, then ` · 2 more` when
  there are others. The line has one button, `Review`, which opens the panel scrolled to
  the consent section. **The card never accepts anything itself.** 045 accepts only the
  current revision "so nobody accepts text they did not see". A button on a card that
  shows a title and not the plan would accept exactly that. One click accepts, from the
  place where the content is on screen.
- **In the panel**, a new `src/components/panel/ConsentSection.tsx` above
  `RunHistorySection`, rendered whenever `consent` has a missing piece or is forbidden, in
  every column. Each missing piece shows its reason, the content it would accept, and one
  button, `Accept revision 4` (or `Accept this commit`, or `Accept these findings`):
  - **plan:** the plan and extra instructions as the panel already renders them;
  - **task review instructions:** 037's override editor, read-only here;
  - **base commit:** the full SHA as copyable monospace (ADR-0024 rule 2), the dependency's
    title, and `Open its review`, which opens the dependency's newest run in
    `RunDetailOverlay` with its 033 bundle;
  - **review findings:** 037's `OpenFindingsList` for that review run.

  One click calls 045's `accept_content` once, with the piece's `taskId`, `kind` and
  `revision` exactly as the read gave them. A `Conflict` (the content changed since it
  was read) re-reads the task and shows, in place of the button,
  `@carol changed it again (revision 6). Read the new version before accepting.` It never
  retries with the new revision on the person's behalf.
- **A written-during-run piece** shows one more sentence under its reason:
  `Trust does not cover changes made during a run, so this needs your acceptance.`
  (ADR-0032 point 6: it "never counts as trusted for anyone").
- **The holder and the pin.** `runnerLine(task, capabilities, viewerId, now)` in
  `src/lib/runners.ts` returns one line or `null`, using `relativeTime` from
  `src/lib/board.ts` for the time, and `@login's Label` for a runner:

  | Case | Line |
  | --- | --- |
  | solo mode | `null` |
  | held by the local runner (049's `localRunnerId`) | `On this machine` |
  | held by another of the viewer's runners | `On your Mac mini · last seen 5m ago` |
  | held by a teammate's runner | `On @bob's Mac mini · last seen 5m ago` |
  | pinned, not held, the viewer's runner | `Waiting for your Mac mini · last seen 9h ago` |
  | pinned, not held, a teammate's runner | `Waiting for @bob's Mac mini · last seen 9h ago` |
  | a runner never seen | `… · never seen` in place of the time |
  | a former member's runner | `a former member's Mac mini` in place of the name |

  The card shows it under the badge. The badge already says `Running`, `Reviewing` or
  `Fixing` (037), so the line names only the machine.
- **A pinned task assigned to someone other than the pin's owner** is claimable by nobody
  until the pin is released (045). The panel says, exactly,
  `Pinned to @bob's Mac mini, but assigned to @carol. Run it elsewhere so @carol's runners
  can take it.`
- **Run elsewhere.** A pinned task's panel offers `Run elsewhere`. It opens an inline
  confirmation, in the pattern of `OnArchiveFields`' confirm block, reading:
  `The next attempt starts a new session on another runner, in a fresh worktree. The agent
  loses its conversation. @bob's Mac mini keeps its worktree, and will not push to this
  branch again.` The buttons are `Run elsewhere` and `Cancel`. Only the first calls 057's
  command, once. Who may press it and what a held task answers are 057's rules: render the
  button wherever 057's command could succeed, and render its refusal as the one error
  type (D8) where it does not.

**3. The instructions banner.** A new board read, `get_team_consent`, answers the
viewer's missing team-wide pieces in the current team:

```ts
interface TeamConsent {
  teamId: string;
  missing: MissingPiece[]; // BaseInstructions and ReviewInstructions only
}
```

Built by 045's `consents` over `team_settings`' revision columns. `ReviewInstructions` is
listed only when 021's review loop is enabled for at least one non-archived task in the
team, read with 037's batched configuration read, because instructions no run reads are
not a reason anything waits. In a personal team `missing` is always empty.

`src/components/board/InstructionsConsentBanner.tsx` sits above the board, in every mode
but solo, when `missing` is non-empty. One line per piece, from `consentReasonText`,
followed by `Your runners will not start tasks in this team until you accept it or trust
@alice.` (for a written-during-run piece: `until you accept it.`). Its one button,
`Review`, opens Settings → Instructions, where the text is on screen, and where
`InstructionsSection` and 037's `ReviewSection` show the same reason with an
`Accept revision 7` button beside the text. Accepting calls `accept_content` with
`taskId: null`. The banner cannot be dismissed: it is the reason the board is idle, and
it goes away when the reason does. It re-reads on `settings` and `tasks` change events
(D7) and after the viewer's own acceptance or trust change.

There is no diff of what changed. `team_settings` holds only the current value, and keeping
old revisions would need a column D28 does not have. The banner names who changed it and
when (`updated_at`), and the text is one click away.

**4. Trust and eligibility in Settings.** A new `src/views/settings/TeamWorkSection.tsx`,
titled `Runners and trust`, rendered in every mode but solo, and only when the viewer
belongs to a team with more than one member.

- **Trust, per team.** For the current team (050's switcher), every member but the viewer,
  from `list_team_members`, with a checkbox `Trust @bob's changes`, checked from 045's
  `list_trusted_authors`. Above the list, exactly: `Changes a trusted teammate makes to
  plans, instructions and code in this team run on your runners without asking you. A
  change they make during a run still asks. Nobody else can see or change this list.`
  Toggling calls `set_author_trust` once with `{ teamId, userId, trusted }`. Trusting is
  not behind a confirmation. Revoking is immediate. Both are one click, and both are what
  the sentence above describes.
- **Eligibility, per runner.** For each of the viewer's runners (050's `list_runners`),
  two radios: `Only tasks assigned to me` and `Tasks assigned to me, then unassigned tasks
  in:`, the second with one checkbox per team from `list_teams`. The checkboxes are
  disabled under the first radio. Saving calls 045's `set_runner_eligibility` once, with
  the policy and the complete list of checked team ids, because 045 replaces the list
  whole. `list_runners` gains `poolTeamIds`, read from `runner_pool_teams` and limited to
  teams the caller belongs to, so the form starts from the stored state.
- Eligibility lives on the board (ADR-0032 point 2), so it is editable in the browser too.
  This replaces 050's read-only `eligibility` in `RunnersSection` with the same control.

**5. This machine's ceiling and limits.** A new
`src/views/settings/RunnerLimitsSection.tsx`, titled `This machine's limits`, rendered
whenever the capabilities name a local runner, in solo as well.

- **Strategy ceiling** (045's `get_strategy_ceiling` / `set_strategy_ceiling`). `Models`
  is `Any model` or a checked subset of the team's catalogue models, in catalogue order.
  `Highest effort` is `No limit` or one of the catalogue's efforts, cheapest first. Under
  them, exactly: `A card that names a model or effort above this is not run on this
  machine. Nothing is lowered for it. A card that names neither runs with the first model
  checked here and the highest effort allowed.` That is 045's "refuse a named choice, fill
  an absent one". `Any model` stores `models: null`, and `No limit` stores
  `max_effort: null`.
- **Run limits** (042's `max_turns` and `disallowed_tools` runner keys, which 042 left
  with no control). Two new local commands, `get_runner_limits` and `set_runner_limits`,
  are registry `local` rows (D32) with thin handlers over 042's typed readers and a writer
  beside them, and local MCP tools with the same names through 041's host-injected
  surface. `Max turns` is a positive integer or empty (no override). `Blocked tools`
  is a textarea, one pattern per line. The sentence: `These apply on top of the team's
  limits. The stricter value wins.` A `0` or a non-integer is refused by the service as
  `invalid`, which is 042's reading rule made a write rule, not a new one.
- In the browser these are not shown. 050's `RunnersSection` already says where a runner's
  settings are changed.

**6. Repositories: who serves them, and the ceiling against each runner's consent.**
`RepositoriesSection` gains, per repository, in every mode but solo:

- **The team ceiling**, 045's `set_repository_unattended_ceiling`. An owner sees a
  checkbox, `The team allows unattended runs in this repository`, with the sentence
  `Each runner still needs its owner's consent on that machine.` It is not behind
  ADR-0012's dialog, because it cannot make any machine run anything (ADR-0032 point 4).
  Turning it off writes at once. A member sees the state as text, and a personal team shows
  no ceiling at all, because 045 does not consult it there.
- **This machine's consent** stays today's local toggle behind ADR-0012's dialog,
  `UNATTENDED_RUNS_GRANT` word for word. It is labelled `On this machine`, and rendered
  only where the local runner has a checkout of the repository.
- **Served by**, a list of the runners that map the repository, from 054's
  `runner_repositories`. Each row names the runner as `runnerLine` does (`this machine`,
  `your Mac mini`, `@bob's Mac mini`), its last seen time, and one state from
  `servingRunnerState(ceiling, consent, personalTeam)` in `src/lib/runners.ts`:

  | Team ceiling | Runner consent | State |
  | --- | --- | --- |
  | allowed | yes | `Runs unattended` |
  | allowed | no | `Not consented on this runner` |
  | forbidden | yes | `Consented, but the team forbids it` |
  | forbidden | no | `The team forbids it` |
  | any | not reported | `Consent not reported yet` |
  | personal team | yes / no | `Runs unattended` / `Not consented on this runner` |

  The state is a dot and a word (ADR-0024 rule 3). The word always carries the meaning,
  and the colour only repeats it.
- **The read.** If 054 added a read that returns these rows with each mapping's reported
  consent, use it. Otherwise add `list_repository_runners(repositoryId)`, a board `Read`
  row with a core handler, returning `{ runner: RunnerRef, unattendedConsent: boolean |
  null, reportedAt }` for the repository's rows, excluding unpaired runners. It is
  team-scoped through the repository, like every board read (039).

**7. The queue's reasons.** `QUEUE_SKIP_LABELS` in `src/components/runs/QueuePlanList.tsx`
gets the final wording for 045's three variants, replacing whatever placeholder 045 had to
write to keep `Record<SkipReason, string>` total:

- `not_eligible`: `assigned to someone else, or outside the pool this runner takes`
- `consent_missing`: `waiting for you to accept a change`
- `forbidden_by_team`: `the team does not allow unattended runs in this repository`

If 054 added a "no checkout" variant, it gets `this machine has no checkout of the
repository`. Existing labels are unchanged.

**8. Doors.** Every command this task adds, and only those, as the registry requires (D32):

- `get_team_consent`: board `Read`, a core handler under `crates/core/src/api/board/`, a
  `board<T>` wrapper, an MCP tool of the same name taking 060's `team` argument, and
  refused on the run-scoped surface (D30).
- `list_repository_runners`, only if Scope 6 needs it: as `get_team_consent`.
- `get_runner_limits` and `set_runner_limits`: `local` rows, `local<T>` wrappers, local MCP
  tools, refused to runs. A run that could lift its own machine's limits would be a run
  choosing its own budget.

Each board command gets 046's per-command case and a two-team case (039's registry test).
Every other command this task calls exists already, and this task changes none of their
shapes except the three additive fields: `poolTeamIds` on `list_runners`, and the four
summary fields in Scope 1.

**9. Fixtures and screenshots** (028's mechanism, 049's per-scenario capabilities). The
fixture table gains a row for every command this task adds or starts calling. New
scenarios:

| Scenario | Seeds | Views |
| --- | --- | --- |
| `team` | connected capabilities with a local runner "MacBook"; viewer `@alice` in a personal team and in "Platform" with `@bob` and `@carol`; `@bob` runs "Mac mini" (seen 5 min ago) and "build-box" (never seen). Cards: one assigned to each member; one with a plan changed by `@bob`; one changed during a run; one starting from `@carol`'s commit; one held by the local runner; one held by `@bob`'s Mac mini; one pinned to it and waiting; one pinned to `@bob` and assigned to `@carol`; a repository the team forbids. The base instructions changed by `@carol` | board with the banner; board with the assignee picker open; a panel with three missing pieces; a panel with the run-elsewhere confirmation open; the `Conflict` state; Settings → Runners and trust; Settings → Repositories with every row of Scope 6's table; Settings → This machine's limits |
| `team-browser` | as `team`, with browser capabilities | board; a panel with a missing piece; Settings → Runners and trust; Settings → Repositories |

`busy` and every existing scenario are unchanged. Run `npm run screenshot -- --label
before` before the first frontend commit and compare after the last: every `busy` capture
must be unchanged in layout except Settings, which gains only `This machine's limits`.
Look at the new captures before finishing, as 028 requires: overflow of long logins and
labels, wrapping of the reason line on a narrow card, contrast in both schemes, and
whether every state in Scope 6's table reads without colour.

**10. Records.**

- Seam contract: a D12 amendment, "the summary carries assignment, the holder, the pin
  and the viewer's consent", in the voice of its 2026-08-28 amendment and 037's. It states
  the fields, the batched reads, and why a copy of 045's rule in SQL or TypeScript was
  refused.
- Seam contract: a new entry under the next free D number, "Task 061's cross-cutting
  choices", in the four-part shape, recording: consent is shown only to the person whose
  runners would run the task; team-wide pieces go to the banner, never to cards;
  acceptance happens where the content is visible; runners are visible to a team only
  through the leases, pins and mappings that tie them to it; no event for heartbeats.
  Add a row for 061 to "How to use this".

## Out of scope

- **Any rule.** Eligibility, consent, the ceiling, pinning and its release, and every
  refusal message are 043's, 045's and 057's. If a refusal's wording is wrong, change it in
  the service with its test, never by rewording it in the view.
- **Any migration**, and any dependency (D4, D28's D4 amendment, D34). Every column this
  task reads exists after 045, 053, 054 and 057.
- **A revision history, or a diff between revisions.** No column holds an old plan or old
  instructions.
- **Filtering the board by assignee.** `planMove` computes positions between visible
  neighbours, and a filtered column would place a dropped card between two cards that are
  not adjacent. That needs its own design.
- **Seeing a teammate's trust list or acceptances** (Scope 1). Trust is personal
  (ADR-0032 Consequences).
- **Showing a card as refused by this machine's ceiling.** The board cannot read the
  ceiling (it is in `runner.db`). The queue's own skip reason is where that shows.
- **Run now on a teammate's runner.** ADR-0031 point 7 forbids it. Run now stays the
  viewer's own, as 052 left it.
- **The `claude mcp add` line and any MCP presentation** (060), and **stale runner
  versions** (063).
- **Notifications** of a new revision to accept. The banner and the card are the
  notification.

## Acceptance criteria

Rust tests use the real SQLite harness, 039's `TwoTeams` and 038's builders, and the fake
`Clock`. They contain no `sleep`. Frontend tests are vitest, mock at
`@tauri-apps/api/core` and at 049's HTTP mock, never at the wrappers, and assert exact
command names, arguments and strings.

- **The board read**, in `crates/core/tests/tasks.rs` or a new
  `crates/core/tests/team_board.rs`:
  - `the_card_and_get_task_consent_agree_on_what_is_missing`, across a plan edit, a
    written-during-run edit, a dependency commit from another owner's runner, a task
    review-instructions override with the loop on, and the same override with the loop
    off (no piece);
  - `a_card_assigned_to_someone_else_carries_no_consent`;
  - `an_unassigned_card_carries_consent_only_when_one_of_the_viewers_runners_takes_the_pool`;
  - `team_wide_pieces_are_on_the_team_consent_read_and_never_on_a_card`;
  - `the_card_names_its_holder_and_its_pin_with_last_seen_as_the_heartbeat_wrote_it`, with
    `last_seen_at` set from the fake clock;
  - `a_board_mixing_consented_and_unconsented_cards_keeps_each_cards_own`, which catches a
    batched read keyed by the wrong id;
  - `a_solo_board_has_nothing_missing_and_no_assignee`;
  - `a_runner_is_visible_to_a_team_only_through_its_leases_pins_and_mappings`: `@bob`'s
    runner holds a task in team B, and nothing in team A's `list_tasks`, `get_task`,
    `get_team_consent` or repository read names it;
  - the batched reads are a fixed number of statements, and no statement runs inside a
    per-task loop. A reviewer can check that in the diff.
- **The team consent read:**
  `a_base_instructions_edit_by_an_untrusted_teammate_is_missing_for_the_team`,
  `trusting_the_editor_clears_it_unless_it_was_written_during_a_run`,
  `review_instructions_are_missing_only_when_the_loop_is_on_somewhere_in_the_team`, and
  `a_personal_team_has_nothing_missing`.
- **Serving runners and limits** (where this task adds the reads):
  `serving_runners_report_consent_as_last_reported`, `an_unpaired_runner_no_longer_serves`,
  `runner_limits_round_trip_through_the_runner_store`, and
  `zero_max_turns_is_refused_as_invalid`.
- **Doors:** 046's `every_board_command_has_a_case`,
  `a_team_cannot_see_another_teams_ids` and
  `both_transports_answer_every_case_identically` pass with the new rows.
  `every_registered_tool_has_a_run_scope_decision` covers every new tool, and
  `the_team_consent_read_and_the_runner_limits_are_refused_to_every_run_grant` is in
  `crates/core/tests/mcp_scope.rs`. `./scripts/check-command-wiring.sh` passes.
- **Strings.** `src/lib/consent.test.ts` asserts every row of Scope 2's reason table, the
  former-member form and the during-a-run form. `src/lib/runners.test.ts` asserts every row
  of `runnerLine`'s table and of `servingRunnerState`'s table, with an injected `now`.
- **The card** (`TaskCard.test.tsx`):
  - `it("shows the assignee's login and nothing when unassigned")`;
  - `it("shows why a ready card will not run, with the first reason and a count")`;
  - `it("shows no consent line outside the ready column")`;
  - `it("never calls accept_content from the card")`: clicking `Review` opens the panel and
    sends no command;
  - `it("names the holder and the machine a pinned card waits for")`;
  - `it("renders none of the new lines in solo mode")`.
- **The assignee picker** (`AssigneePicker.test.tsx`): `Unassigned (team pool)` first, then
  members by login with `(you)`; choosing sends exactly one `assign_task` with the task id
  and the user id, or `null`; it is reachable and operable by keyboard alone; it does not
  render in a one-member team; after a `tasks` change event from a member's removal (051),
  the card shows no assignee.
- **The consent section** (`ConsentSection.test.tsx`):
  - each piece renders its content beside its button, as Scope 2 lists;
  - one click sends exactly one `accept_content` with `{ taskId, kind, revision }` as read,
    and the section re-reads;
  - a `Conflict` answer renders `@carol changed it again (revision 6). Read the new version
    before accepting.`, sends no second `accept_content`, and re-reads;
  - a written-during-run piece shows the trust sentence exactly;
  - `Open its review` opens the dependency's newest run.
- **Run elsewhere:** choosing it sends nothing until the confirmation; the confirmation's
  text is exact; `Run elsewhere` sends 057's command once, and `Cancel` sends nothing; the
  reassigned-pin sentence is exact.
- **The banner** (`InstructionsConsentBanner.test.tsx`): it renders one line per missing
  piece with the exact sentence; it has no dismiss control; `Review` opens Settings →
  Instructions; accepting there sends `accept_content` with `taskId: null`; it disappears
  after the re-read; it never renders in solo mode or with nothing missing.
- **Settings:**
  - `TeamWorkSection`: the trust sentence is exact; the viewer is not listed; toggling
    sends `set_author_trust` with `{ teamId, userId, trusted }`; eligibility saves send
    `set_runner_eligibility` once with the policy and the whole team-id list; team
    checkboxes are disabled under `Only tasks assigned to me`; it does not render in solo.
  - `RunnerLimitsSection`: `Any model` and `No limit` send `null`; a subset sends the
    checked ids in catalogue order; the ceiling sentence is exact; limits send
    `set_runner_limits`; an `invalid` answer renders as the one error type.
  - `RepositoriesSection`: a member sees the ceiling as text and an owner as a checkbox;
    a personal team shows no ceiling; turning the ceiling on opens no dialog; the local
    consent toggle still opens ADR-0012's dialog with `UNATTENDED_RUNS_GRANT` unchanged;
    every row of Scope 6's table renders its exact state.
- **The queue:** `QueuePlanList.test.tsx` asserts the three new labels exactly.
- **Nothing else moves.** Every pre-existing frontend test passes without an edit to its
  assertions, and 050's `it("sends no local command from any view in browser mode")`
  passes with the new sections rendered. No component checks the platform. Every choice
  comes from 049's capabilities.
- **Screenshots.** Scope 9's scenarios exist, and `npm run screenshot` produces them in both
  schemes and both widths. The PR body lists the captures that were inspected and the
  before/after comparison of `busy`. It carries a human checklist: long logins and labels
  at the narrow width, the reason line wrapping on a card, and Scope 6's states told apart
  in greyscale.
- **Records.** The D12 amendment, the new D entry and the "How to use this" row exist.
- **No migration**, and `package.json` and every `Cargo.toml` gain no dependency. Every new
  or changed query has its entry regenerated in the matching offline cache with D33's
  recipe, in the same commit.
- Every CI check passes, exactly as CLAUDE.md lists them at the time this lands, including
  the runner and server crates' steps.
- **Needs a person; the PR body carries it as a checklist:** against a local
  `rimaia-server` with two users and one runner each, assign a card to the other user and
  see it run only there; edit their plan and see their card say why; accept it from their
  panel and see it start; forbid a repository as owner and see both runners' rows change;
  sleep one machine mid-run and see its card say `Waiting for …`, then run it elsewhere.

## Notes

**Read first.** ADR-0032 in full, ADR-0031 points 4, 6 and 7 and its Consequences,
ADR-0024 (every rule applies to every new line and control), ADR-0033 point 2,
ADR-0034 point 5 and ADR-0012's dialog. Then the seam entries:

- **D12** and all its amendments, especially 037's, which this task's amendment follows.
- **D28**: 045's consent DDL, 043's `runner_leases` and `tasks.pinned_runner_id`, 038's
  `runners` and `users`, and 054's `runner_repositories`.
- **D29**: `head_sha` means an implementation run's, for the base-commit piece.
- **D30**: the run-scoped surface every new tool is refused on.
- **D31** point 14: 054's `report_runner`, which carries each checkout's consent.
- **D32**: the registry, `board` and `local` rows, and point 8's local-handler rule.
- **D33**: the caches. **D7**, **D8**, **D10**, and **D4** and **D34** as prohibitions.
- 045's, 050's and 051's own seam entries, whatever numbers they took.

**Files to start from.**

- Core: `crates/core/src/tasks/service.rs` (`TASK_SUMMARY_SELECT`, `list_tasks`, where 037
  put its batched reads); 045's `crates/core/src/consent/`, where `board.rs` goes;
  `crates/core/src/mcp/scope.rs`; 046's `crates/core/src/api/` registry and
  `api/board/`.
- Frontend logic: `src/lib/board.ts` (`relativeTime`, `cardBadge`), `src/lib/commands.ts`,
  `src/lib/events.ts`, 049's capabilities module, `src/types.ts`.
- Board and panel: `src/components/board/TaskCard.tsx`, `TaskDetailPanel.tsx`,
  `Board.tsx`; `src/components/panel/RunHistorySection.tsx`, `PlanEditor.tsx`,
  `ExtraInstructionsEditor.tsx`; `src/components/runs/QueuePlanList.tsx` and
  `RunDetailOverlay.tsx`.
- Settings: `src/views/SettingsView.tsx`, and in `src/views/settings/`:
  `RepositoriesSection.tsx` (`UNATTENDED_RUNS_GRANT`), `InstructionsSection.tsx`,
  `OnArchiveFields.tsx` (the inline-confirmation pattern), `StrategyDefaultsFields.tsx`
  (the catalogue vocabulary), 037's `ReviewSection.tsx` and 050's `RunnersSection.tsx`.
- Fixtures: 028's `src/dev/fixtures/` and `scripts/screenshot.mjs`.

**Migration:** none.

**What the chain provides.**

- **045:** `assign_task`, `accept_content`, `set_author_trust`, `list_trusted_authors`,
  `set_runner_eligibility`, `set_repository_unattended_ceiling`, `get_task_consent`,
  `get_strategy_ceiling` and `set_strategy_ceiling`; `pieces_for` and `consents`; the three
  `SkipReason` variants; `Task`'s authorship fields.
- **043 and 053:** the lease, the pin and `runners.last_seen_at`.
- **050:** `list_teams`, `list_runners`, the switcher, `RunnersSection`, and
  `TaskDetail.worktreeRunner`, which this task's `holder` complements and does not replace.
- **051:** `list_team_members` and roles, and unassignment on removal.
- **054:** `runner_repositories` and each mapping's reported consent.
- **057:** the run-elsewhere command.
- **059:** the connected desktop, and a `localRunnerId` that is not the solo runner.
- **037:** `review_loop` on the summary, `OpenFindingsList`, `ReviewSection`.

`depends_on` names 045 and 059. Every other task above is ordered before 059, so it has
landed by the time this starts. If any of these is not where this file says, stop and ask
rather than build a second copy.

**One gap to check before starting.** D31 point 14 says 054's `report_runner` carries each
checkout's consent, but D28's `runner_repositories` DDL has no column for it. If 054
amended D28 and stores it, Scope 6 reads it. If it stores nothing, **stop and ask**. The
board cannot show "the runner decides" (ADR-0032's last alternative) from a fact it never
received, and this task has no migration to add.

**Decisions this file makes, which the new D entry records.**

- **Consent is shown to the person whose runners would run the task, and to nobody
  else.** Anything wider leaks a personal trust list.
- **Team-wide pieces go to one banner.** Each card would otherwise repeat one sentence.
- **Acceptance happens where the content is visible.** The brief asked for one-click
  accept on the card. It is one click, from the panel the card's `Review` opens, because a
  card shows a title and not the text being accepted. If the product owner wants the
  button on the card itself, that is a change to 045's "nobody accepts text they did not
  see", and needs an ADR-0032 note first.
- **Runners are visible through the team's own work only.**
- **No heartbeat event.** Last seen is as of the board read.

**What the next tasks expect.**

- **062** watches the new reads' cost in its metrics, and needs nothing else here.
- **063** adds a stale-version mark beside the runner names this task renders. Keep the
  runner name a component (`RunnerName`) it can extend.
- **064** documents the screens in the final pass and adds 061 to CLAUDE.md's
  must-have-tests list only if a rule moved here, which it must not.
- **065** drops retired columns. Nothing here reads `repositories.allow_unattended_runs`
  except through 045's ceiling function.

**Size.** L, and at the upper edge of one session: roughly 500 lines of core reads and
their tests, 250 across the new doors, 1,600 of components, 1,200 of frontend tests and 300
of fixtures, about 3,800 lines before `.sqlx/`. If it runs over, cut here:

- **061 keeps Scopes 1 to 4, 7 and 8, the board-facing half:** assignment, consent on the
  card and the panel, the banner, trust and eligibility, holder, pin and run elsewhere, and
  the queue labels. These are what stop a shared board skipping cards in silence.
- **A follow-up, appended under the next free number and placed directly after 061, takes
  Scopes 5 and 6:** this machine's limits and the per-repository serving runners. Each is
  useful alone, and neither is needed to understand why a card is not running.

Amend this file in the same commit as the cut, and say so in the PR. Never cut the card's
reason line, the acceptance flow or the banner. They are the property this task exists
for.
