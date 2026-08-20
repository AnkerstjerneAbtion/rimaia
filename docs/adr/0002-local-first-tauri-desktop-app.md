# 2. Local-first Tauri desktop app

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

Rimaia orchestrates Claude Code runs against local git repositories using the developer's
own Claude Code subscription. That constrains where it can run:

- The Claude Code CLI authenticates with the user's subscription credentials on their
  machine. There is no server-side equivalent of "use my Max plan".
- Work happens in git worktrees on the local filesystem, next to the repositories being
  worked on.
- The MCP server must be reachable from Claude Code sessions the user is already running
  locally (in a terminal, in Conductor, in an IDE).

A hosted service would need to replicate the repositories, hold API credentials, and
still could not use a personal subscription. The problem is local by nature.

The repository is already scaffolded as a Tauri 2 app with React 19, TypeScript and Vite.

## Decision

Rimaia is a local-first desktop application built with **Tauri 2** — a Rust backend
(`src-tauri/`) and a React + TypeScript frontend (`src/`).

- All orchestration, process supervision, git operations, persistence, and the MCP server
  live in the Rust backend.
- The frontend is a view layer: Kanban board, task editor, run logs, settings. It holds no
  orchestration state of its own and talks to the backend through Tauri commands and
  events.
- No network service, no account, no cloud sync. All state lives on disk in the user's
  application data directory.
- The app targets macOS first. Windows and Linux are kept viable — no macOS-only APIs in
  the core — but are not tested per-release until there is a reason to.

## Consequences

- A single process supervises the run queue. Runs stop when the app quits; there is no
  headless daemon. Acceptable for the intended workflow (start a queue in the evening,
  leave the machine on, review in the morning) and revisitable later.
- Rust owns the parts that must not silently fail: subprocess lifecycle, git, the
  database, retry timers. The frontend cannot corrupt run state by crashing or reloading.
- Long-running work outlives the window. UI state must be derived from the database, not
  from React state, so closing and reopening a view shows the truth.
- Cross-platform packaging is Tauri's problem, not ours — but Tauri does not
  cross-compile, so each target builds on its own OS.

## Alternatives considered

- **Electron.** Larger runtime, and the orchestration core would end up in Node where
  process supervision and file locking are weaker. The one advantage — running the
  TypeScript Claude Agent SDK in-process — is unnecessary given ADR-0004.
- **CLI + TUI only.** Fastest to build, but a Kanban board with drag-to-reorder priority
  is genuinely better as a GUI, and reordering priority is a core interaction.
- **Web app with a local agent.** Two deployment targets and an auth boundary, for no
  gain in a single-user local tool.
