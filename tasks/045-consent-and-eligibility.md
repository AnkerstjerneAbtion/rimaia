---
id: "045"
title: Consent and eligibility
milestone: v0.5
status: ready
depends_on: ["039", "043", "044"]
adrs: ["0032", "0012", "0009"]
size: L
---

# Consent and eligibility

## Goal

Make [ADR-0032](../docs/adr/0032-assignment-and-consent-to-run-on-a-machine.md) true in the
claim. After this task no runner claims, re-reads or continues a task unless four things
hold, and each is checked by one `rimaia-core` function that every door reaches:

- **Eligibility (point 2).** The task is assigned to the runner's owner, or it is unassigned
  in a team the runner takes pool work from. It is never assigned to someone else.
- **Consent (points 3 and 6).** The runner's owner wrote, accepted or trusts every piece of
  content the run would execute, at its current revision. A revision written during a run
  on someone else's content counts only when it was explicitly accepted.
- **The team ceiling (point 4).** The team allows unattended runs in the repository. The
  runner's own consent is still checked by the runner, in its own store, before it spawns.
- **The runner's strategy ceiling (point 3's last paragraph).** The model and effort the
  run would use are within what this runner allows. This is a cost control, not consent.

The prompt then states the two facts point 7 names: who wrote the plan revision being
executed, and whose machine and credentials it runs on.

**In solo nothing changes that anyone can see.** Every piece of content is the solo user's,
every task is in their personal team, the runner has no strategy ceiling, and a personal
team's prompt is byte for byte what it was. Every existing test passes unchanged.

## Why now

This is the last task of M2, and the one that makes a shared board defensible. Everything a
remote runner will do in M4 goes through the claim that 043 made one transaction and 044
made commit-based. Consent has to be inside that transaction before 052 puts a network in
front of it. Otherwise the HTTP adapter and its contract suite are written against a claim
that runs anyone's text on anyone's machine, and 052 inherits that.

The revision columns cannot wait either. Every edit made to a plan, the base instructions or
the review instructions without one is an edit nobody can be asked to accept later. From
this task on, every writer of consent-gated content records a revision, an author and the
written-during-a-run mark. Teams, invitations and connected runners arrive in 051 and later,
and by then every write has been recorded this way from the start.

## Scope

**1. The migration.** `src-tauri/migrations/20261003120200_consent.sql`, with exactly the
DDL of seam-contract D28 part 6 under that name: `tasks.created_by`, `assignee_id`,
`assigned_by`, the plan and review-instructions revision, author and written-during-run
columns, the same four facts on `team_settings`, the backfill to the solo user, and the
`acceptances`, `trusted_authors` and `runner_pool_teams` tables, plus
`runners.eligibility`. The header comment is this task's own, in the voice of the existing
migrations. Its first line is its title and never begins with `-- no-transaction`, and it
contains no `DELETE` (D28 part 1: nothing cascades inside a migration). It does not touch
`repositories.allow_unattended_runs`. That column already means the team ceiling (D28 part
6's comment on 038), and 041 copied it into `checkouts.unattended_consent`. A backfill here
would run before that copy on an install that upgrades across both, and would grant consent
nobody gave.

Regenerate both offline caches with D33's recipe (D33 names 045 among the tasks that must).

**2. Every writer of consent-gated content records a revision.** One helper per content
kind, and every write goes through it. There is no second `UPDATE tasks SET plan`.

- `tasks::create_task` writes `created_by = ctx.actor`, `plan_updated_by = ctx.actor`, and
  `review_instructions_updated_by = ctx.actor` when an override is given. Each
  written-during-run column gets the mark from point 4 below.
- `tasks::update_task`, when `plan` or `extra_instructions` **changes value**, increments
  `plan_revision` and sets `plan_updated_by` and `plan_written_during_run`. The same applies
  to `review_instructions` and its three columns. A patch that writes the same text again is
  not an edit. It bumps nothing, so it does not undo anyone's acceptance.
- The team-settings writer (039's) increments `team_settings.revision`, and sets
  `updated_by`, `updated_at` (from `ctx.clock`) and `written_during_run` on every key whose
  value changes. Consent reads only `base_instructions` and `review_instructions`, but one
  rule for every row is simpler than a list, and an audit of "who changed the catalogue" is
  free.
- MCP's `create_task` and `update_task` reach the same services, so a task written over MCP
  is written by the context's actor (ADR-0032 point 3, "Tasks created over MCP count as
  written by the token's user").
- `grep -rn "plan\s*=\|extra_instructions\s*=\|review_instructions\s*=" crates/core/src`
  lists every SQL writer. Each one either goes through the helper or is a test fixture.

**3. Assignment (point 1).** `tasks::assign_task(ctx, task_id, assignee_id: Option<UserId>)`
sets `assignee_id` and `assigned_by = ctx.actor`. The assignee must be a member of the
task's team: a user who is not a member is refused as `NotFound`, worded as for a user who
does not exist (039's rule). `None` returns the task to the pool. Publishes
`ChangeEvent::tasks` for the task's team. `Task` (Rust `db::models` and `src/types.ts`)
gains `createdBy`, `assigneeId`, `assignedBy`, `planRevision` and `planUpdatedBy`, all
optional on the TypeScript side. No component renders them (061 does).

**4. The consent module.** New: `crates/core/src/consent/` with `mod.rs`, `pieces.rs`,
`eligibility.rs` and `ceiling.rs`. Everything in `pieces.rs`, `eligibility.rs` and
`ceiling.rs` is a pure function over values, unit-tested without a pool. `mod.rs` holds the
queries, which run on a connection the caller passes in, so the claim evaluates consent
inside its own transaction.

- **`ContentKind`**, an enum in the `acceptances.content` `CHECK`'s spelling: `Plan`,
  `TaskReviewInstructions`, `BaseInstructions`, `ReviewInstructions`, `ReviewFindings`,
  `BaseCommit`. Not strings (CLAUDE.md).
- **`Piece { kind, task_id: Option<TaskId>, revision: String, author: Option<UserId>,
  written_during_run: bool }`**, and **`pieces_for(purpose, inputs) -> Vec<Piece>`**, which
  lists exactly the content the composer for that lease purpose reads:

  | Purpose | Pieces |
  | --- | --- |
  | `implementation` | plan · base instructions · base commit |
  | `strategy` | plan · base commit |
  | `review` | plan · the effective review instructions · base commit |
  | `fix` | plan · base instructions · review findings from another runner · base commit |

  The planner takes no base instructions (ADR-0009's 2026-08-28 amendment) and borrows the
  implementation's worktree, so its base commit is a piece. The effective review
  instructions are the task's override when it is non-blank, and the team's otherwise
  (021's rule). Findings are a piece only when a different runner ran the review.

  A piece whose content is empty or absent is not in the list: there is nothing to execute.
  The base commit is a piece only when 044's `RunContext::base` is a dependency's commit,
  with `task_id` the dependency, `revision` the full SHA, and `author` the owner of the
  runner recorded on the run that produced it. Review findings use the review run's id as
  `revision` and the owner of that run's runner as `author`.
- **`consents(owner, piece, accepted, trusted) -> Consent`**, the rule, in this order:
  1. The piece was written during a run: only an acceptance of exactly `(owner, kind,
     task, revision)` counts. **Its author's own runners included.** That is the case
     point 6 exists for: a plan Bob's credentials wrote while Bob's runner ran Alice's
     plan must not pass Bob's runners on authorship.
  2. The owner is the author: consents.
  3. An acceptance of exactly that revision exists: consents.
  4. The owner's trust list for the task's team names the author: consents. An author of
     `None` (a deleted account) is never trusted.
  5. Otherwise it is `Missing { piece, reason }`.
- **The written-during-run mark (point 6).** `written_during_run(conn, actor) -> bool` is
  true when a live `runner_leases` row belongs to a runner whose `user_id` is the actor,
  and `pieces_for` of that lease's purpose over that task names any author other than the
  actor. It reuses `pieces_for`, so "content authored by someone else" means the same thing
  in the mark and in the check. Ordinary planning is unaffected, including while the actor's
  runner works on their own tasks. In a personal team it is always false.
- **Trust.** `set_trust(ctx, team_id, user_id, trusted: bool)` adds or removes a
  `trusted_authors` row for `ctx.actor`. The trusted user must be a member of that team.
  Trusting oneself is refused as `Invalid` before the `CHECK` sees it. `list_trusted(ctx,
  team_id)` returns the actor's own list and nobody else's (ADR-0032 Consequences: trust is
  personal).
- **Acceptance.** `accept(ctx, task_id: Option<TaskId>, kind, revision)` records `(actor,
  kind, revision)` with `accepted_at` from `ctx.clock`. It accepts **only the current
  revision**. A stale one is `Conflict`, naming the current revision and who wrote it, so
  nobody accepts text they did not see. Accepting twice is idempotent. `task_id` is `None`
  exactly for the two team-wide kinds, mirroring the table's `CHECK`.
- **`status(ctx, task_id, runner_id) -> TaskConsent`**, the read 061 draws the card from:
  eligibility, the team ceiling, and every missing piece with its kind, revision, author's
  login and reason. It is a DTO with camelCase fields and no path (ADR-0028 point 2's test
  covers it).

**5. Eligibility (point 2).** `eligibility::decide(task, runner, pool_teams) -> Eligibility
{ Assigned, Pool, NotEligible(Reason) }`:

- assigned to the runner's owner: `Assigned`;
- assigned to anyone else: `NotEligible(AssignedToSomeoneElse)`, whatever the policy;
- unassigned, in the runner owner's **personal** team: `Assigned` ("in a personal team
  every task is the owner's own", which is why the migration does not backfill
  `assignee_id`);
- unassigned, policy `assigned_then_pool`, and the task's team is in `runner_pool_teams`:
  `Pool`;
- otherwise `NotEligible(Unassigned)`.

`RunnerEligibility { Assigned, AssignedThenPool }` is an enum in the column's spelling.
`set_runner_eligibility(ctx, runner_id, policy, pool_team_ids)` is refused as `NotFound`
unless the runner belongs to `ctx.actor`, and every pool team must be one the owner is a
member of. The pool list is replaced whole, in one transaction.

**6. The claim applies all of it, in 043's one transaction.** In `board::service`, for every
`ClaimTarget`:

- **`Next`** considers only eligible tasks whose pieces all consent, in a repository the
  team ceiling allows, and within the runner's strategy ceiling. It takes every `Assigned`
  task in board order before any `Pool` task ("the above first, then the pool"). A task
  that fails is skipped, not failed. `selection::SkipReason` gains `NotEligible`,
  `ConsentMissing` and `ForbiddenByTeam`, and the TypeScript union matches, so the queue
  says why a card waits (ADR-0032 Consequences: never "silently skipped").
- **`Run` and `Plan`** return an `Err` whose message a person reads, naming the first thing
  that fails. They never return `None` for a consent or eligibility failure: `None` means
  "someone else got there first" (D31 point 4).
- A pin (043) is checked as well, not instead: a pinned task that was reassigned is
  claimable by nobody until "run elsewhere" (057) releases the pin, and `status` says both.
- **`preview`, `run_context`** and the claim that `finish_run`'s `NextStep::Continue`
  implies re-check consent for the lease's purpose (D31 point 6). If a piece no longer
  consents, `preview` answers the same refusal the claim would, `run_context` answers
  `Conflict`, and `Continue` becomes `Released`. So a plan edited between the claim and the
  composition is never composed.
- **The team ceiling.** `repo::ensure_team_allows_unattended_runs(repository, team)` reads
  `repositories.allow_unattended_runs`, **except in a personal team, where it is not
  consulted.** There the owner of the team and the owner of the only machine are one person,
  so the runner's consent is the whole decision. That keeps solo's single toggle, and
  repositories registered after 041 (column default `0`) still run in solo. It applies to
  every run, as ADR-0012's opt-in does today (`src-tauri/src/commands/runs.rs` checks it for
  Manual runs too).
- `set_repository_unattended_ceiling(ctx, repository_id, allowed)` is the new board command
  D32 promises for 045. It requires the `Owner` role in the repository's team (ADR-0029's
  roles table). A member is refused as `Invalid`. This is the first role check, and 051
  generalises it. On a personal team the command is refused as `Invalid`, saying the
  machine's own consent decides there. `set_repository_unattended_runs` stays the runner's
  consent, and stays local.

**7. The runner re-checks its own consent before it spawns (point 4).** The runner has
already claimed, composed and prepared. At the last point before the agent process
starts, in the same process that spawns it, it reads `checkouts.unattended_consent` (041)
and its strategy ceiling again. If either now refuses, it `release`s, publishes the reason
as the run's refusal, and spawns nothing. `ClaimTarget::Next.repositories` lists only
checkouts with consent, so the board never offers the queue a repository this machine has
not opted into. The re-check exists because the board is not trusted to have honoured that
list.

**8. The runner's strategy ceiling.** A runner setting in `runner.db`'s `runner_settings`
(ADR-0028 point 2's table), key `strategy_ceiling`, JSON:

```rust
pub struct StrategyCeiling {
    pub models: Option<Vec<String>>, // allowed model ids; the first fills an absent model
    pub max_effort: Option<String>,  // an effort id; ranks by the catalogue's order
}
```

Absent means no ceiling, which is every existing install. The rule, as
`ceiling::judge(strategy, ceiling, catalogue) -> Result<CeiledStrategy, CeilingExceeded>`:

- **A named choice is never silently changed.** A model the card, the repository or the
  team names that is not in `models`, or an effort that ranks above `max_effort`, makes the
  task not claimable by this runner (skipped in `Next`, refused for `Run`/`Plan`). Efforts
  rank by their position in the team's catalogue, which lists them cheapest first. An effort
  the catalogue does not list exceeds every ceiling.
- **An absent choice is filled.** No model anywhere means the first of `models`. No effort
  means `max_effort`. The filled half's `StrategyOrigin` is a new `RunnerCeiling` variant
  (Rust and `src/types.ts`), so the transcript and the panel do not claim the CLI chose it.
- It applies to every purpose: the effective strategy for implementation and fix, the
  planner budget for strategy, and 021's review-phase model and effort for review.

The claim cannot read `runner.db`, so the ceiling travels with the claim, the way ADR-0031
point 1 sends the provider: `ClaimTarget::Next`, `Run` and `Plan` each gain `ceiling:
StrategyCeiling`, which is `#[serde(default)]`. This adds one field to D31 point 2's
`ClaimTarget`, and the seam entry in scope point 11 records it. Local commands
`get_strategy_ceiling` and `set_strategy_ceiling`, plus a local MCP tool through 041's
host-injected surface, read and write it.

**9. What the agent is told (point 7).** `RunContext` gains `authorship:
Option<RunAuthorship>`, filled board-side from the lease's runner:

```rust
pub struct RunAuthorship {
    pub plan_revision: i64,
    pub plan_author: Option<String>, // login; None for a deleted account
    pub runner_owner: String,        // login
    pub runner_label: String,
}
```

It is `None` in a personal team, where every author and the machine's owner are one person
and the facts would say nothing. `runner::prompt`'s shared `task_context`, which every
composer with a `# Task context` section uses, gains a parameter for it. When it is `Some`,
two lines follow `- Base ref:` and come before `- Links:`, exactly:

```
- Plan revision: 4, written by @alice
- Running on: @bob's runner "Mac mini", with @bob's credentials
```

A deleted author renders as `written by a former member`. Nothing goes into
`--append-system-prompt`, because these facts describe the situation, and are not rules the
agent must obey (ADR-0009). `compose_resume_prompt` and 021's continuation are unchanged,
because a resumed session already holds them. The persisted `runs.prompt` carries the lines,
which is what makes the transcript honest.

**10. Doors (ADR-0021 parity, ADR-0006).** Board commands in `src-tauri/src/commands/`,
registered in both `generate_handler!` lists, wrapped in `src/lib/commands.ts`, and exposed
as MCP tools with the same names:

- `assign_task`
- `accept_content`
- `set_author_trust` and `list_trusted_authors`
- `set_runner_eligibility`
- `set_repository_unattended_ceiling`
- `get_task_consent`

Each is a thin adapter over the service above. Every one of them is refused to runs in
`Tool::run_access`. **`accept_content` and `set_author_trust` are the ones that matter:** a
run that could accept or trust would launder consent through its own handle. Each is added
to 039's two-team registry test and to D32's appendix (D32 point 8). A board command's
refusal crosses the boundary as the one error type (D8).

**11. Records.**

- Seam contract: a new entry under the next free D number, "Task 045's cross-cutting
  choices", in the four-part shape. It records the refinements this file makes:
  - the personal-team rules: no ceiling, no authorship facts, and the unassigned task that
    is the owner's own;
  - a revision only for a changed value;
  - the ceiling's shape, and the `ClaimTarget.ceiling` field D31 point 2 gains;
  - findings and commits trusted like any authored piece.
- ADR-0032 gains a one-line pointer to that entry under points 4 and 7. No decision in the
  ADR is edited.
- CLAUDE.md: "consent, eligibility and the strategy ceiling" joins the list of modules that
  must have tests.

## Out of scope

- **Any interface.** The assignee picker, the consent banner, the trust list, the runner
  policy and the ceiling controls are all 061's. This task ships the services, doors and
  DTOs that 061 draws.
- **Point 6's first bullet.** Stripping Rimaia tokens from a run's environment and denying
  MCP servers by URL is 055's. This task writes only the written-during-run mark.
- **Inviting a second member and changing roles.** Tests add a member through a
  `testing`-feature helper that stands in for 051's invitation. 051 replaces the helper's
  body with the real service, and generalises the role check this task writes once.
- **Assignment on `create_task` and on the hosted MCP surface**, and the `team` argument:
  060.
- **Releasing a pin when a pinned task is reassigned:** 057's "run elsewhere".
- **Acceptance the runner can verify** (signatures): ADR-0032 point 4 leaves it undecided.
- **Moving `max_turns` or `disallowed_tools` to "the stricter of team and runner":** 042.
  The strategy ceiling is a separate setting with its own rule.
- **Any migration other than `20261003120200_consent.sql`.** If a column is missing from D28
  part 6, stop and ask (D28's D4 amendment).

## Acceptance criteria

- `src-tauri/migrations/20261003120200_consent.sql` exists under exactly that name, with D28
  part 6's DDL and no statement that touches `allow_unattended_runs` or deletes a row. It is
  the only new migration. Both offline caches are regenerated with D33's recipe and
  committed, and `SQLX_OFFLINE=true cargo check --workspace --all-targets` passes.
- `a_board_migrated_through_consent_attributes_everything_to_the_solo_user`: a board built
  at the pre-045 schema with 038's builder, then migrated. Every task has `created_by` and
  `plan_updated_by` equal to the solo user, `assignee_id` is `NULL`,
  `review_instructions_updated_by` is set exactly where an override exists, every
  `team_settings` row names the solo user, and `pragma_foreign_key_check` is empty.
- Unit tests in `consent/pieces.rs`, one per purpose, assert the exact piece list for a task
  that has every kind of content, and that an empty plan, a blank override and a base that
  is the default branch contribute no piece:
  `an_implementation_run_needs_the_plan_the_base_instructions_and_the_base_commit`,
  `a_planner_run_needs_no_base_instructions`,
  `a_review_run_reads_the_override_instead_of_the_teams_review_instructions`,
  `a_fix_run_on_the_runner_that_reviewed_needs_no_consent_to_its_own_findings`.
- Unit tests in `consent/mod.rs` for `consents`, one per rule:
  `the_author_consents_to_their_own_revision`,
  `an_acceptance_covers_exactly_one_revision`,
  `trusting_an_author_consents_to_their_next_revision`,
  `a_former_member_is_never_trusted`,
  `a_revision_written_during_a_run_needs_an_acceptance_even_from_its_author`.
- Service tests against the in-memory pool, with a faked clock and a shared team of two
  members:
  - `editing_a_teammates_plan_makes_it_unrunnable_for_them_until_they_accept`;
  - `saving_the_same_plan_again_is_not_a_new_revision`;
  - `base_instructions_edits_need_acceptance_from_members_who_do_not_trust_the_editor`;
  - `accepting_a_stale_revision_is_a_conflict_naming_the_current_one`;
  - `revoking_trust_stops_the_next_claim`;
  - `a_plan_bob_wrote_while_his_runner_ran_alices_plan_needs_his_own_acceptance`;
  - `planning_while_your_runner_works_on_your_own_task_is_not_marked`;
  - `a_dependency_commit_from_another_owners_runner_needs_trust_or_acceptance`;
  - `a_member_cannot_assign_a_task_to_someone_outside_the_team`.
- Eligibility tests:
  - `a_runner_never_runs_a_task_assigned_to_someone_else`, with both policies;
  - `an_unassigned_task_in_a_personal_team_is_the_owners`;
  - `the_pool_is_only_the_teams_the_runner_opted_into`;
  - `a_runner_takes_its_owners_tasks_before_the_pool`: a pool task above an assigned one in
    board order, and `Next` returns the assigned one first;
  - `a_reassigned_pinned_task_is_claimable_by_nobody`.
- Claim tests, through the in-process `BoardPort`, and added to D31 point 13's contract
  suite so 052's HTTP adapter inherits them:
  - `run_now_on_an_unconsented_task_refuses_with_the_reason` (exact message string);
  - `next_skips_an_unconsented_task_and_takes_the_one_below_it`;
  - `a_plan_edited_after_the_claim_is_a_conflict_on_run_context`;
  - `a_fix_phase_whose_findings_lost_consent_is_released_instead_of_continued`;
  - `a_team_that_forbids_unattended_runs_blocks_every_runner`;
  - `a_personal_team_ignores_the_ceiling_column`;
  - `a_member_cannot_set_the_team_ceiling`.
- Runner tests, with a real git repository in a `TempDir` and the fixture CLI stream:
  - `a_runner_refuses_to_spawn_where_it_never_consented_even_when_the_board_claims_it`: the
    fixture CLI is never invoked, and the lease is released;
  - `the_queue_offers_the_board_only_consented_repositories`.
- Ceiling tests, unit and claim:
  - `a_named_model_outside_the_ceiling_makes_the_task_unclaimable_here`;
  - `an_effort_above_the_ceiling_is_refused_never_lowered`;
  - `an_absent_model_and_effort_are_filled_from_the_ceiling`, with origin `RunnerCeiling`;
  - `an_effort_the_catalogue_does_not_list_exceeds_every_ceiling`;
  - `no_ceiling_changes_nothing`.
- Prompt tests assert whole strings (CLAUDE.md):
  - `a_team_prompt_states_who_wrote_the_plan_and_whose_machine_runs_it`, for
    `compose_prompt`, the strategy prompt, and 021's review and fix prompts. Each expected
    string contains the two lines from Scope 9 verbatim;
  - `a_deleted_author_is_named_a_former_member`;
  - `a_personal_team_prompt_is_unchanged`. Every pre-existing exact-string prompt test
    passes with its expected string untouched.
- `a_run_cannot_accept_or_trust_through_its_handle`: `accept_content` and `set_author_trust`
  are refused on the run-scoped route for every grant.
  `every_registered_tool_has_a_run_scope_decision` and 039's registry test both cover every
  new tool and command.
- **Solo is unchanged.** No existing test's behavioural assertion changes. The diffs are
  confined to fixture setup and to new optional fields. The 31 frontend test files that mock
  `@tauri-apps/api/core` pass, and no component under `src/components/` changes.
- The seam entry, the ADR-0032 pointer and the CLAUDE.md line from Scope 11 exist.
  `./scripts/check-command-wiring.sh` passes, and D32's appendix lists every new command.
- Every CI check passes, exactly as CLAUDE.md lists them, including the runner crate's test
  and clippy steps from 040.
- **Needs a person; the PR body carries it as a checklist:** launch with
  `RIMAIA_DATA_DIR` pointing at a scratch copy of a real `rimaia.db`. Queue a task, run it,
  and confirm the run history shows a prompt identical in form to one from before the
  upgrade. Edit a plan and re-run it with no prompt to accept anything.

## Notes

**Read first.** ADR-0032 in full: this task implements points 1 to 5 and 7, and the second
bullet of point 6. ADR-0012 (the opt-in whose wording does not soften) and ADR-0009 (the
channels, and its amendment on the planner prompt). Then the seam entries:

- **D28** part 6: the `20261003120200_consent.sql` DDL to type out, and 038's and 043's
  tables it references. Its D4 amendment names the file.
- **D31** points 2, 4, 6 (045's row: authorship facts, and consent on `preview`, `claim`,
  `run_context` and `Continue`), 11 (one reaction to `Conflict`) and 13 (the contract
  suite).
- **D32** point 8 and its 045 bullet: the ceiling becomes a new board command, and
  `set_repository_unattended_runs` stays local.
- **D29** (which runs a reader means: "the run that produced the dependency's commit" is an
  implementation run's).
- **D30** (the `rimaia-run` surface the new tools are refused on).
- **D33** (both caches).
- D8, D10, D12, and D4 and D6 as prohibitions. No new dependency is needed.

**Files to start from.**

- `crates/core/src/tasks/service.rs`: `create_task`, `update_task`.
- `crates/core/src/db/settings.rs`, and 039's team-settings writer.
- `crates/core/src/repo/mod.rs`: `ensure_unattended_runs_allowed` and
  `set_allow_unattended_runs`.
- `crates/core/src/scheduler/selection.rs`: `SkipReason`.
- `crates/core/src/strategy/resolve.rs` (`EffectiveStrategy`, `StrategyOrigin`) and
  `crates/core/src/strategy/catalogue.rs`.
- `crates/core/src/runner/prompt.rs`: `task_context`, and its whole-string tests.
- `crates/core/src/mcp/scope.rs`: `Tool::run_access` and its completeness test.
- `crates/core/src/board/` (036, 043, 044): the claim transaction, `RunContext`.
- `src-tauri/src/commands/repositories.rs` and `src-tauri/src/commands/runs.rs`.
- `src-tauri/src/lib.rs`: both handler lists.
- `src/lib/commands.ts` and `src/types.ts`.
- 040/041's `crates/runner/`: `runner_settings`, `checkouts`, the adoption list.

**Migration.** `src-tauri/migrations/20261003120200_consent.sql`, frozen once this task
lands on the branch.

**What the chain provides.**

- 038: `users`, `runners`, `team_settings`, `ServiceContext.actor`, and `identity::Role`.
- 039: scoped services, the team-settings writer, `testing::teams::TwoTeams`, and the
  registry test.
- 040/041: `runner.db`, `checkouts.unattended_consent`, and the host-injected local MCP
  tools.
- 042: `claim(Next)` and the runner loop.
- 043: `runner_leases`, the one-transaction claim, pins, `lease_generation`, and
  `ErrorCode::Conflict`.
- 044: `RunContext::base` and the dependency run that produced it.
- 021/035: the review instructions, the findings, and `NextStep::Continue`.

If any of those is not where this file says, stop and ask rather than build a second copy.
In particular, if 041 or 042 left the pre-spawn path unable to see the runner store, Scope 7
has nowhere to go.

**What the next tasks expect.**

- 046: the new commands in D32's appendix, and their kinds.
- 051: a membership and role check to generalise, and copy-to-team to carry revisions and
  authors as they are.
- 052: consent cases already in the contract suite.
- 055: the mark in place, so its part of point 6 only removes surfaces.
- 057: a reassigned pin that "run elsewhere" releases.
- 060: `assign_task` to extend with a `team` argument.
- 061: `get_task_consent`, `list_trusted_authors`, the runner policy and the ceiling, with
  every refusal message already written.

**Decisions this file makes, which the seam entry records.**

- **The personal-team rules.** ADR-0032's distinctions between people collapse where there
  is one person. The ceiling is not consulted, the facts are omitted, and an unassigned task
  is the owner's own. That is how "nothing changes in solo" holds without a solo code path.
- **Only a changed value is a revision,** so re-saving never revokes an acceptance.
- **A ceiling refuses a named choice and fills an absent one.** A cost control that quietly
  downgraded a card's model would change the work, not only its cost.
- **Findings and commits are trusted like any authored piece.** They follow ADR-0032 point
  3's table literally. Point 6's mark covers board revisions written with a person's
  credentials, which is the laundering path it names.

**Size.** L, and at the upper edge of one session: roughly 60 lines of SQL, 1,200 of
consent, eligibility and ceiling code with its unit tests, 900 of claim, service and runner
tests, 400 across the ten doors, and the prompt change and caches. That comes to about
3.5–4k lines before `.sqlx/`. If it runs over, cut in this order:

1. **The strategy ceiling (Scope 8)** becomes a new task, appended under the next free
   number and placed directly after 045. It is a cost control that shares no rule with
   consent. Only the `ClaimTarget.ceiling` field touches the same code.
2. **The doors for trust, eligibility and the ceiling (Scope 10)** move to the first
   commit of 061. `assign_task`, `accept_content` and `get_task_consent` stay here.

Never cut the mark, the claim checks, or the runner's re-check. Those are the property the
task exists for.
