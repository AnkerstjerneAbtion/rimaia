# 16. Per-task execution strategy: model, effort, and planned workflows

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

Not every task deserves the same execution. A one-file rename does not need Opus at `max`
effort; a cross-cutting refactor with an ambiguous plan does. Getting this wrong is
expensive in both directions — burning subscription budget on trivial work, or sending a
hard task in underpowered and reviewing slop in the morning.

The user does not want to make that call by hand for every task. There is a third option
between "always default" and "always choose manually": **let a cheap agent read the plan
and decide** — which model, which effort, whether the work should fan out into a
multi-agent workflow and how many agents — then write that decision back onto the task, so
it is visible and editable before the run starts.

This is wanted early — not MVP, but soon after the MCP server.

## Decision

Every task carries an **execution strategy** with three modes:

| Mode | Behaviour |
| --- | --- |
| `default` | Rimaia's configured default model and effort |
| `manual` | User-selected model and effort from a dropdown on the card |
| `planned` | A planner agent analyzes the plan and writes the strategy back to the task |

### Planned mode

A **strategy run** executes before the implementation run:

1. A short, cheap Claude Code run (fast model, low effort) receives the plan, the
   repository context, and a strategy prompt.
2. It decides: top-level model, effort, whether a multi-agent workflow fits, and if so the
   phase breakdown with model and effort per phase and agent counts.
3. It **writes the result back to the task through Rimaia's own MCP server**, with a
   rationale.
4. The task shows the proposed strategy. The user can accept, edit, or override — and can
   let the queue proceed without waiting for approval, which is the point for an overnight
   run.

Strategy runs are cheap, bounded by `--max-turns`, and their failure is not fatal: a
failed strategy run falls back to `default` and notes it on the task rather than blocking
the queue.

### Rimaia does not orchestrate agents

**The strategy is injected into the implementation prompt as guidance; Rimaia does not
spawn or supervise subagents.** Claude Code already has subagents and workflow tooling
inside a session, and ADR-0004's entire premise is that we use that harness rather than
rebuild it. Rimaia decides the *top-level* `--model` and `--effort`, and tells the run what
shape of workflow to use. The run's own session does the fan-out.

If Rimaia started scheduling agents itself it would be a second, worse agent harness
running inside a desktop app.

### Runs get a scoped MCP handle to their own task

To let a run write back to Rimaia, the runner passes `--mcp-config` with the local Rimaia
server and injects the current task id into the prompt. The run can update its own task —
strategy, notes, findings, status — and cannot address other tasks' destructive
operations.

This capability is introduced here and reused by the review loop (ADR-0017). It is also
the first place Rimaia dogfoods its own MCP server.

### Storage

On `tasks`: `strategy_mode`, `model`, `effort`, `strategy_plan` (JSON: phases, per-phase
model and effort, agent counts, rationale), `strategy_source` (`user` | `planner`),
`strategy_updated_at`.

### UI

A compact control on the card and in the detail panel: mode selector, model and effort
dropdowns when manual, and the planner's proposal rendered as a readable summary with its
rationale when planned. A per-repository default strategy so a repo of small tasks can
default low without touching each card.

## Consequences

- Cost and quality become a per-task decision without becoming per-task work.
- The planner's reasoning is on the card, so a bad morning result can be traced to the
  strategy that produced it, not just the implementation.
- Every planned task costs one extra small run. Bounded and cheap, but real — hence
  `planned` is opt-in, not the default.
- The planner can be wrong. It is advisory: the strategy is visible and editable, and the
  default is conservative rather than aggressive.
- Scoped MCP access means a run can modify its own task. Contained by construction — the
  handle carries its task id — and consistent with the trust boundary already accepted in
  ADR-0012.
- Model and effort names change as models ship. The dropdown is populated from
  configuration, not hard-coded, so a new model does not require a release.

## Alternatives considered

- **Manual selection only.** Simpler, and it is per-task work forever — the thing the user
  is explicitly trying to avoid.
- **Rimaia orchestrating the multi-agent workflow itself.** Full control over agent
  scheduling, at the cost of reimplementing the harness ADR-0004 exists to reuse, and
  losing everything Claude Code does natively inside a session.
- **Heuristic strategy selection** (plan length, file count, diff size). Free, no extra
  run, and a poor proxy for difficulty — a short plan can describe very hard work.
- **Deciding strategy at planning time, in the planning session.** Attractive, and it puts
  the decision in a session that has the most context. Worth adding as an MCP field so a
  planning agent *can* set it — but the planner run is still needed for tasks created
  without one.
