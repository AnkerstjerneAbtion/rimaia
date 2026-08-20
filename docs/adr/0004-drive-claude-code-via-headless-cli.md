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
