---
id: "061"
title: Assignment and consent in the interface
milestone: v0.5
status: ready
depends_on: ["045", "059", "060"]
adrs: ["0032", "0024"]
size: L
---

# Assignment and consent in the interface

## Goal

Make the rules of [ADR-0032](../docs/adr/0032-assignment-and-consent-to-run-on-a-machine.md)
visible where a person plans and reviews work, so that a shared board never skips a card in
silence. After this task, in the connected desktop and in the browser:

- a card can be **assigned** to a teammate, or returned to the team's pool, from the card,
  the panel and the new-task form;
- a card that the viewer's runners will not run **says why**, naming who changed what, and
  the viewer accepts it in **one click, next to the content being accepted**;
- a change to the team's **base or review instructions** that the viewer has not consented
  to is a banner above the board, not a column of cards that never start;
- **Settings** hold the viewer's trust list and each of their runners' eligibility policy;
- in the browser, **Plan** records 060's request, and the card says who asked for a plan.

This task is the interface half of task 045, and takes 060's interface. It adds no rule.
Who may run what, what counts as consent and what a refusal says are 045's. This task
renders them, and adds the reads a board of fifty cards needs to render them without fifty
requests.

**The runner-facing half is [task 069](069-runners-in-the-interface.md)**, placed directly
after this one: who holds a card and the pin, run elsewhere, the Run now runner picker, the
fenced-worktree notice, the repository screens 054 left (serving runners, mapping, the
browser's register form, the board's unmapped line), doctor reports, an archived task's
cleanup, and this machine's limits. The split was decided before either task was started,
because together they were well past one session. Tasks 042–067 were written when the two
were one task and name 061 for both halves; 069's Notes list which of those references are
its.

**Solo does not change.** A solo board has one person, one team and one machine. None of
the new card lines, the banner, the trust list or the eligibility controls render there,
and every existing frontend test passes without an edit to its assertions.

## Why now

Task 045 made the claim refuse unconsented content and shipped no screen, by design (its
Out of scope: "The assignee picker, the consent banner, the trust list, the runner policy
and the ceiling controls are all 061's"). Since then a shared board works exactly the way
ADR-0032's Consequences forbid:

> Editing a teammate's plan, or the team's base instructions, makes affected tasks not
> runnable for members who have not accepted it or who do not trust the editor. That is
> the consent model working. The card, and a banner for base instructions, must make the
> reason obvious instead of leaving tasks silently skipped.

Today the reason exists only in `get_task_consent`'s answer and in the queue's
`SkipReason`, and nothing on the board renders either. Task 060 likewise left the browser's
Plan buttons, the "plan requested" mark and the assignee picker to this task.

It lands after 059 because "your runner" only means something once a desktop can be
connected: 049's `localRunnerId` is not the solo runner before then. It lands after 060
because it calls 060's request commands and `create_task`'s `assigneeId`. It lands before
062–064 because the end-of-M4 smoke run in the plan ("assignment routing, consent blocks
and acceptance") has nothing to click without it.

## Scope

**0. First commit: the doors for trust and eligibility (045's second cut).** 045 wrote the
services `consent::set_trust`, `consent::list_trusted` and `consent::set_runner_eligibility`
with their tests, and left their doors here (045's Notes, "The split"). Three board commands
in `src-tauri/src/commands/`, registered in both `generate_handler!` lists, wrapped in
`src/lib/commands.ts`, and exposed as MCP tools with the same names: `set_author_trust`,
`list_trusted_authors` and `set_runner_eligibility`. Each is a thin adapter, refused to runs in
`Tool::run_access`, added to 039's two-team registry test and to D32's appendix, and given a
solo fixture row: `set_author_trust` and `set_runner_eligibility` answer success without
changing the seed, and `list_trusted_authors` answers `[]`. **`set_author_trust` is the one
that matters:** a run that could trust would launder consent through its own handle. The
hosted MCP tools follow 060's `team` rule for each, checked against its handler. These land
in one commit before anything below, with the tests in "The doors" under Acceptance criteria.

**1. The board read carries assignment, the plan request and the viewer's consent.** A
D12 amendment, following 037's pattern exactly: `list_tasks` runs `TASK_SUMMARY_SELECT`,
then a fixed number of batched reads keyed by task id, then Rust functions per task in
memory. No statement runs inside a per-task loop. `TaskSummary` and `TaskDetail` (Rust and
`src/types.ts`) gain three fields, built by the same functions for both, so the card and
the panel cannot disagree:

```rust
pub struct UserRef { pub id: UserId, pub login: String, pub avatar_url: Option<String> }

pub struct CardConsent {
    pub missing: Vec<MissingPiece>, // task-level pieces only; see below
    pub forbidden_by_team: bool,    // 045's team ceiling, never true in a personal team
}

pub struct MissingPiece {
    pub kind: ContentKind,              // 045's enum
    pub task_id: Option<TaskId>,        // the dependency, for BaseCommit
    pub revision: String,               // exactly as accept_content takes it
    pub author: Option<UserRef>,        // None: a deleted account (045's Piece.author)
    pub written_during_run: bool,
    pub run_id: Option<RunId>,          // BaseCommit: 044's BaseDependency::run_id;
                                        // ReviewFindings: the run whose text it is
    pub dependency_title: Option<String>, // BaseCommit only
}

// on TaskSummary and TaskDetail:
pub assignee: Option<UserRef>,
pub strategy_requester: Option<UserRef>, // 060's strategy_requested_by, while pending
pub consent: Option<CardConsent>,
```

- **The pieces are 045's, over the purposes the task's next claim can take.**
  `consent::board::card_consent` calls 045's `pieces_for` and `consents` for each purpose
  in `consent::next_purposes(task)`, takes the union, and drops `BaseInstructions` and
  `ReviewInstructions`. `next_purposes` lives in 045's module and returns:
  - `implementation`, always;
  - `review`, when the task's effective review loop (037's `review_loop`) is enabled,
    because 045's claim checks the review's pieces at the `Continue` that starts it. That
    is also where the effective review instructions come from: `pieces_for(Review)` applies
    021's override rule, and this task never re-implements it;
  - the newest run's kind, `review` or `fix`, when the task is `waiting_retry`, because
    021 resumes it "as the same kind". This is how a fix moved to another runner shows
    the findings it would act on;
  - `strategy`, while 060's request is pending.

  045's `status` takes its purposes from the same function. If 045 wrote `status` over a
  different set, change it to call `next_purposes`, with 045's tests still passing.
- **The inputs are 045's loader, batched.** If 045's loader for `pieces_for`'s inputs
  reads one task at a time, generalise it to a list of task ids in 045's module, one
  loader with two callers, and do not write a second copy here. The base commit comes from
  044's `choose`, which takes dependencies paired with their `SuccessfulHead` and runs no
  git, over D29 point 5's implementation and fix rows. Beside it: one read of
  `tasks.assignee_id` and `strategy_requested_by` joined to `users`, one of the viewer's
  acceptances and trust rows in the team, one of the viewer's runners with their
  `runner_pool_teams`, and one of the team ceiling for every listed repository.
- **`consent` is the viewer's, and only when one of the viewer's runners would run the
  task.** For each of the viewer's paired runners, call 045's `eligibility::decide(task,
  runner, pool_teams)`. `consent` is `Some` when any returns `Assigned` or `Pool`, and
  `None` otherwise, including for a viewer with no runner. Calling `decide` keeps a stale
  pool row under the `assigned` policy from showing a card the claim would never offer.
  Whether a teammate has accepted or trusts something is theirs: 045's `list_trusted`
  returns the actor's own list and nobody else's, and a card that said "waiting for Bob to
  accept Alice's plan" would publish Bob's trust list one card at a time.
- **Team-wide pieces are not on the card.** Base instructions and the team's review
  instructions reach every task in the team at once. On each card they would turn a column
  into a wall of one repeated sentence. They go to the banner (Scope 4).
- A TypeScript or SQL copy of `pieces_for`, `consents`, `decide` or 021's override rule
  is a finding.

**2. Assignment.** `src/components/board/AssigneePicker.tsx` is a menu listing
`Unassigned (team pool)` first, then the current team's members from 051's
`list_team_members` by login, the viewer marked `(you)`. The board shows one team (050's
switcher), so the board reads `list_team_members` once for the current team and re-reads it
on 051's membership events. That list is also the member count: the picker does not render
when the team has one member.

- The picker opens from the card's row actions, revealed on hover and on `:focus-within`
  (ADR-0024 rule 5), and from an `Assignee` field in `TaskDetailPanel`. Choosing calls
  045's `assign_task` once.
- A card whose task is assigned shows the assignee's avatar and `@login` as plain text, not
  a pill (ADR-0024 rule 3's spirit: a name is not a state). An unassigned card shows
  nothing.
- The new-task form gains the same Assignee field, sending 060's `assigneeId` on
  `create_task`, and nothing when the pool is chosen.

**3. Why the viewer's runners will not run it.** Pure functions in a new
`src/lib/consent.ts` produce every string, so the wording is tested once and rendered on
the card, in the panel and in Settings.

- **`consentReasonText(piece)`**, exact strings, with `@alice` standing for the author's
  login and `a former member` replacing it when `author` is `None`:

  | Kind | Text |
  | --- | --- |
  | `plan` | `@alice changed the plan (revision 4)` |
  | `task_review_instructions` | `@alice changed this task's review instructions (revision 2)` |
  | `base_commit` | `Starts from commit 1a2b3c4 of "Add the parser", by @carol` |
  | `review_findings` | `Review findings from @carol's runner` |
  | `base_instructions` | `@alice changed the team's base instructions (revision 7)` |
  | `review_instructions` | `@alice changed the team's review instructions (revision 3)` |

  A piece with `written_during_run` inserts ` during a run` after the verb phrase, for
  example `@bob changed the plan during a run (revision 5)`. The commit is its first seven
  characters. 045 lists one `BaseCommit` piece per distinct author of the same commit, and
  one acceptance covers them all, so pieces of one kind, task and revision are one line
  and one button, with authors joined: `by @carol and @dan`, `by @carol, @dan and @erin`.
  `forbidden_by_team` reads `The team does not allow unattended runs in this repository`.
- **On the card**, one line, only where a claim takes the card: the `ready` column (the
  only column the queue reads, ADR-0010) and a `waiting_retry` card. It reads `Not
  runnable for you: ` followed by the first reason, then ` · 2 more` when there are others.
  Its one button, `Review`, opens the panel scrolled to the consent section. **The card
  never accepts anything itself** (Notes, decisions).
- **In the panel**, a new `src/components/panel/ConsentSection.tsx` above
  `RunHistorySection`, rendered in every column whenever `consent` has a missing piece or
  is forbidden. Each missing piece shows its reason, the content it would accept, and one
  button, `Accept revision 4` (or `Accept this commit`, or `Accept these findings`):
  - **plan:** the plan and extra instructions as the panel already renders them;
  - **task review instructions:** 037's override editor, read-only here;
  - **base commit:** the full SHA as copyable monospace (ADR-0024 rule 2), the dependency's
    title, and `Open its review`, which opens the piece's `run_id` in `RunDetailOverlay`
    with its 033 bundle. That is the run whose `head_sha` is the commit being accepted, not
    the dependency's newest run, which may be a review or a later failed attempt;
  - **review findings:** 037's `OpenFindingsList` for the piece's `run_id`.
- **One click calls 045's `accept_content` once**, with `{ teamId, taskId, kind, revision }`
  exactly as the read gave them. 045 accepts only the current revision, and refuses a
  stale one as `invalid` with its sentence (`revision {given} is not current: …`). That
  refusal renders verbatim as the one error type (D8), and the section re-reads. After the
  re-read the piece shows its new revision and its own button, or is gone if the newer
  content now consents or no longer exists. It never retries with the new revision on the
  person's behalf.
- **A written-during-run piece** shows one more sentence under its reason:
  `Trust does not cover changes made during a run, so this needs your acceptance.`
  (ADR-0032 point 6: it "never counts as trusted for anyone").

**4. The instructions banner.** A new board read, `get_team_consent`, answers the viewer's
missing team-wide pieces in the current team:

```ts
interface TeamConsent {
  teamId: string;
  missing: MissingPiece[]; // BaseInstructions and ReviewInstructions only
}
```

It calls 045's `consents` on the team's pieces. `BaseInstructions` is a candidate whenever
the team has base instructions. The team's `ReviewInstructions` is a candidate only when
some non-archived task's `pieces_for(Review)` lists it, meaning that task's loop is on and
its override is blank, read with Scope 1's batched loader. Instructions no run reads are
not a reason anything waits. In a personal team `missing` is always empty.

`src/components/board/InstructionsConsentBanner.tsx` sits above the board, in every mode
but solo, when `missing` is non-empty. One line per piece, from `consentReasonText`,
followed by `Your runners will not start tasks in this team until you accept it or trust
@alice.` (for a written-during-run piece: `until you accept it.`). Its one button,
`Review`, opens Settings → Instructions, where the text is on screen, and where
`InstructionsSection` and 037's `ReviewSection` show the same reason with an
`Accept revision 7` button beside the text. Accepting calls `accept_content` with
`taskId: null`. The banner cannot be dismissed: it is the reason the board is idle, and it
goes away when the reason does. It re-reads on `settings` and `tasks` change events (D7)
and after the viewer's own acceptance or trust change.

There is no diff of what changed. `team_settings` holds only the current value. The banner
names who changed it, and the text is one click away.

**5. Trust and eligibility in Settings.** A new `src/views/settings/TeamWorkSection.tsx`,
titled `Runners and trust`, rendered in every mode but solo. It is the one place both are
edited.

- **Trust, for the current team.** Rendered when the current team has a member besides the
  viewer, from the same `list_team_members` read. Every member but the viewer, with a
  checkbox `Trust @bob's changes`, checked from 045's `list_trusted_authors`. Above the
  list, exactly: `Changes a trusted teammate makes to plans, instructions and code in this
  team run on your runners without asking you. A change they make during a run still asks.
  Nobody else can see or change this list.` Toggling calls `set_author_trust` once with
  `{ teamId, userId, trusted }`. Neither direction is behind a confirmation: the sentence
  says what both do.
- **Eligibility, per runner.** For each of the viewer's runners (050's `list_runners`),
  two radios: `Only tasks assigned to me` and `Tasks assigned to me, then unassigned tasks
  in:`, the second with one checkbox per non-personal team from 050's `list_teams`. The
  personal team is not listed, because its unassigned tasks are already the owner's own
  (045's `decide`). The checkboxes are disabled under the first radio. Saving calls 045's
  `set_runner_eligibility` once, with the policy and the complete list of checked team ids,
  because 045 replaces the list whole. Under `Only tasks assigned to me` the list sent is
  empty, whatever is still checked. `list_runners` gains `poolTeamIds`, read from
  `runner_pool_teams` and limited to teams the caller belongs to, so the form starts from
  the stored state. Rendered when `list_teams` has a non-personal team.
- 050's `RunnersSection` replaces its read-only `eligibility` with a link,
  `Change in Runners and trust`, to this section. Eligibility lives on the board (ADR-0032
  point 2), so it is editable in the browser too.

**6. Plan in the browser is a request.** The planning commands stay local (D32's 2026-10-04
amendment on them), so 050's gates on Plan, Re-plan and the pass stay. Where there is no
local runner, `StrategySection`'s Plan and Re-plan call 060's `request_task_strategy` once
with `{ taskId }`, and `PlanPassPanel`'s start calls `request_tasks_strategy` once with
`{ selection }` and lists each card's `requested` or `skipped` outcome with 060's sentence,
with no progress and no Cancel. A card or panel with `strategy_requester` set shows `Plan
requested by @bob · 5m ago`, from `relativeTime` in `src/lib/board.ts`, and the button
reads `Plan requested`, disabled. Where there is a local runner, both are unchanged.

There is no withdraw command. 060's selection drops a request once the task no longer
qualifies (its point 5), so changing the strategy mode withdraws it, and a planner costs
cents.

**7. Doors.** One new command, as the registry requires (D32). `get_team_consent`: a board
`Read` row, a core handler under `crates/core/src/api/board/`, a `board<T>` wrapper, an MCP
tool of the same name taking 060's `team` argument, refused on the run-scoped surface
(D30), 046's per-command case and a two-team case (039's registry test), and a D32 appendix
row. Every other command this task calls exists already, and this task changes no shape
except the additive fields: `poolTeamIds` on `list_runners`, and Scope 1's three.

**8. Fixtures and screenshots** (028's mechanism, 049's per-scenario capabilities). The
fixture table gains a row for every command this task adds or starts calling, including
`list_team_members`, `request_task_strategy` and `request_tasks_strategy`. New scenarios:

| Scenario | Seeds | Views |
| --- | --- | --- |
| `team` | connected capabilities with a local runner "MacBook"; viewer `@alice` in a personal team and in "Platform" with `@bob` and `@carol`. Cards: one assigned to each member; one with a plan changed by `@bob`; one changed during a run; one starting from a commit by `@carol` and `@dan`; a `waiting_retry` fix with findings from `@bob`'s runner; one with a plan requested by `@bob`; a repository the team forbids. The base instructions changed by `@carol` | board with the banner; board with the assignee picker open; a panel with three missing pieces; a panel after a stale acceptance; Settings → Runners and trust |
| `team-browser` | as `team`, with browser capabilities | board; a panel with a missing piece; the plan pass's outcomes; Settings → Runners and trust |

069 extends both scenarios' seeds with runners, holders and pins. `busy` and every existing
scenario are unchanged. Run `npm run screenshot -- --label before` before the first
frontend commit and compare after the last: every `busy` capture must be unchanged. Look at
the new captures before finishing, as 028 requires: overflow of long logins, wrapping of the
reason line on a narrow card, and contrast in both schemes.

**9. Records.**

- Seam contract: a D12 amendment, "the summary carries assignment, the plan request and
  the viewer's consent", in the voice of its 2026-08-28 amendment and 037's. It states the
  three fields, the batched reads, `next_purposes`, and why a copy of 045's rule in SQL or
  TypeScript was refused.
- Seam contract: a new entry under the next free D number, "Task 061's cross-cutting
  choices", in the four-part shape, recording the decisions in the Notes.
- D32's appendix gains `get_team_consent`'s row. The Binds lines of D12, D28 (whose
  consent and membership tables this task reads) and D32 name 061, and "How to use this"
  gains 061's row.

## Out of scope

- **Any rule.** Eligibility, consent, the ceiling and every refusal message are 045's. If a
  refusal's wording is wrong, change it in the service with its test, never by rewording it
  in the view.
- **Everything listed for 069** in the Goal.
- **The queue's skip labels.** 045's Scope 11 wrote `QUEUE_SKIP_LABELS` for its three
  variants. This task does not touch them.
- **A card refused for a reason with no `SkipReason`**: 043's pin, 067's model rule and
  this machine's strategy ceiling. 045 gave them none for D23 point 4's reason: another
  runner can take the card with nobody acting. A ceiling that passes a card on to another
  machine is the cost control working, and Run now on this machine still answers with
  045's ceiling sentence.
- **Any migration**, and any dependency (D4, D28's D4 amendment, D34).
- **A revision history, or a diff between revisions.** No column holds an old plan or old
  instructions.
- **Filtering the board by assignee.** `planMove` computes positions between visible
  neighbours, and a filtered column would place a dropped card between two cards that are
  not adjacent. That needs its own design.
- **Seeing a teammate's trust list or acceptances** (Scope 1). Trust is personal
  (ADR-0032 Consequences).
- **The `claude mcp add` line** (060), unless 060's cut moved it here (Notes).
- **Notifications** of a new revision to accept. The banner and the card are the
  notification.

## Acceptance criteria

Rust tests use the real SQLite harness, 039's `TwoTeams` and 038's builders, and the fake
`Clock`. They contain no `sleep`. Frontend tests are vitest, mock at
`@tauri-apps/api/core` and at 049's HTTP mock, never at the wrappers, and assert exact
command names, arguments and strings.

- **The doors** (Scope 0): `a_run_cannot_trust_through_its_handle`, `set_author_trust` refused
  on the run-scoped route for every grant; `every_registered_tool_has_a_run_scope_decision`
  and 039's registry test cover all three; 028's fixture coverage test passes with their rows,
  `fixtures.test.ts` unmodified; and D32's appendix lists them.
- **The board read**, in a new `crates/core/tests/team_board.rs`:
  - `the_card_and_get_task_consent_agree_on_what_is_missing`: for every listed task whose
    `consent` is `Some`, `missing` equals `get_task_consent`'s missing pieces less the two
    team-wide kinds, for each of the viewer's runners for which `decide` answers `Assigned`
    or `Pool`. Across a plan edit, a written-during-run edit, a dependency commit by two
    other owners' runners, a task review-instructions override with the loop on (a piece)
    and off (no piece), and a pending strategy request;
  - `a_waiting_fix_carries_the_findings_of_another_runner`: a `waiting_retry` fix whose
    review ran on `@bob`'s runner has a `ReviewFindings` piece whose `run_id` is that
    review;
  - `a_base_commit_piece_names_the_run_044_chose`: `run_id` is `BaseDependency::run_id`,
    while the dependency's newest run is a later review;
  - `a_card_assigned_to_someone_else_carries_no_consent`;
  - `an_unassigned_card_carries_consent_exactly_when_decide_admits_a_viewers_runner`,
    including a stale pool row under the `assigned` policy (`None`), and a viewer with no
    runner (`None`);
  - `team_wide_pieces_are_on_the_team_consent_read_and_never_on_a_card`;
  - `a_board_mixing_consented_and_unconsented_cards_keeps_each_cards_own`, which catches a
    batched read keyed by the wrong id;
  - `the_card_names_its_assignee_and_who_requested_a_plan`;
  - `a_solo_board_has_nothing_missing_and_no_assignee`;
  - `list_tasks_issues_as_many_statements_for_fifty_cards_as_for_five`: the harness counts
    the statements a `list_tasks` call issues, from sqlx's `sqlx::query` tracing events
    captured by a counting subscriber written in the harness (no new dependency). If they
    cannot be captured that way, stop and ask rather than drop the test.
- **The team consent read:**
  `a_base_instructions_edit_by_an_untrusted_teammate_is_missing_for_the_team`,
  `trusting_the_editor_clears_it_unless_it_was_written_during_a_run`,
  `review_instructions_are_missing_only_when_a_loop_reads_them`, which includes a team
  where every loop-enabled task overrides them (nothing missing), and
  `a_personal_team_has_nothing_missing`.
- **Doors:** 046's `every_board_command_has_a_case`, `a_team_cannot_see_another_teams_ids`
  and `both_transports_answer_every_case_identically` pass with the new row.
  `every_registered_tool_has_a_run_scope_decision` covers `get_team_consent`, and
  `the_team_consent_read_is_refused_to_every_run_grant` is in
  `crates/core/tests/mcp_scope.rs`. `./scripts/check-command-wiring.sh` passes.
- **Strings.** `src/lib/consent.test.ts` asserts every row of Scope 3's table, the
  former-member form, the during-a-run form, and the joined base-commit authors for one,
  two and three.
- **The card** (`TaskCard.test.tsx`):
  - `it("shows the assignee's login and nothing when unassigned")`;
  - `it("shows why a ready or waiting card will not run, with the first reason and a count")`;
  - `it("shows no consent line in any other column")`;
  - `it("never calls accept_content from the card")`: clicking `Review` opens the panel and
    sends no command;
  - `it("says who requested a plan")`;
  - `it("renders none of the new lines in solo mode")`.
- **The assignee picker** (`AssigneePicker.test.tsx`): `Unassigned (team pool)` first, then
  members by login with `(you)`; choosing sends exactly one `assign_task` with the task id
  and the user id, or `null`; it is reachable and operable by keyboard alone; it does not
  render in a one-member team; after a `tasks` change event from a member's removal (051),
  the card shows no assignee.
- **The new-task form:** choosing a member sends `create_task` with that `assigneeId`, and
  the pool sends none.
- **The consent section** (`ConsentSection.test.tsx`):
  - each piece renders its content beside its button, as Scope 3 lists, and two
    base-commit pieces of one commit render one line and one button;
  - one click sends exactly one `accept_content` with `{ teamId, taskId, kind, revision }`
    as read, and the section re-reads;
  - a stale-acceptance refusal renders 045's sentence verbatim as the one error type,
    sends no second `accept_content`, and re-reads; after the re-read the piece shows the
    new revision's button, or is gone when the re-read no longer lists it;
  - a written-during-run piece shows the trust sentence exactly;
  - `Open its review` opens the piece's `run_id`, not the dependency's newest run.
- **The banner** (`InstructionsConsentBanner.test.tsx`): it renders one line per missing
  piece with the exact sentence; it has no dismiss control; `Review` opens Settings →
  Instructions; accepting there sends `accept_content` with `taskId: null`; it disappears
  after the re-read; it never renders in solo mode or with nothing missing.
- **Settings** (`TeamWorkSection.test.tsx`): the trust sentence is exact; the viewer is not
  listed; toggling sends `set_author_trust` with `{ teamId, userId, trusted }`; the
  personal team has no pool checkbox; saving sends `set_runner_eligibility` once with the
  policy and the whole team-id list, and under `Only tasks assigned to me` with an empty
  list although a team is still checked; it does not render in solo. `RunnersSection`
  links to it and has no eligibility control of its own.
- **Plan in the browser:** Plan sends one `request_task_strategy` with `{ taskId }`; the
  pass sends one `request_tasks_strategy` and lists each outcome's sentence; a requested
  task's button is disabled and reads `Plan requested`; with a local runner, Plan still
  sends `plan_task_strategy` and nothing else changes.
- **Nothing else moves.** Every pre-existing frontend test passes without an edit to its
  assertions, and 050's `it("sends no local command from any view in browser mode")`
  passes with the new sections rendered. No component checks the platform. Every choice
  comes from 049's capabilities.
- **Screenshots.** Scope 8's scenarios exist, and `npm run screenshot` produces them in both
  schemes and both widths. The PR body lists the captures inspected and the before/after
  comparison of `busy`, with a human checklist: long logins at the narrow width, and the
  reason line wrapping on a card.
- **Records.** The D12 amendment, the new D entry, D32's appendix row, the three Binds lines
  and the "How to use this" row exist. The PR body flags the deviation from the brief
  (Notes, decisions) for the product owner.
- **No migration**, and `package.json` and every `Cargo.toml` gain no dependency. Every new
  or changed query has its entry regenerated in the matching offline cache with D33's
  recipe, in the same commit.
- Every CI check passes, exactly as CLAUDE.md lists them at the time this lands, including
  the runner and server crates' steps.
- **Needs a person; the PR body carries it as a checklist:** against a local
  `rimaia-server` with two users and one runner each, assign a card to the other user and
  see it run only there; edit their plan and see their card say why; accept it from their
  panel and see it start; change the base instructions and see the other user's banner.

## Notes

**Read first.** ADR-0032 in full and ADR-0024 (every rule applies to every new line and
control). Then the seam entries:

- **D12** and all its amendments, especially 037's, which this task's amendment follows.
- **D28**: 045's consent DDL, 038's `users`, 051's memberships, and 043's request columns.
- **D29** and its 2026-10-04 amendment: the implementation and fix rows a base commit
  comes from.
- **D30**: the run-scoped surface the new tool is refused on.
- **D32**: the registry, `board` rows, the appendix, and the amendment on the planning
  commands.
- **D33**: the caches. **D7**, **D8**, **D10**, and **D4** and **D34** as prohibitions.
- 045's, 050's, 051's and 060's own seam entries, whatever numbers they took.

**Files to start from.**

- Core: `crates/core/src/tasks/service.rs` (`TASK_SUMMARY_SELECT`, `list_tasks`, where 037
  put its batched reads); 045's `crates/core/src/consent/`, where `board.rs` goes;
  044's `worktree/base_ref.rs` (`choose`); `crates/core/src/mcp/scope.rs`; 046's
  `crates/core/src/api/` registry and `api/board/`.
- Frontend logic: `src/lib/board.ts` (`relativeTime`), `src/lib/commands.ts`,
  `src/lib/events.ts`, 049's capabilities module, `src/types.ts`.
- Board and panel: `src/components/board/TaskCard.tsx`, `TaskDetailPanel.tsx`,
  `Board.tsx`, `PlanPassPanel.tsx`; `src/components/panel/RunHistorySection.tsx`,
  `PlanEditor.tsx`, `ExtraInstructionsEditor.tsx`, `StrategySection.tsx`;
  `src/components/runs/RunDetailOverlay.tsx`.
- Settings: `src/views/SettingsView.tsx`, and in `src/views/settings/`:
  `InstructionsSection.tsx`, 037's `ReviewSection.tsx` and 050's `RunnersSection.tsx`.
- Fixtures: 028's `src/dev/fixtures/` and `scripts/screenshot.mjs`.

**Migration:** none.

**What the chain provides.**

- **045:** `assign_task`, `accept_content`, `get_task_consent`; the services behind
  `set_author_trust`, `list_trusted_authors` and `set_runner_eligibility`, whose doors are
  Scope 0's; `pieces_for`, `consents`, `decide` and the input loader; the refusal sentences;
  `Task`'s authorship fields.
- **044:** `choose` over rows, and `BaseDependency::run_id`.
- **021 and 037:** the override rule inside `pieces_for(Review)`, `review_loop` on the
  summary, `OpenFindingsList`, `ReviewSection`.
- **050:** `list_teams`, `list_runners`, the switcher, `RunnersSection`, and the gates on
  the planning commands.
- **051:** `list_team_members`, its membership events, and unassignment on removal.
- **059:** the connected desktop, and a `localRunnerId` that is not the solo runner.
- **060:** `request_task_strategy` and `request_tasks_strategy` with their `board<T>`
  wrappers, `Task.strategyRequestedAt` and `strategyRequestedBy`, `create_task`'s
  `assigneeId`, and the `team` argument on MCP tools.

`depends_on` names 045, 059 and 060. 060 is ordered after 059 and depends on neither, so it
is named. Every other task above is ordered before 059. If any of these is not where this
file says, stop and ask rather than build a second copy.

**If a neighbour took its cut.** 045's second cut fired: the doors for trust and
eligibility are Scope 0. The team ceiling's door stayed in 045, and the strategy ceiling's
commands went to 072. 060's first cut moves `request_task_strategy` and
`request_tasks_strategy` here, as 060's Scope 5 describes them, with their registry,
fixture and appendix rows. Its second moves the token dialog's `claude mcp add` line here.
Each arrives with that task's tests and strings, unchanged.

**Decisions this file makes, which the new D entry records.**

- **Consent is shown to the person whose runners would run the task, and to nobody
  else**, decided by 045's `decide`. Anything wider leaks a personal trust list.
- **The card shows what the next claim reads**, through `next_purposes`, not only the
  implementation's pieces. Otherwise a fix moved to another machine waits on findings
  nobody is shown.
- **Team-wide pieces go to one banner.** Each card would otherwise repeat one sentence.
- **Acceptance happens where the content is visible.** The brief asked for one-click
  accept on the card. It is one click, from the panel the card's `Review` opens, because a
  card shows a title and not the text being accepted, and an acceptance is a statement
  that the person read that revision. This is this task's decision, not 045's rule, so
  the PR body flags it for the product owner, who can move the button with an amendment to
  the D entry.
- **No withdraw command** for a plan request (Scope 6).

**What the next tasks expect.**

- **069** adds its batched reads beside Scope 1's under its own D12 amendment, and extends
  this task's fixture scenarios.
- **062** watches the new reads' cost in its metrics, and needs nothing else here.
- **064** documents the screens in the final pass and adds 061 to CLAUDE.md's
  must-have-tests list only if a rule moved here, which it must not.

**Size.** L: roughly 450 lines of core reads and their tests, 150 for the door, 1,100 of
components, 900 of frontend tests and 250 of fixtures, about 2,900 lines before `.sqlx/`. If
it runs over, stop and propose the next split rather than trimming tests. Never cut the
card's reason line, the acceptance flow or the banner. They are the property this task
exists for.
