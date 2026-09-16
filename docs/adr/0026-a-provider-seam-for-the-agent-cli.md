# 26. A provider seam for the agent CLI, drawn from Rimaia's needs

- **Status:** Accepted
- **Date:** 2026-09-16

## Context

Rimaia drives exactly one agent CLI, and there is no provider abstraction anywhere.
Three modules carry all of it:

- `runner/process.rs` emits Claude's flag vocabulary directly — `Invocation::args`
  is eleven fields in and ten Claude flags out.
- `runner/events.rs` hand-extracts Claude's wire field names: `permissionMode`,
  `terminal_reason`, `rate_limit_info.resetsAt`.
- `runner/outcome.rs` branches on Claude's terminal vocabulary — `completed`,
  `aborted_streaming`, `error_max_turns`, `"allowed"`.

Everything else in `rimaia-core` is already provider-agnostic. The board, the
scheduler, worktrees, dependencies, the retry policy, the transcript files and the
MCP server never learn what spawned a run.

The goal here is **not** to add a second provider. It is to draw the seam while
Claude is still the only implementation, so that adding one later is "implement the
trait". The refactor is cheap against one implementation and expensive against two.

**The failure mode this ADR exists to prevent** is a trait that is Claude's flag
surface with renamed methods. A second provider would then arrive as a degraded
Claude: a session id it must fabricate, a tool blocklist it must silently drop, an
`--append-system-prompt` it must fold into the prompt. Every structural mismatch
becomes a lie told in a `match`.

Four mismatches are known, and the decision below is validated against them. They
are constraints on the design, not a plan to implement any of them:

1. **Pre-minted session id.** Rimaia mints a uuid before spawn (`--session-id`) so
   `--resume` survives a child that dies before `init`. Another provider mints its
   own and announces it. `runs.session_id` is `NOT NULL`, and `scheduler::attempts`
   uses it as the **retry-budget boundary**, so this is not cosmetic.
2. **MCP injection.** `mcp::RunHandles::mcp_config_json` emits Claude's
   `{"mcpServers":{"rimaia":{"type":"http","url":…}}}` and it is passed as
   `--mcp-config`. A provider that reads its MCP servers from a config home needs a
   file plus an environment variable, not a flag.
3. **Per-tool blocklist.** ADR-0012's `--disallowedTools` carries eleven Claude
   permission-rule strings, plus every `mcp__rimaia__*` tool, plus the planner's
   `Write`/`Edit`/`NotebookEdit`/`Bash`. A provider with coarse sandbox modes and no
   deny list cannot express any of it. That is **safety posture, not a rename**.
4. **Usage limits.** `scheduler::retry::decide` consumes an epoch instant from
   `rate_limit_info.resetsAt`. Another provider reports a window *relative* to the
   moment it spoke.

**ADR-0004 is not superseded.** Driving a headless agent CLI as a child process,
over `stream-json` on stdout, with the prompt on stdin, remains the decision — as
does never bundling the binary. This ADR amends *which* CLI, not *how*.

## Decision

**The provider owns two things — how an intent becomes a child process, and what
one line of its output means. Everything else stays Rimaia's.**

### 1. The seam is `RunIntent → SpawnPlan` and `line → RunEvent`; process supervision is on Rimaia's side of it.

A provider that spawned its own child could leak a process tree, skip redaction, or
write a transcript Rimaia cannot read, and `spike/FINDINGS.md` §7's process-group
work would have to be re-proved per provider. The three-way split `runner/` already
has — line, ending, process — is the split that generalises.

The rule that keeps this honest: **the trait may not return events, outcomes or
runs.** It returns only values. Everything that touches a pipe, a signal or a
process group is written once, in `runner::process`. The falsification is simple —
if a provider implementation can satisfy the trait without any process ever being
spawned, the seam is cut in the wrong place.

### 2. The trait returns a `SpawnPlan`, never a bare argument vector.

A flag on one provider is a config file plus an environment variable on another. A
trait whose spawn method returns `Vec<String>` cannot express MCP injection for a
config-home provider at all, and that alone would force this ADR open again on the
first real second provider. A `SpawnPlan` carries argv, an environment delta in both
directions, and what goes on stdin.

### 3. Classification is not a trait method. It stays Rimaia's policy over a neutral vocabulary the provider supplies.

ADR-0011 puts classification "in one module with unit tests over captured CLI
output"; a provider that could answer `ExitClass` directly could satisfy the trait
with no stream involved at all. The six classes are Rimaia's state machine, not a
wire format. The provider answers `EndReason` and `UsageWindow`; Rimaia decides what
they mean.

### 4. `ForbiddenOperation` names git operations, not permission-rule strings, and an operation a provider cannot enforce refuses an unattended run.

ADR-0012 grants `bypassPermissions` *because* its mitigations are part of the
feature; dropping point 3 silently would make the per-repository opt-in a statement
about a posture that no longer exists. Degrading with a warning is the one answer
this axis does not get. A manual run — a human is present — is the only exception,
for the reason point 3 exists in the first place.

### 5. `runs.session_id` holds a Rimaia-minted conversation id; the provider's own session id is never persisted.

`scheduler::attempts` has always used it as the retry-budget boundary, which is a
statement about Rimaia's intent that merely coincided with Claude's id. A
per-(provider, task) home makes "continue the last conversation here" exact, and a
provider that resumes by id recovers it from the transcript the previous attempt
already wrote. `NOT NULL` holds, `attempts::fold` is untouched, and **no migration
is required** — which is why seam-contract D4's closed list of migrations stands.

### 6. Whether a resume sends the composed prompt or the one-line continuation is the provider's ability to continue, not `RunRequest::resume`.

A continuation prompt delivered into a fresh session produces an agent with no plan,
no context and an empty diff — a seam bug that would read as a bad model. Not
re-running the planner stays keyed off `request.resume`, because the effective model
and effort are already on the row and a second planner reading a half-finished
worktree could change them mid-chain.

### 7. A reset window is `At(instant)` or `After { duration, observed_at }`, with `observed_at` stamped by the clock when the line was read.

Resolving a relative window at `finish_run` time is wrong by however long the run
took to die, in the direction that wastes a night. `retry::decide`,
`USAGE_LIMIT_FALLBACK_POLL` and the jitter are unchanged, so ADR-0011's table stays
a pure function over one instant.

### 8. `UsageState::Unknown` is not a wall, and `Exhausted` latches.

A provider that reports percentages continuously must not have "93% used" read as a
limit — that raises the *global* pause on a healthy run. A heartbeat arriving after
a refusal must not clear the refusal, or a walled task backs off 1m/5m/15m into a
closed window and is abandoned by morning. Both are ADR-0011's named nightmare ("a
misclassified `usage_limit` looks like a hard failure at 2am") reached by routes
that only exist once a second provider's shape is taken seriously.

## Consequences

- **A provider can now refuse to run at all**, and the first refusal an operator
  meets will be a sentence about a capability, not about their repository. That is
  the intended trade for not silently dropping ADR-0012's mitigations.
- The escape from that refusal is an explicit, informed, **per-repository**
  acknowledgement of the specific operations a provider cannot enforce — the same
  mechanism ADR-0012 already chose for the same class of decision. It needs a
  column, therefore a migration, therefore seam-contract D4's "stop and ask".
  **It is deferred to whichever task ships the second real provider, and until then
  an unattended run on a provider without a blocklist simply refuses.**
- `crates/core/src/runs/transcript.rs` holds a **second, independent** Claude-shaped
  parser for the finished-log viewer. It is deliberately separate and this ADR does
  not touch it, so a second provider's transcript would render as opaque rows. Named
  here so it is not a surprise later.
- A stateless `parse_line` cannot fold a provider that streams deltas into one turn.
  Such a provider's deltas become `RunEvent::Other` — tolerated, not modelled. The
  upgrade path is a stateful interpreter with a `finish()`; it is not built on
  speculation.
- Claude Code appears in this product in **two unrelated roles**: the agent Rimaia
  drives, and (ADR-0006) an MCP client that hands plans *to* Rimaia. This ADR changes
  only the first. `claude mcp add` stays exactly as it is.

## Alternatives considered

- **One trait with `fn args(&self, invocation: &Invocation) -> Vec<String>`.** The
  smallest diff and the one the code shape invites. Loses because `Invocation`'s
  eleven fields are ten Claude flags, so the second provider arrives as a
  translation layer — and because argv cannot carry a config file.
- **Normalise every provider's stream into Claude's wire vocabulary and change
  nothing downstream.** Zero churn in `outcome.rs` and `scheduler/`. Loses on the
  usage-limit axis alone: a relative window has to be resolved inside the adapter at
  a moment the adapter cannot justify, and a refusal status has to be invented for a
  provider that never says it. It makes Claude's wire format the interchange format,
  which is the same defect one level down.
- **A `runs.provider_session_id` column so any provider persists its own id.** Would
  work. Loses because seam-contract D4's list of migrations is closed, because the
  task can be done without it, and because it would enshrine "the provider's id is
  the important one" exactly when the finding is that Rimaia's conversation id is.
- **Enforce ADR-0012's operations by wrapping `git` on the child's `PATH`.** Would
  give every provider the blocklist for free and need no capability negotiation.
  Loses because a wrapper an unattended `bypassPermissions` run can see it can also
  bypass — call the real binary by path, or use libgit2 — so it is a sandbox that is
  not one, which ADR-0012's Consequences explicitly refuse to pretend to have.
- **Ship a real second-provider variant now and stub what does not work.** Proves
  the seam against an actual target. Loses because a stub is a claim: the next person
  and the UI would read it as "that provider is supported". A deliberately fictional
  provider makes the same claim honestly — it is evidence about Rimaia's seam and
  nothing else — which is the discipline `SYNTHESIZED_UNOBSERVED` already enforces on
  the fixture corpus.
