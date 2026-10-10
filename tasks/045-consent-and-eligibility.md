---
id: "045"
title: Consent and eligibility
milestone: v0.5
status: ready
depends_on: ["021", "039", "043", "044", "067"]
adrs: ["0032", "0012", "0009"]
size: L
---

# Consent and eligibility

## Goal

Make [ADR-0032](../docs/adr/0032-assignment-and-consent-to-run-on-a-machine.md) true in the
claim. After this task no runner claims, re-reads or continues a task unless four things
hold, and all four are checked in `board::lease::eligible`, the one function 043 made for
"may this runner take this task":

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
team's prompt is byte for byte what it was.

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
DDL of seam-contract D28 part 6 under that name. The header comment is this task's own, in
the voice of the existing migrations. Its first line is its title and never begins with
`-- no-transaction`, and it contains no `DELETE` (D28 part 1). It does not touch
`repositories.allow_unattended_runs`. That column already means the team ceiling (D28 part
6's comment on 038), and 041 copied it into `checkouts.unattended_consent`. A backfill here
would run before that copy on an install that upgrades across both, and would grant consent
nobody gave.

Regenerate both offline caches with D33's recipe (D33 names 045 among the tasks that must).

**2. Every writer of consent-gated content records a revision.** One helper per content
kind, and every write goes through it. A write that stores the same text again is not an
edit: it bumps nothing, so it does not undo anyone's acceptance.

- `tasks::create_task` writes `created_by` and `plan_updated_by` as `ctx.actor`, and
  `plan_written_during_run` from the mark below. `plan_revision` starts at the column's 1.
- `tasks::update_task`, when `plan` or `extra_instructions` changes value, increments
  `plan_revision` and sets `plan_updated_by` and `plan_written_during_run`.
- 034's review note. `review::actions::request_changes` and `reject` append the reviewer's
  note to `extra_instructions`, and ADR-0032 point 3 makes every edit to that column a plan
  revision. 034 routed the append through one private function in `review::actions` that
  writes `extra_instructions`, so that this task has exactly one place to add the bump
  (034's Notes, "What the next tasks expect"). That function goes through the plan helper:
  it increments `plan_revision`, sets `plan_updated_by` to `ctx.actor` and sets
  `plan_written_during_run` from the mark, inside 034's one `BEGIN IMMEDIATE` transaction.
  A refused move therefore bumps nothing, as it appends nothing. A note is never blank (034
  refuses one), so an accepted action always changes the value. Approve writes no note and
  bumps nothing. If 034's append is not in one function, stop and ask rather than add the
  bump at each call site.
- 021's `set_task_review`, when the task's `review_instructions` changes value, increments
  `review_instructions_revision` and sets `review_instructions_updated_by` and
  `review_instructions_written_during_run`. It is the only writer of that column: 021 kept
  it off `TaskPatch`, and `create_task` takes no override.
- 039's team-settings writer increments `team_settings.revision`, and sets `updated_by`,
  `updated_at` (from `ctx.clock`) and `written_during_run`, on every key whose value
  changes. `set_base_instructions` (`db/settings.rs`) and 021's `set_review_settings`
  (`review_loop/config.rs`) both reach it. Consent reads only the two instruction keys, but
  one rule for every row is simpler than a list.
- MCP's tools reach the same services, so content written over MCP is written by the
  context's actor (ADR-0032 point 3).
- `grep -rn "plan\s*=\|extra_instructions\s*=\|review_instructions\s*=" crates/core/src`
  lists every SQL writer. Each one either goes through a helper or is a test fixture. The
  grep is a check on the list above, not a substitute for it: a writer it misses because
  its SQL is spelled differently is still a writer.

**3. Assignment (point 1).** `tasks::assign_task(ctx, task_id, assignee_id: Option<UserId>)`
sets `assignee_id` and `assigned_by = ctx.actor`. The assignee must be a member of the
task's team: a user who is not a member is refused as `NotFound`, worded as for a user who
does not exist (039's rule). `None` returns the task to the pool. Publishes
`ChangeEvent::tasks` for the task's team. `Task` (Rust `db::models` and `src/types.ts`)
gains `createdBy`, `assigneeId`, `assignedBy`, `planRevision` and `planUpdatedBy`, all
optional on the TypeScript side. No component renders them (061 does).

**4. The consent module.** New: `crates/core/src/consent/` with `mod.rs`, `pieces.rs`,
`eligibility.rs` and `ceiling.rs`. The last three are pure functions over values, unit-tested
in their own files without a pool. `mod.rs` holds the queries, which run on a connection the
caller passes in, so the claim evaluates consent inside its own transaction.

- **`ContentKind`**, an enum in the `acceptances.content` `CHECK`'s spelling: `Plan`,
  `TaskReviewInstructions`, `BaseInstructions`, `ReviewInstructions`, `ReviewFindings`,
  `BaseCommit`.
- **`Piece { kind, task_id: Option<TaskId>, revision: String, author: Option<UserId>,
  written_during_run: bool }`**, and **`pieces_for(purpose, inputs) -> Vec<Piece>`**, which
  lists the consent-gated content the composer for that lease purpose reads:

  | Purpose | Pieces |
  | --- | --- |
  | `implementation` | plan · base instructions · base commit |
  | `strategy` | plan · base commit |
  | `review` | plan · the effective review instructions · findings from another runner · base commit |
  | `fix` | plan · base instructions · findings from another runner · base commit |

  The planner takes no base instructions (ADR-0009's 2026-08-28 amendment) and borrows the
  implementation's worktree, so its base commit is a piece. The effective review
  instructions are the task's override when it is non-blank, and the team's otherwise
  (021's rule). A piece whose content is empty or absent is not in the list.

  **Findings.** One `ReviewFindings` piece per run on a runner other than the claiming one
  whose text the composer includes: for `fix`, the review run whose open findings it acts
  on; for `review`, each fix run whose rejection reasons fill `# Findings already rejected`.
  `revision` is that run's id, and `author` is the owner of its runner.

  **The base commit.** Present only when 044's `RunContext::base.dependency` is `Some`.
  `task_id` is its `task_id` and `revision` its `commit`. The commit holds the work of
  every implementation and fix run on the dependency up to the chosen one (`run_id`), not
  only that run's, and they need not share a runner. So there is **one `BaseCommit` piece per
  distinct owner** of the runners of the dependency's implementation and fix runs with
  `attempt` up to the chosen run's, failed ones included, because a failed attempt's
  commits stay in the worktree the next attempt continues from. One acceptance of the commit
  covers all of them. Review runs are left out for D29 point 5's reason: a review that moved
  `HEAD` is `review_changed_branch`, and nothing builds on it. The read is one board query
  beside `runs::latest_successful_head`, `runs::commit_authors(conn, task_id, attempt)`.

  **Not a piece:** the strategy planner's guidance that the implementation prompt carries
  (ADR-0009's fifth section). ADR-0032 point 3's last paragraph exempts execution strategy.

- **The current revision** of each kind, which `accept` compares against: the decimal
  `plan_revision` or `review_instructions_revision`; `team_settings.revision` of the key's
  row in the team; and for `ReviewFindings` and `BaseCommit`, a revision that `pieces_for`
  lists for the task now.
- **`consents(owner, piece, accepted, trusted) -> Consent`**, in `pieces.rs`, in this order:
  1. The piece was written during a run: only an acceptance of exactly `(owner, kind,
     task, revision)` counts. **Its author's own runners included.** That is point 6's
     case: a plan Bob's credentials wrote while Bob's runner ran Alice's plan must not pass
     Bob's runners on authorship.
  2. The owner is the author: consents.
  3. An acceptance of exactly that revision exists: consents.
  4. The owner's trust list for the task's team names the author: consents. An author of
     `None` (a deleted account) is never trusted.
  5. Otherwise it is `Missing { piece, reason }`.
- **The written-during-run mark (point 6).** `written_during_run(conn, actor) -> bool` is
  true when a live `runner_leases` row belongs to a runner whose `user_id` is the actor, and
  `pieces_for` of that lease's purpose over its task names an author other than the actor.
  Reusing `pieces_for` makes "content authored by someone else" mean the same thing in the
  mark and in the check. **The lookup deliberately reads across all of the actor's runners
  and teams, ignoring `ctx.scope`.** Bob's runner can hold a lease in a shared team while
  Bob writes into his personal team, and that is exactly the path point 6 closes. It is
  safe because it returns one bool about the actor's own write and nothing else crosses
  back. For a user who belongs only to a personal team, every lease's content is their own,
  so the mark is never set. That is a consequence, not a rule: there is no team-kind
  short-circuit.
- **Trust.** `set_trust(ctx, team_id, user_id, trusted: bool)` adds or removes a
  `trusted_authors` row for `ctx.actor`. The trusted user must be a member of that team.
  Trusting oneself is refused as `Invalid` before the `CHECK` sees it. `list_trusted(ctx,
  team_id)` returns the actor's own list and nobody else's (trust is personal).
- **Acceptance.** `accept(ctx, team_id, task_id: Option<TaskId>, kind, revision)` records
  `(actor, team, task, kind, revision)` with `accepted_at` from `ctx.clock`. The team must be
  in `ctx.scope`, and a given task must be in that team, or `NotFound`. `task_id` is `None`
  exactly for the two team-wide kinds, mirroring the table's `CHECK`. It accepts **only the
  current revision**, so nobody accepts text they did not see. A stale one is `Invalid`,
  naming the current revision and who wrote it: not `Conflict`, which means only "your lease
  is not the current one" (043's D8 amendment). Accepting twice is idempotent.
- **`status(ctx, task_id, runner_id) -> TaskConsent`**, the read 061 draws from:
  eligibility, the pin, the team ceiling, and every missing piece with its kind, revision,
  author's login and reason. A camelCase DTO with no path (ADR-0028 point 2's test covers
  it).

**5. Eligibility (point 2).** `eligibility::decide(task, runner, pool_teams) -> Eligibility
{ Assigned, Pool, NotEligible(Reason) }`:

- assigned to the runner's owner: `Assigned`;
- assigned to anyone else: `NotEligible(AssignedToSomeoneElse)`, whatever the policy;
- unassigned, in the runner owner's **personal** team: `Assigned` (ADR-0032 point 2: "in a
  personal team every task is the owner's own", which is why the migration does not
  backfill `assignee_id`);
- unassigned, policy `assigned_then_pool`, and the task's team is in `runner_pool_teams`:
  `Pool`;
- otherwise `NotEligible(Unassigned)`.

`RunnerEligibility { Assigned, AssignedThenPool }` is an enum in the column's spelling.
`set_runner_eligibility(ctx, runner_id, policy, pool_team_ids)` is refused as `NotFound`
unless the runner belongs to `ctx.actor`, and every pool team must be one the owner is a
member of. The pool list is replaced whole, in one transaction.

**6. The claim: `board::lease::eligible` gains four predicates.** 043 wrote
`eligible(conn, task, runner, purpose)` with the pin, 067 added the model rule, and 043 said
consent goes there and nowhere else. It gains a `ceiling: &StrategyCeiling` parameter (Scope 8) and,
after 043's pin check, in this order: `eligibility::decide`, the team ceiling, 067's model
rule, `ceiling::judge`, then `consents` over `pieces_for(purpose)`. It returns the first
failure as one `Ineligible` value, which every caller renders and none re-derives:

- **`Next`** (042's body in `board::service`). `selection::plan` gains the runner, and
  every entry that 042's listed-repository check leaves without a skip reason goes through
  `eligible`. The owner's tasks come first: every `Assigned` entry in board order, then
  every `Pool` entry. The claim transaction calls `eligible` again for the chosen task, as
  043 has it.
- **`Run` and `Plan`** answer the failure as an `Err` with its sentence from Scope 11,
  never `None`, which means "someone else got there first" (D31 point 4).
- **`Continue`** (021's decision in `finish_run`) calls `eligible` for the next phase's
  purpose. A failure answers `Released`, and the task lands as 021's window-closed exit:
  `in_review`, `idle`, unreviewed.
- **`run_context`** re-checks consent (D31 point 6). If a piece no longer consents, it ends
  the lease in the same transaction with `release`'s semantics (the row deleted, the task
  landed as `release` lands it), then answers `Conflict`. The lease really is gone, so
  D31 point 11's reaction is correct, and a solo lease (`LeaseTerm::Never`) cannot be left
  `running` with no holder. A plan edited between the claim and the composition is never
  composed.
- **`preview`** takes no ceiling. It passes `StrategyCeiling::default()` and so reports
  eligibility and consent refusals only. The claim is where a ceiling refusal comes from.

**The team ceiling** is new in `eligible`, beside 042's listed-repository check, which
stays the runner's half (`repositories` names only consented checkouts). It applies to
every purpose, as today's opt-in applies to Manual runs in 036's core starter
(`runner/start.rs`). **In a personal team the ceiling is not consulted.** There the team's
owner and the machine's owner are one person, so the runner's consent is the whole decision. That keeps solo's single toggle, and repositories registered after 066
(column default `0`) still run in solo. A repository never leaves its team (039 refuses the
move, and 051 copies tasks, not repositories), so a personal-team repository's column never
becomes a team's ceiling. A repository registered in a shared team starts at `0`.

`set_repository_unattended_ceiling(ctx, repository_id, allowed)` is the board command D32
promises for 045. It requires the `Owner` role in the repository's team (ADR-0029's roles
table), and a member is refused as `Invalid`. This is the first role check, and 051
generalises it. On a personal team it is refused as `Invalid`.
`set_repository_unattended_runs` stays the runner's consent, and stays local.

**`SkipReason` gains `NotEligible`, `ConsentMissing` and `ForbiddenByTeam`**, each with an
`explanation`, and the TypeScript union matches. The set is closed (D23 point 4), so each
needs the reason D23 point 4 gave `WaitingForRetry`: it persists until a person acts, and
each names a different act. Reassign the card or join the pool; accept or trust; ask an
owner. Capacity fails that test (D21 point 3), and so do 043's pin, 067's model rule and the
strategy ceiling, which another runner resolves with nobody acting, so `Next` still passes
over those without a reason. `ForbiddenByTeam` is not `UnattendedRunsNotAllowed`: after
042 that one means only that this runner did not list the repository, which its owner fixes
on this machine. **The reasons are the local runner's.** The Runs view's plan is 042's
`status_with_plan`, built from the same runner view the loop sends with `Next`, so a skip
reason says why *this* runner will not take the card.

**Two owners in the contract suite.** Runners `A` and `B` are both the solo user's, in the
personal team. D31 point 13's `Harness` gains `async fn start_shared()`: a server-shaped
board (no `solo_identity`, teams built as 039's `TwoTeams` builds them) with one shared
team of two members, `A` owned by the first and `B` by the second, and both adapters scoped
to that team. The second member is added through the `testing` helper that stands in for
051's invitation. 052's HTTP harness implements `start_shared` too, minting `B`'s runner
token for the second member.

**7. The runner re-checks before every spawn (point 4).** 042 lists only consented
checkouts in `repositories`, and leaves the re-check to this task. Its test
`the_queue_offers_the_board_only_consented_repositories` is built by 042, in
`crates/runner/tests/queue.rs`, and stays 042's. At the last point
before the agent process starts, after composition and `prepare`, in the process that
spawns it, the runner reads `checkouts.unattended_consent` (066) and its strategy ceiling again, and
calls `ceiling::judge` on the phase's strategy from the context. It spawns with that result
(Scope 8). A refusal releases, records the reason as the run's refusal, and spawns nothing.
This holds for the queue, Run now, Retry now and Plan now, and for every phase a `Continue`
starts. The runner store reaches that point by the route 041 gave `run_environment`, read
when each run starts. The board is not trusted to have honoured `repositories` or the
claim's ceiling, which is why the re-check exists.

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
  team names that is not in `models`, or an effort that ranks above `max_effort`, is
  refused. Efforts rank by their position in the team's catalogue, which lists them
  cheapest first. An effort the catalogue does not list exceeds every ceiling.
- **An absent choice is filled.** No model anywhere means the first of `models`. No effort
  means `max_effort`. The filled half's origin is a new `StrategyOrigin::RunnerCeiling`.
- It applies to every purpose: the effective strategy for implementation and fix, the
  planner budget for strategy, and 021's review model and effort for review.

**Who judges.** The board refuses; the runner fills. The claim cannot read `runner.db`, so
the ceiling travels with the claim, as ADR-0031 point 1 sends the provider:
`ClaimTarget::Next`, `Run` and `Plan` each gain `ceiling: StrategyCeiling`, and so does
`FinishRun`, for the phase a `Continue` would start. All are `#[serde(default)]`. `eligible`
calls `judge` and uses only its refusal. The runner calls `judge` again at spawn (Scope 7)
and spawns with its result. So `RunnerCeiling` appears only in the strategy a runner spawns
with, never in the board's per-task resolution: `TaskSummary` and `TaskDetail`'s
`effectiveOrigin` cannot carry it, and `src/types.ts`'s `StrategyOrigin` does not gain it.

Local commands `get_strategy_ceiling` and `set_strategy_ceiling`, plus a local MCP tool
through 041's host-injected surface, read and write it.

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

**10. Doors (ADR-0021 parity, ADR-0006).** Seven board commands in `src-tauri/src/commands/`,
registered in both `generate_handler!` lists, wrapped in `src/lib/commands.ts`, and exposed
as MCP tools with the same names:

- `assign_task`
- `accept_content` (`teamId`, optional `taskId`, `kind`, `revision`)
- `set_author_trust` and `list_trusted_authors`
- `set_runner_eligibility`
- `set_repository_unattended_ceiling`
- `get_task_consent`

Plus the two local commands from Scope 8. Each is a thin adapter over the service above.
Every one of them is refused to runs in `Tool::run_access`. **`accept_content` and
`set_author_trust` are the ones that matter:** a run that could accept or trust would
launder consent through its own handle. Each is added to 039's two-team registry test and to
D32's appendix (D32 point 8). A refusal crosses the boundary as the one error type (D8).

**Fixtures.** 028's rule is that every command `commands.ts` sends has a row in the fixture
table in `src/dev/fixtures/`, and its coverage test fails on a wrapper without one. So all
nine wrappers get a row, and fixture mode is a solo board, so each answers the solo shape:

- `assign_task`, `accept_content`, `set_author_trust`, `set_runner_eligibility` and
  `set_strategy_ceiling` answer success without changing the seed, as every fixture write
  does (028).
- `list_trusted_authors` answers `[]`: in solo there is nobody else to trust.
- `get_task_consent` answers the solo `TaskConsent` for any task: `Assigned`, no pin, the
  team ceiling not consulted, and no missing piece. `TaskConsent` is in `src/types.ts`, so
  the row is typed against it.
- `get_strategy_ceiling` answers no ceiling, which is every existing install.
- `set_repository_unattended_ceiling` answers an explicit `invalid` refusal with Scope 11's
  personal-team sentence, because that is what a solo board says to it.

The team scenario's answers are 061's, which extends these rows rather than adding them.

**11. Refusal sentences.** Exact, and quoted by 057 and 061. `{what}` is `the plan`, `this
task's review instructions`, `the team's base instructions`, `the team's review
instructions`, `the findings recorded in run {run_id}` or `commit {sha} from "{title}"`.
`{who}` is `@{login}`, or `a former member`.

| Refusal | Sentence |
| --- | --- |
| assigned to someone else | `this task is assigned to @{login}. Only their runners run it: reassign it to run it here.` |
| unassigned, outside the pool | `this task is unassigned, and {label} does not take pool work from this team. Assign it to yourself, or add the team to the runner's pool.` |
| pinned here, then reassigned | `this task is pinned to {label}, but it is now assigned to @{login}. No runner can take it until someone chooses to run it elsewhere.` |
| consent missing | `{what} was changed by @{login}, and you have not accepted that revision. Accept it, or trust @{login}'s changes.` |
| consent missing, former member | `{what} was changed by a former member. Accept that revision to run it.` |
| consent missing, written during a run | `{what} was written with @{login}'s credentials during a run on someone else's task. Only accepting that revision lets it run.` |
| team ceiling | `the team does not allow unattended runs in {repository}. A team owner can allow them.` |
| strategy ceiling, model | `this task asks for the model "{model}", which {label}'s strategy ceiling does not allow. Change the task's model, or run it on another runner.` |
| strategy ceiling, effort | `this task asks for the effort "{effort}", above {label}'s ceiling of "{max_effort}". Nothing is lowered for it: change the task's effort, or raise the ceiling.` |
| stale acceptance | `revision {given} is not current: {what} is at revision {current}, changed by {who}. Read it before accepting it.` |
| trusting oneself | `you cannot trust yourself: your own changes already count.` |
| ceiling command, member | `only an owner of this team can change whether it allows unattended runs.` |
| ceiling command, personal team | `a personal team has no ceiling: this machine's own consent decides.` |

A task pinned to another runner keeps 043's pin sentence.

The Runs view's labels for the three new `SkipReason`s, in `QUEUE_SKIP_LABELS`:
`not_eligible`: `assigned to someone else, or outside the pool this runner takes`;
`consent_missing`: `waiting for you to accept a change`; `forbidden_by_team`: `the team does
not allow unattended runs in this repository`.

**12. Records.**

- **A D31 amendment**, "What task 045 decided": `ceiling` on the three `ClaimTarget`
  variants and on `FinishRun`; `RunContext::authorship`; `eligible`'s new parameter and
  predicate order; `run_context` ending the lease before it answers `Conflict`; and
  `Harness::start_shared`. Amendments, as 043 set the precedent.
- **A new entry under the next free D number**, "Task 045's cross-cutting choices", in the
  four-part shape: the personal-team rules (no ceiling, no authorship facts, the
  unassigned task that is the owner's own); a revision only for a changed value; the
  pieces, including findings, base-commit authors and the strategy-plan exclusion; the
  current revision per kind; the ceiling's "refuse a named choice, fill an absent one"; the
  three `SkipReason`s with their justification; and Scope 11's table verbatim.
- **A dated ADR-0032 amendment** recording that point 4's team ceiling is not consulted in
  a personal team, with the reason from Scope 6. Point 4's "a run needs both" and the
  Consequences' "the stricter wins" otherwise read as written.
- **The "How to use this" row** for 045: D4 · D8 · D10 · D12 · D21 · D23 · D28 · D29 · D30
  · D31 · D32 · D33.
- **CLAUDE.md:** "consent, eligibility and the strategy ceiling" joins the list of modules
  that must have tests.

## Out of scope

- **Any interface beyond Scope 11's three labels.** The assignee picker, the consent banner,
  the trust list, the runner policy and the ceiling controls are all 061's.
- **Point 6's first bullet.** Stripping Rimaia tokens from a run's environment and denying
  MCP servers by URL is 055's. This task writes only the written-during-run mark.
- **Inviting a second member and changing roles.** Tests add a member through a
  `testing`-feature helper that stands in for 051's invitation. 051 replaces the helper's
  body with the real service, and generalises the role check this task writes once.
- **Assignment on `create_task` and on the hosted MCP surface:** 060.
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
- `a_board_migrated_through_consent_attributes_everything_to_the_solo_user`. The board is
  built by the old-schema builder 040 lifted into `crates/core/src/testing/`, which gains a
  version cutoff: it applies every board file older than `20261003120200`. Every task has
  `created_by` and `plan_updated_by` equal to the solo user, `assignee_id` is `NULL`,
  `review_instructions_updated_by` is set exactly where an override exists, every
  `team_settings` row names the solo user, and `pragma_foreign_key_check` is empty.
- Unit tests in `consent/pieces.rs`:
  - one per purpose, asserting the exact piece list for a task with every kind of content,
    and that an empty plan, a blank override, a base on the default branch and the
    strategy-plan guidance contribute no piece:
    `an_implementation_run_needs_the_plan_the_base_instructions_and_the_base_commit`,
    `a_planner_run_needs_no_base_instructions`,
    `a_review_run_reads_the_override_and_rejections_from_another_runner`,
    `a_fix_run_on_the_runner_that_reviewed_needs_no_consent_to_its_own_findings`;
  - `a_base_commit_built_by_two_owners_runners_needs_consent_to_both`;
  - one per rule of `consents`: `the_author_consents_to_their_own_revision`,
    `an_acceptance_covers_exactly_one_revision`,
    `trusting_an_author_consents_to_their_next_revision`,
    `a_former_member_is_never_trusted`,
    `a_revision_written_during_a_run_needs_an_acceptance_even_from_its_author`.
- Service tests against the in-memory pool, with a faked clock and a shared team of two
  members:
  - `editing_a_teammates_plan_makes_it_unrunnable_for_them_until_they_accept`;
  - `saving_the_same_plan_again_is_not_a_new_revision`;
  - `changing_a_tasks_review_instructions_is_a_new_revision`, through `set_task_review`;
  - `requesting_changes_is_a_new_plan_revision_by_the_reviewer`: a task whose plan the
    first member wrote, in `in_review`; the second member's `request_changes` leaves
    `plan_revision` one higher and `plan_updated_by` equal to the second member, and the
    first member's runner is then refused with Scope 11's consent sentence until the first
    member accepts. The same through `reject`. A `request_changes` refused by 034's table
    leaves `plan_revision` and `plan_updated_by` unchanged;
  - `base_instructions_edits_need_acceptance_from_members_who_do_not_trust_the_editor`;
  - `accepting_a_stale_revision_is_invalid_naming_the_current_one`;
  - `accepting_base_instructions_names_the_team`, including a task in another team refused
    as `NotFound`;
  - `revoking_trust_stops_the_next_claim`;
  - `a_plan_bob_wrote_while_his_runner_ran_alices_plan_needs_his_own_acceptance`;
  - `a_plan_written_into_a_personal_team_during_a_shared_team_run_is_marked`;
  - `planning_while_your_runner_works_on_your_own_task_is_not_marked`;
  - `a_dependency_commit_from_another_owners_runner_needs_trust_or_acceptance`;
  - `a_member_cannot_assign_a_task_to_someone_outside_the_team`;
  - `a_member_cannot_set_the_team_ceiling`, and the personal-team refusal.
- Eligibility unit tests in `consent/eligibility.rs`:
  - `a_runner_never_runs_a_task_assigned_to_someone_else`, with both policies;
  - `an_unassigned_task_in_a_personal_team_is_the_owners`;
  - `the_pool_is_only_the_teams_the_runner_opted_into`.
- Claim cases in D31 point 13's contract suite, on `Harness::start_shared`, so 052's HTTP
  adapter inherits them:
  - `run_now_on_an_unconsented_task_refuses_with_the_reason` (exact string);
  - `next_skips_an_unconsented_task_and_takes_the_one_below_it`;
  - `a_runner_takes_its_owners_tasks_before_the_pool`: a pool task above an assigned one in
    board order, and `Next` returns the assigned one first;
  - `a_reassigned_pinned_task_is_claimable_by_nobody`;
  - `a_consent_lost_before_composition_ends_the_lease_and_answers_conflict`: no
    `runner_leases` row remains, and the task is where `release` lands it;
  - `a_fix_phase_whose_findings_lost_consent_is_released_instead_of_continued`;
  - `a_review_phase_reached_through_continue_with_a_model_outside_the_ceiling_is_released`;
  - `a_team_that_forbids_unattended_runs_blocks_every_runner`;
  - `a_personal_team_ignores_the_ceiling_column`, on the default `start`.
- Ceiling unit tests in `consent/ceiling.rs`:
  `a_named_model_outside_the_ceiling_is_refused`,
  `an_effort_above_the_ceiling_is_refused_never_lowered`,
  `an_absent_model_and_effort_are_filled_from_the_ceiling` (origin `RunnerCeiling`),
  `an_effort_the_catalogue_does_not_list_exceeds_every_ceiling`, `no_ceiling_changes_nothing`.
- `every_refusal_reads_exactly`: one case per row of Scope 11's table, asserting the whole
  string.
- Runner tests, with a real git repository in a `TempDir` and the fixture CLI stream:
  `a_runner_refuses_to_spawn_where_it_never_consented_even_when_the_board_claims_it`, for
  the queue, Run now, Plan now and a phase started by `Continue`, and
  `a_ceiling_lowered_after_the_claim_refuses_the_spawn`. Each time the fixture CLI is never
  invoked and the lease is released.
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
- 028's `it("has an answer or an explicit refusal for every command commands.ts sends")`
  passes with a row for each of the nine commands, answering as Scope 10's Fixtures list
  says: `assign_task`, `accept_content`, `set_author_trust`, `list_trusted_authors`,
  `set_runner_eligibility`, `set_repository_unattended_ceiling`, `get_task_consent`,
  `get_strategy_ceiling` and `set_strategy_ceiling`. `fixtures.test.ts` itself is
  unmodified, `npm run typecheck` passes with the rows typed against `src/types.ts`, and
  028's bundle test still finds no fixture in the production build.
- **Solo is unchanged.** No existing test's behavioural assertion changes. The diffs are
  confined to fixture setup and new optional fields. The 31 frontend test files that mock `@tauri-apps/api/core` pass. The
  only change under `src/components/` is Scope 11's three `QUEUE_SKIP_LABELS` entries.
- The D31 amendment, the new D entry, the ADR-0032 amendment, the "How to use this" row and
  the CLAUDE.md line from Scope 12 exist. `./scripts/check-command-wiring.sh` passes, and
  D32's appendix lists every new command.
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

- **D28** part 6: the DDL to type out, and 038's and 043's tables it references.
- **D31** points 2, 4, 6, 11 and 13, and 042's and 043's amendments to it.
- **D29** point 5 and its 044 amendment: `latest_successful_head` takes implementation and
  fix rows only, and returns the run's id.
- **D21** point 3 and **D23** point 4, before widening `SkipReason`.
- **D32** point 8 and its 045 bullet: the ceiling becomes a new board command, and
  `set_repository_unattended_runs` stays local.
- **D30** (the `rimaia-run` surface the new tools are refused on), **D33** (both caches).
- D8, D10, D12, and D4 and D6 as prohibitions. No new dependency is needed.

**Files to start from.**

- `crates/core/src/board/lease.rs` (043): `eligible`, the claim transaction, `current`.
- `crates/core/src/board/{types,service}.rs` (036, 042, 044): `ClaimTarget`, `FinishRun`,
  `RunContext`, `Next`'s body, `finish_run`'s `Continue`.
- `crates/core/src/tasks/service.rs`: `create_task`, `update_task`.
- `crates/core/src/review_loop/config.rs` (021): `set_task_review`, `set_review_settings`.
- `review::actions` (034): `request_changes`, `reject`, and the one private function that
  writes `extra_instructions`.
- `crates/core/src/db/settings.rs` (`set_base_instructions`), and 039's team-settings writer.
- `crates/core/src/runs/`: `latest_successful_head` (044), where `commit_authors` goes.
- `crates/core/src/scheduler/selection.rs`: `SkipReason`, `plan`'s view (042).
- `crates/core/src/strategy/resolve.rs` (`EffectiveStrategy`, `StrategyOrigin`) and
  `crates/core/src/strategy/catalogue.rs`.
- `crates/core/src/runner/prompt.rs`: `task_context`, and its whole-string tests.
- `crates/core/src/runner/start.rs` (036, 042): the starters' preflight.
- `crates/core/src/mcp/scope.rs`: `Tool::run_access` and its completeness test.
- `crates/core/src/testing/board_contract.rs`, and the old-schema builder (040).
- `src-tauri/src/commands/repositories.rs`, `src-tauri/src/lib.rs` (both handler lists).
- `src/lib/commands.ts`, `src/types.ts`, `src/components/runs/QueuePlanList.tsx`.
- `src/dev/fixtures/` and `fixtures.test.ts` (028): the table every wrapper needs a row in.
- `crates/runner/` (040–042): `runner_settings`, `checkouts`, the queue's `try_step`.

**What the chain provides.** 028: fixture mode and its coverage test. 034: `review::actions`
with its note append in one private function. 021: review instructions, findings, the
review and fix composers, `NextStep::Continue`. 038: `users`, `runners`, `team_settings`,
`ServiceContext.actor`, `identity::Role`. 039: scoped services, the team-settings writer,
`TwoTeams`, the registry test. 040/041/066: `runner.db`, `checkouts.unattended_consent`, the
host-injected local MCP tools, the old-schema builder. 042: `claim(Next)`, the listed-repository
check, `scheduler::view::for_runner` and the runner loop. 043: `runner_leases`, `eligible`, pins,
`lease_generation`, `Conflict`. 044: `RunContext::base` with `BaseDependency::run_id`.

If any of those is not where this file says, stop and ask rather than build a second copy.
In particular, if 041, 066 or 042 left the pre-spawn path unable to see the runner store, Scope 7
has nowhere to go.

**What the next tasks expect.**

- 046: the new commands in D32's appendix, and their kinds.
- 051: a role check to generalise. Its copy-to-team goes through `create_task`, so the
  copier is the author at `plan_revision` 1.
- 052: consent cases in the contract suite, and `start_shared` to implement.
- 055: the mark in place, so its part of point 6 only removes surfaces.
- 057: a reassigned pin that "run elsewhere" releases, and Scope 11's sentences.
- 060: `assign_task` exposed unchanged; the task id determines the team.
- 061: `get_task_consent`, `list_trusted_authors`, the runner policy and the ceiling, with
  every refusal sentence and queue label already written, and a solo fixture row for each
  of the nine commands for its `team` scenarios to extend.

**Size.** L, at the upper edge of one session: roughly 60 lines of SQL, 1,200 of consent,
eligibility and ceiling code with its unit tests, 900 of claim, service and runner tests,
400 across the nine doors and their fixture rows, and the prompt change. The review-note
bump is one call in 034's existing function and one service test, and the fixture rows are
about 60 lines; neither moves the estimate. **If the diff passes about 3,500 lines
excluding `.sqlx/`, stop and propose the split** rather than trimming tests. Cut in this
order:

1. **The strategy ceiling (Scope 8)** becomes a new task, appended under the next free
   number and placed directly after 045. It shares no rule with consent; only the
   `ceiling` fields and one predicate in `eligible` touch the same code.
2. **The doors for trust, eligibility and the ceiling (Scope 10)** move to the first
   commit of 061, and their fixture rows move with them, because a wrapper and its row land
   together. `assign_task`, `accept_content` and `get_task_consent` stay here.

Never cut the mark, the claim checks, or the runner's re-check. Those are the property the
task exists for.
