# 4. Drive Claude Code through the headless CLI

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

Rimaia must run the full Claude Code agentic loop — file reads and edits, bash, git,
subagents, MCP tools, hooks, `CLAUDE.md` discovery — against a git worktree, unattended,
using the user's existing Claude Code subscription rather than a metered API key.

Three ways to reach that loop:

1. The **Claude Agent SDK** (`@anthropic-ai/claude-agent-sdk`) in a bundled Node sidecar
   process, with the Rust backend supervising it.
2. The **Claude Code CLI in headless mode** (`claude -p ... --output-format stream-json`),
   spawned directly from Rust.
3. Reimplementing the loop against the Messages API. Not seriously on the table — it
   discards the harness that is the entire point, and API keys bypass the subscription.

The Agent SDK is the Claude Code harness packaged as a library; it drives the same binary
underneath. Wrapping it in a Node sidecar means shipping a Node runtime, an IPC protocol,
and a second process to supervise — to reach a CLI that Rust can already spawn.

The installed CLI (verified against 2.1.234) exposes everything the orchestrator needs:

| Flag | Use |
| --- | --- |
| `-p, --print` | Non-interactive run |
| `--output-format stream-json` | Newline-delimited JSON event stream |
| `--verbose` | Required for full event detail with `stream-json` |
| `--session-id <uuid>` | Pre-assign the session id instead of parsing it back |
| `--resume <id>` | Continue an interrupted session (ADR-0011) |
| `--permission-mode <mode>` | Unattended permission posture (ADR-0012) |
| `--append-system-prompt` | Inject orchestrator-level instructions |
| `--mcp-config` / `--strict-mcp-config` | Control which MCP servers the run sees |
| `--model`, `--effort` | Per-task model and effort selection |
| `--add-dir` | Extra readable directories beyond the worktree |

## Decision

Spawn the **`claude` CLI in headless mode** as a child process from the Rust backend, one
process per run, with the worktree as its working directory.

- Invocation shape:
  `claude -p --output-format stream-json --verbose --session-id <uuid> [--resume <uuid>]`,
  cwd set to the task's worktree, prompt delivered on stdin.
- **Runs inherit the operator's own Claude Code configuration by default** — their MCP
  servers, hooks and plugins are capability, not noise. A single Settings toggle switches
  the whole app between `inherit` and `strict_local`. Inherited `CLAUDE_*`
  process-identity env vars are always stripped. See the amendment below.
- The backend parses the stdout event stream line by line and maps events to run state,
  live UI updates, and the persisted transcript (ADR-0013).
- Rimaia generates the session id up front (`--session-id`) so resume works even if the
  process dies before emitting its `init` event.
- The CLI is a **prerequisite**, not a bundled dependency. Startup checks that `claude` is
  on `PATH` and reports its version; a missing or too-old CLI is a first-run error with a
  clear message, not a mid-run failure.
- Process supervision — spawn, stream, timeout, kill on cancel, reap on app exit — lives
  in one Rust module so retry and resume logic has a single place to hook into.

## Consequences

- No Node runtime, no sidecar, no IPC layer. One binary to supervise, and it is the same
  binary the user already trusts interactively.
- Authentication is inherited from the user's existing Claude Code login. Rimaia never
  handles credentials.
- We are coupled to the CLI's flag surface and event schema across versions. Mitigations:
  pin a minimum version at startup, treat unknown event types as opaque and log them
  rather than erroring, and keep the parser tolerant.
- Everything Claude Code does interactively — hooks, skills, `CLAUDE.md`, project MCP
  servers, subagents — works in the run, because it is the same harness. This is the main
  reason for the choice.
- The typed conveniences of the Agent SDK (`canUseTool` callbacks, typed message unions)
  are unavailable. We hand-roll a tolerant event parser instead; a small, contained cost.
- Cross-platform spawning needs care on Windows (no `.cmd` shim assumptions, correct
  process-group kill). Handled in the process module.

## Alternatives considered

- **Node sidecar running the Agent SDK.** Ships a Node runtime and an IPC protocol to
  reach the same binary. If we later need per-tool permission callbacks that headless mode
  cannot express, this becomes worth revisiting — the runner module is the seam.
- **Bundling the CLI as a Tauri sidecar binary.** Removes the install prerequisite but
  pins users to whatever version we shipped and complicates subscription auth. The user
  running this already has Claude Code installed.

---

## Amendment, 2026-08-20 — verified by spike against CLI 2.1.234

The decision stands and was confirmed end to end: a plan went in, a branch with commits and
passing tests came out, unattended, with `bypassPermissions` and no stalls. Full write-up in
[`spike/FINDINGS.md`](../../spike/FINDINGS.md). Three things the spike found that this ADR
did not anticipate:

### Environment inheritance is a setting, defaulting to inherit

A run inherits the operator's entire personal Claude Code environment unless told
otherwise. Measured, same one-word prompt:

| | Inherited (default) | Isolated |
| --- | --- | --- |
| Tools exposed | 255 | 26 |
| MCP servers connected | 2 | 0 |
| `SessionStart` hooks fired | 4 | 0 |
| Cost | $0.1061 | $0.0291 |

**Decision: inherit by default, with one toggle in Settings.** The operator's MCP servers
are capability, not noise — a run that can reach the issue tracker, the design tool, or the
org's own knowledge base while implementing is more useful than an isolated one, and that
is a large part of why this is a local desktop app on the user's own machine at all.

A single app-level setting, `run_environment`, with two modes:

| Mode | Behaviour |
| --- | --- |
| `inherit` (default) | The operator's full Claude Code environment: MCP servers, hooks, plugins, output styles |
| `strict_local` | `--strict-mcp-config --setting-sources project,local` — the repository's own `CLAUDE.md` and project settings only |

Two modes, one control, no per-repository matrix to reason about. (Do not implement
`strict_local` with `--bare`; it also disables `CLAUDE.md` discovery, which we want in both
modes.) A per-repository override is a plausible later refinement, deliberately not built
until something asks for it.

Two consequences the UI must own rather than hide:

- **Cost.** Inheriting a large tool set costs roughly 3.6× per run before any work happens.
  Since `result` reports `total_cost_usd`, show per-run cost and put the mode toggle within
  reach of it.
- **Hooks change agent behaviour.** A personal `SessionStart` hook injects instructions
  into every run — during the spike one altered the agent's output style. That is fine when
  intended and confusing when forgotten. The `init` event lists what loaded; task 018's
  doctor should report the hooks and MCP servers a run will inherit, so it is a visible
  choice rather than a surprise at 2am.

### Inherited `CLAUDE_*` env vars are stripped regardless

Separate from the setting above, and not configurable. Claude Code exports 13 `CLAUDE_*` /
`CLAUDECODE` variables into its children, including `CLAUDE_CODE_SESSION_ID` and
`CLAUDE_CODE_CHILD_SESSION`, which tell the child it is a nested session of the parent.
That is a leak of process identity, not user configuration — inheriting it makes the child
misreport which session it is. Rimaia will routinely be developed and tested from inside a
Claude Code session, so this is a live hazard rather than a theoretical one.

### The event stream is wider than assumed

Beyond `system`/`init`, `assistant`, `user` and `result`, real runs emit
`rate_limit_event`, `system/thinking_tokens`, `system/vcs_state_changed`, and
`system/hook_started` + `hook_response`. None of these appear in `--help`.

The tolerant-parsing rule is load-bearing, not defensive. Switch on `system.subtype` and
treat unknown subtypes as opaque. Parse the JSON properly — naive substring matching on
`"type":"` mis-parses, because assistant events nest `"type":"message"` inside.

### `init` echoes the applied configuration

`system/init` returns `permissionMode`, `model`, `cwd`, `tools[]`, `mcp_servers[]` and
`apiKeySource`. The runner should **verify** that the permission mode and isolation it
asked for were actually applied, rather than assuming — a cheap guard against a CLI
change silently widening permissions. `apiKeySource: "none"` confirms subscription auth.

---

## Amendment, 2026-08-28 — the strategy run is always `strict_local`

ADR-0016's planned mode (task 020) spawns a **second** child per task: a short, cheap
strategy run that reads the plan and decides model, effort and workflow shape before the
implementation run starts. It is a different kind of run, and it does not take the
`run_environment` setting.

**Whatever `run_environment` says, a strategy run spawns with
`--strict-mcp-config --setting-sources project,local`.** This contradicts the sentence
above — "A single Settings toggle switches the whole app between `inherit` and
`strict_local`" — and the contradiction is stated here rather than left as a surprise in
the runner. Two reasons, either sufficient on its own.

**Cost.** The table above measures inheritance at roughly 3.6× per run before any work
happens, and this is the one run whose entire premise is being cheap. The planner exists to
decide whether the implementation run deserves Opus; a planner that spends $0.10 loading
255 tools it will never call has already spent most of what the decision might save. Its
complete output is one MCP call, on a fast model, at low effort, inside six turns. Here the
inherited tool set is not capability. It is the bill.

**Security.** `--strict-mcp-config` is what makes the `--mcp-config` the runner passes the
*complete* list of MCP servers rather than an addition to the operator's. It is therefore
what guarantees that the only server the planner can reach is the scoped Rimaia handle
ADR-0006's 2026-08-28 amendment describes. Without it, a run holding a token that names a
task would also be holding the operator's issue tracker, design tool and knowledge base —
and scoping one door in a room with several doors is not scoping. The same property covers
hooks: the amendment above notes a personal `SessionStart` hook once altered the agent's
output style, which is tolerable for implementation and not for a run whose answer is a
single tool call against a fixed schema.

`--setting-sources project,local` still leaves `CLAUDE.md` discovery on, so the planner
reads the repository's own conventions — which is context it wants. The prohibition on
`--bare` stands for the same reason it does above.

The "two modes, one control, no per-repository matrix" argument survives, because this is
not a matrix creeping back in. `run_environment` remains one setting choosing one thing:
what the *implementation* run inherits, which is a real trade the operator makes between
capability and cost. The strategy run has no such trade to offer — nobody wants their
planner reaching the issue tracker — so its environment is not configuration at all, and is
not surfaced as any kind of override.
