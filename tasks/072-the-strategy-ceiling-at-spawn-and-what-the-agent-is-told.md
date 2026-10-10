---
id: "072"
title: The strategy ceiling at spawn, and what the agent is told
milestone: v0.5
status: ready
depends_on: ["045"]
adrs: ["0032", "0009", "0028", "0031"]
size: M
---

# The strategy ceiling at spawn, and what the agent is told

## Goal

Finish the two parts of [ADR-0032](../docs/adr/0032-assignment-and-consent-to-run-on-a-machine.md)
that task 045 split off when its diff passed the size limit (045's Notes, "Size"):

- **The runner's strategy ceiling (point 3's last paragraph), at spawn.** 045 built
  `consent::ceiling::judge`, put the ceiling on the claim, and made `eligible` refuse with it.
  After this task the runner also judges again before it spawns, fills an absent model or
  effort from its ceiling, and its owner can read and set the ceiling.
- **What the agent is told (point 7).** 045 fills `RunContext::authorship` board-side. After
  this task the prompt states the two facts: who wrote the plan revision being executed, and
  whose machine and credentials it runs on.

**In solo nothing changes that anyone can see.** The runner has no strategy ceiling, so
nothing is refused or filled, and `authorship` is `None` in a personal team, so every prompt is
byte for byte what it was.

## Why now

Both halves read what 045 just wrote, and neither may wait for M4. A remote runner in 052 must
not spawn on the board's word about the ceiling, and a run on a teammate's machine in M4 must
carry its authorship facts in the persisted prompt from the first run. The claim side of the
ceiling is already in 045, so a ceiling set before this task lands is already enforced at the
claim; this task closes the gap between the claim and the spawn.

## Scope

**What 045 already built, and this task does not build again.** `StrategyCeiling` and its
`runner_settings` key `strategy_ceiling` in `runner.db`; `ceiling::judge` with its five unit
tests; `StrategyOrigin::RunnerCeiling`; `ceiling: StrategyCeiling` on `ClaimTarget::Next`,
`Run` and `Plan` and on `FinishRun`, all `#[serde(default)]`; `eligible` calling `judge` and
using only its refusal; the two strategy-ceiling sentences in 045's Scope 11 table and their
cases in `every_refusal_reads_exactly`; `RunContext::authorship: Option<RunAuthorship>`,
filled board-side. If any of these is not where this list says, stop and ask rather than build
a second copy.

**1. The runner judges again at spawn (045 Scope 7's ceiling half).** At the last point before
the agent process starts, after composition and `prepare`, in the process that spawns it, the
same place 045's consent re-check reads `checkouts.unattended_consent`, the runner reads its
strategy ceiling again and calls `ceiling::judge` on the phase's strategy from the context. A
refusal releases, records the reason as the run's refusal, and spawns nothing, as 045's
consent refusal does. Otherwise it spawns with `judge`'s result. This holds for the queue, Run
now, Retry now and Plan now, and for every phase a `Continue` starts. The board is not trusted
to have honoured the claim's ceiling, which is why the re-check exists.

**2. An absent choice is filled (045 Scope 8's rule, runner side).** The rule `judge` already
implements:

- **A named choice is never silently changed.** A model the card, the repository or the team
  names that is not in `models`, or an effort that ranks above `max_effort`, is refused.
- **An absent choice is filled.** No model anywhere means the first of `models`. No effort
  means `max_effort`. The filled half's origin is `StrategyOrigin::RunnerCeiling`.
- It applies to every purpose: the effective strategy for implementation and fix, the planner
  budget for strategy, and 021's review model and effort for review.

**Who judges.** The board refuses; the runner fills. So `RunnerCeiling` appears only in the
strategy a runner spawns with, never in the board's per-task resolution: `TaskSummary` and
`TaskDetail`'s `effectiveOrigin` cannot carry it, and `src/types.ts`'s `StrategyOrigin` does
not gain it.

**3. Reading and setting the ceiling.** Local commands `get_strategy_ceiling` and
`set_strategy_ceiling`, plus a local MCP tool through 041's host-injected surface, read and
write it. They are runner commands, so they are local (D32): registered in both
`generate_handler!` lists, wrapped in `src/lib/commands.ts`, refused to runs in
`Tool::run_access`, and listed in D32's appendix. Each gets a fixture row (028):
`get_strategy_ceiling` answers no ceiling, which is every existing install, and
`set_strategy_ceiling` answers success without changing the seed. No component renders them
(069 does).

**4. What the agent is told (point 7).** `runner::prompt`'s shared `task_context`, which every
composer with a `# Task context` section uses, gains a parameter for `RunContext::authorship`:

```rust
pub struct RunAuthorship {
    pub plan_revision: i64,
    pub plan_author: Option<String>, // login; None for a deleted account
    pub runner_owner: String,        // login
    pub runner_label: String,
}
```

It is `None` in a personal team, where every author and the machine's owner are one person and
the facts would say nothing. When it is `Some`, two lines follow `- Base ref:` and come before
`- Links:`, exactly:

```
- Plan revision: 4, written by @alice
- Running on: @bob's runner "Mac mini", with @bob's credentials
```

A deleted author renders as `written by a former member`. Nothing goes into
`--append-system-prompt`, because these facts describe the situation, and are not rules the
agent must obey (ADR-0009). `compose_resume_prompt` and 021's continuation are unchanged,
because a resumed session already holds them. The persisted `runs.prompt` carries the lines,
which is what makes the transcript honest.

**5. Records.**

- **A D31 amendment**, "What task 072 decided": the runner's spawn-time `judge`, and where its
  result replaces the context's strategy.
- **The "How to use this" row** for 072.

## Out of scope

- **Any interface.** The ceiling controls are 069's.
- **Changing `judge`, the claim's ceiling predicate or the refusal sentences.** They are
  045's.
- **Any migration.** The ceiling lives in `runner_settings`, which needs none.

## Acceptance criteria

- Runner tests, with a real git repository in a `TempDir` and the fixture CLI stream:
  `a_ceiling_lowered_after_the_claim_refuses_the_spawn`, for the queue, Run now, Plan now and a
  phase started by `Continue`. Each time the fixture CLI is never invoked and the lease is
  released. And `an_absent_model_is_spawned_with_the_ceilings_first`: the argv carries the
  filled model, and the run's recorded strategy names `RunnerCeiling`.
- Claim case in D31 point 13's contract suite, on 045's `Harness::start_shared`:
  `a_review_phase_reached_through_continue_with_a_model_outside_the_ceiling_is_released`.
- Prompt tests assert whole strings (CLAUDE.md):
  - `a_team_prompt_states_who_wrote_the_plan_and_whose_machine_runs_it`, for
    `compose_prompt`, the strategy prompt, and 021's review and fix prompts. Each expected
    string contains the two lines from Scope 4 verbatim;
  - `a_deleted_author_is_named_a_former_member`;
  - `a_personal_team_prompt_is_unchanged`. Every pre-existing exact-string prompt test passes
    with its expected string untouched.
- `every_registered_tool_has_a_run_scope_decision` covers the new local tool, and the run-scoped
  route refuses it.
- 028's `it("has an answer or an explicit refusal for every command commands.ts sends")` passes
  with rows for `get_strategy_ceiling` and `set_strategy_ceiling`, as Scope 3 says.
  `fixtures.test.ts` itself is unmodified.
- **Solo is unchanged.** No existing test's behavioural assertion changes.
- The D31 amendment and the "How to use this" row exist. `./scripts/check-command-wiring.sh`
  passes, and D32's appendix lists both commands.
- Every CI check passes, exactly as CLAUDE.md lists them.

## Notes

**Read first.** 045 in full, especially Scope 7 to 9 and its Notes' "Size". ADR-0032 point 3's
last paragraph and point 7. ADR-0009 (the channels: why the facts go in the prompt and not the
system prompt). ADR-0031 point 1 (why the ceiling travels with the claim). Then D31 points 6 and
13 and 045's amendment to it, D32 (local commands and the appendix), D30 (the `rimaia-run`
surface the new tool is refused on).

**Files to start from.**

- `crates/core/src/consent/ceiling.rs` (045): `judge`, `StrategyCeiling`.
- `crates/core/src/runner/prompt.rs`: `task_context`, its callers and whole-string tests.
- `crates/core/src/runner/start.rs` and `crates/runner/src/queue/` (042, 045): the pre-spawn
  path where 045's consent re-check sits.
- `crates/core/src/board/types.rs`: `RunContext::authorship` (045).
- `crates/core/src/mcp/scope.rs`, `src-tauri/src/lib.rs`, `src/lib/commands.ts`,
  `src/dev/fixtures/`.

**What the next tasks expect.**

- 046: both commands in D32's appendix, as local.
- 058: the headless runner obeys the ceiling unchanged.
- 069: `get_strategy_ceiling` and `set_strategy_ceiling` to render, and `judge`'s fill rule to
  describe in its `ceilingNote`.

**Size.** M: about 150 lines of runner change and tests, 100 for the two commands and their
rows, and the prompt parameter across `task_context`'s callers (about 44 call sites, most of
them tests passing `None`) with its tests.
