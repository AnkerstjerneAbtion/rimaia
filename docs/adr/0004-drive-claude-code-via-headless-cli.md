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
- **The run environment is isolated from the operator's own Claude Code configuration**
  with `--strict-mcp-config --setting-sources project,local`, and by stripping inherited
  `CLAUDE_*` environment variables. See the amendment below — this is not optional.
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

### Run isolation is mandatory, not hygiene

Spawned without isolation flags, a run inherits the operator's entire personal Claude Code
environment. Measured, same one-word prompt:

| | Default | Isolated |
| --- | --- | --- |
| Tools exposed | 255 | 26 |
| MCP servers connected | 2 | 0 |
| `SessionStart` hooks fired | 4 | 0 |
| Cost | $0.1061 | $0.0291 |

A personal `SessionStart` hook injected an unrelated instruction into the run's context,
and two personal MCP servers were connected and callable. An overnight queue would inherit
whatever the operator happened to have configured that day.

**Required on every run:** `--strict-mcp-config` and `--setting-sources project,local`.
This keeps the repository's own `CLAUDE.md` and project settings — which we want — while
dropping user-level hooks, plugins, and output styles. Do not use `--bare`; it also
disables `CLAUDE.md` discovery.

**Also required:** strip the 13 `CLAUDE_*` / `CLAUDECODE` variables Claude Code exports
into its children. Rimaia will routinely be developed and tested from inside a Claude Code
session, and without stripping, the child does not behave like a fresh run.

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
