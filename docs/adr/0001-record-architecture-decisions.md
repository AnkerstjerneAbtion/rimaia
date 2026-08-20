# 1. Record architecture decisions

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

Rimaia is being built by a single developer working mostly through coding agents. Agents
receive a plan and a repository, not the conversation that produced the plan. Decisions
that live only in chat history are lost the moment the session ends, and the next agent
re-litigates them — usually differently.

The project also has an unusual number of load-bearing decisions for its size: how agents
are invoked, how work is isolated, how dependencies gate execution, how unattended runs
handle subscription limits. These are the decisions an implementing agent needs, and they
are exactly the ones that are cheapest to get wrong silently.

## Decision

Record every architecturally significant decision as a numbered Markdown file in
`docs/adr/`, using a lightweight MADR structure: context, decision, consequences,
alternatives considered.

"Architecturally significant" means: it constrains how other parts of the system are
built, it is expensive to reverse, or a reasonable engineer would pick differently
without knowing the reasoning.

Every task in `tasks/` lists the ADRs it implements. An implementing agent reads those
ADRs before writing code.

## Consequences

- Agents get a stable, checked-in source of intent that survives context compaction.
- ADRs are versioned with the code, so the reasoning and the implementation drift
  together and drift is visible in review.
- A decision that changes gets a new ADR marking the old one superseded, rather than an
  in-place edit — history stays readable.
- Small overhead per decision. Accepted; the alternative is re-deciding.

## Alternatives considered

- **A single `DECISIONS.md`.** Cheaper to start, but grows into an unsearchable wall and
  makes superseding awkward.
- **Nothing; rely on code and commit messages.** Commit messages explain what changed,
  not what was rejected. The rejected options are the expensive part to rediscover.
