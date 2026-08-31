---
id: "020"
title: Per-task execution strategy (model, effort, planned workflows)
milestone: v0.2
status: ready
landed: "#7"
depends_on: ["010"]
adrs: ["0016", "0004"]
size: L
---

# Per-task execution strategy

## Goal

Choose per task how it should be executed — model and effort by hand, or by letting a
planner agent read the plan and decide (including whether to fan out into a multi-agent
workflow, and how far), writing that decision back onto the card.

## Why now

Right after the MCP server, and before the rest of v0.2. It changes what every subsequent
run costs and how good its output is, so the earlier it lands the more runs benefit — and
it is the first real dogfooding of Rimaia's own MCP server.

## Scope

**Model**

- Task fields: `strategy_mode` (`default` | `manual` | `planned`), `model`, `effort`,
  `strategy_plan` (JSON), `strategy_source` (`user` | `planner`), `strategy_updated_at`.
- Per-repository default strategy; global default under it.
- Model and effort options come from **configuration, not hard-coded constants** — a new
  model must not require a release.

**Manual mode**

- Compact control on the card and in the detail panel: mode selector, plus model and
  effort dropdowns when manual.
- Settable over MCP, so a planning agent can decide strategy at plan time.

**Planned mode**

- A **strategy run** before the implementation run: short, cheap model, low effort,
  bounded by `--max-turns`.
- Receives the plan, repository context, available models and effort levels, and a
  strategy prompt (composed like any other, ADR-0009).
- Produces: top-level model and effort, whether a multi-agent workflow fits, phase
  breakdown with per-phase model/effort and agent counts, and a rationale.
- **Writes the result back through Rimaia's MCP server** using the scoped task handle.
- Failure is non-fatal: fall back to `default`, note it on the task, continue the queue.

**Scoped MCP handle** (introduced here, reused by task 021)

- The runner passes `--mcp-config` pointing at the local Rimaia server and injects the
  current task id into the prompt.
- A run can update its own task; it cannot perform destructive operations on others.
- Verify the handle works end to end — this is the first time a run talks back to Rimaia.

**Injection, not orchestration**

- The strategy sets `--model` and `--effort` on the implementation run, and the workflow
  shape is injected into the prompt as guidance.
- **Rimaia does not spawn or supervise subagents** (ADR-0016). The run's own Claude Code
  session does the fan-out with its native tooling.

**UI**

- Card shows the effective model and effort compactly.
- Detail panel renders the planner's proposal with its rationale, and allows accept, edit,
  or override.
- Setting for whether a planned strategy needs approval before the implementation run, or
  proceeds automatically. **Automatic is the useful default for overnight queues.**

## Acceptance criteria

- Manual mode: chosen model and effort appear verbatim in the spawned command line,
  verified by the fixture harness.
- Planned mode: the strategy run completes, writes a strategy back via MCP, and the
  implementation run uses it — end to end, unattended.
- A failing strategy run falls back to `default`, annotates the task, and does not block
  the queue.
- The scoped handle cannot be used to delete or move another task.
- Adding a new model to configuration makes it selectable with no code change.
- Strategy set over MCP at task creation is respected and not overwritten by a planner run
  unless the mode says `planned`.

## Notes

Resist the pull toward Rimaia scheduling agents itself. The moment it does, it is a second
agent harness competing with the one it spawns — ADR-0004 exists to prevent exactly that.
