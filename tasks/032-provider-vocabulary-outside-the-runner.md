---
id: "032"
title: Provider vocabulary outside the runner
milestone: v0.4
status: ready
landed: "#33"
depends_on: ["031"]
adrs: ["0026", "0004", "0016", "0022"]
size: M
---

# Provider vocabulary outside the runner

## Goal

The Claude vocabulary that lives *outside* `runner/` moves behind the trait
[task 031](031-a-provider-seam-for-the-agent-cli.md) introduced, and the UI stops
hardcoding the product name where that is free to do.

## Why now

Task 031 draws the seam and leaves it deliberately narrow: `RunIntent → SpawnPlan` and
`line → RunEvent`, and nothing else. That is the right cut for a refactor whose evidence
is the tests it did not change — but it leaves Claude's name and Claude's assumptions in
five places a second provider would immediately trip over: the doctor's two checks, the
prerequisite probe, the prompt's tool spelling, two hardcoded numbers, and a handful of
strings in `src/`. None of those is architectural, which is why they are a separate task
rather than a wider 031.

Doing it now, before any second provider exists, keeps the same property 031 bought: each
of these is a mechanical move with a test that pins it, where after a second provider
lands each one is a behaviour difference between two live code paths.

## Scope

**The trait gains** `display_name()`, `version_probe()` / `read_version()`, `auth_probe()`
/ `read_auth()`, `minimum_version()`, `tool_handle(server, tool)`, `fanout_noun()`,
`inherit_cost_usd()` and `default_catalogue()`, and their consumers move behind them.

**The doctor.** `Programs.claude` becomes `Programs.agent`; `checks::claude_cli` and
`checks::claude_authenticated` become `checks::agent_cli(provider, program)` and
`checks::agent_authenticated(provider, program)`; `MINIMUM_CLAUDE_VERSION` becomes
`provider.minimum_version()`. A provider whose sign-in cannot be checked — `auth_probe()`
is `None` — reports **no row**, not a failing one.

**`Check::ClaudeCli.as_str() == "claude_cli"` and `"claude_authenticated"` are frozen
forever.** They are the key half of stored `doctor_dismissals`
(`db::settings::DOCTOR_DISMISSALS`), so renaming them makes every dismissal the user put
down silently come back, with no migration to catch it. Only `Check::label()` becomes
dynamic. This is the one place in this task where the tidy rename is a user-visible
regression, and it is named here so nobody discovers it by shipping it.

**`probe_cli`** and `missing_cli`'s "install Claude Code" sentence, at all five call sites
(`scheduler/queue.rs`, `commands/runs.rs` ×2, `commands/strategy.rs` ×2) plus
`doctor/checks.rs`.

**Prompt vocabulary.** `SET_TASK_STRATEGY_TOOL` becomes `tool_handle("rimaia",
"set_task_strategy")` — one string, so the prompt's instruction and the allow-list cannot
drift, which is the rule its own doc comment already states. "subagents" becomes
`fanout_noun()`. `crates/core/tests/prompt.rs` asserts these exactly and changes with
them.

**`ENVIRONMENT_SETUP_COST_USD = 0.077`** becomes `inherit_cost_usd() -> Option<f64>`.
`None` for a provider nobody has measured, and the UI then says nothing rather than
quoting someone else's figure beside a cost decision.

**`DEFAULT_CATALOGUE_JSON`**'s `opus`/`sonnet`/`haiku` entries and the `haiku`/`low`
planner budget become `default_catalogue()`.

**Frontend**, neutralised only where that is free. A `providerInfo { id, displayName }`
on an existing payload — prefer the settings or catalogue payload over a new command, so
`./scripts/check-command-wiring.sh` keeps passing. Then
`src/components/panel/StrategySection.tsx`'s origin label, the three "No model — Claude
Code chooses" placeholders in `src/views/settings/StrategySection.tsx`, and
`src/views/RunsView.tsx`'s usage-limit banner.

**The Vitest change that matters** is `src/components/panel/StrategySection.test.tsx`:
parameterise it and render once with a **non-Claude** name. Without that, the hardcoded
string has simply moved one file to the left.

`src/views/settings/InstructionsSection.tsx`'s `run_environment` prose is
provider-specific in *content*, not just in product name — it comes from the provider or
it degrades to "your agent CLI's full environment".

## Out of scope

`src/components/McpAddCommand.tsx`'s `claude mcp add` line and
`src/views/WelcomeView.tsx`'s equivalent. Those are **Claude Code as an MCP client of
Rimaia** (ADR-0006), which is a different relationship from the agent Rimaia drives: a
user could drive another provider and still hand plans in from a Claude Code session, so
neutralising that copy would make it wrong rather than general.

`crates/core/src/runs/transcript.rs`'s second, independent Claude-shaped parser for the
finished-log viewer is a **known gap, named and not fixed** — ADR-0026's Consequences say
the same. A second provider's finished transcript renders as opaque rows until somebody
takes that on deliberately.

Adding a real second provider, and the per-repository acknowledgement that would let an
operator defeat a capability refusal, both stay out — same reasons as task 031.

## Acceptance criteria

- No Claude-specific string survives in `crates/core/src/doctor/`, `runner/prompt.rs`,
  `db::settings`'s cost constant or `strategy::catalogue`'s default; each reaches its
  consumer through a trait method.
- `Check::ClaudeCli.as_str()` is still `"claude_cli"` and `Check::ClaudeAuthenticated`'s
  is still `"claude_authenticated"`, with a test asserting it and naming
  `doctor_dismissals` as the reason.
- A provider whose `auth_probe()` is `None` produces a doctor report with no
  authentication row at all, rather than a failing one.
- `inherit_cost_usd()` returning `None` makes the Settings panel say nothing about cost,
  rather than rendering a zero or another provider's figure.
- `crates/core/tests/prompt.rs` asserts the composed prompt exactly, with the tool handle
  and the fan-out noun coming from the provider.
- `src/components/panel/StrategySection.test.tsx` renders at least once with a non-Claude
  display name and asserts on it.
- `./scripts/check-command-wiring.sh` passes, and no new Tauri command was added to carry
  `providerInfo`.
- Every CI check passes.

## Notes

The split from task 031 is deliberate and one-directional: 031 is architecture with a
falsifiable seam, and this is vocabulary with a rename risk. The only item here with
teeth is the frozen `doctor_dismissals` keys — everything else fails loudly at compile
time or in a test, and that one fails silently, months later, as dismissed rows quietly
reappearing.
