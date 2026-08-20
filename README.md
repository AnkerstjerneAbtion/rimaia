# Rimaia

*Review in the morning, agent in the afternoon.*

A local-first desktop app for queueing implementation plans and letting Claude Code work
through them unattended — in git worktrees, on your own subscription — so the results are
waiting to be reviewed the next morning.

- **Kanban board** — Not ready · Ready · In review · Done. Board order is execution order.
- **Plans in, branches out** — each task carries a plan, extra instructions, and links.
- **Git worktree per task** — your checkout is never touched.
- **Local MCP server** — hand plans to Rimaia from the Claude Code session you planned in.
- **Unattended runs** — sequential or parallel, scheduled, resuming through usage limits.

Status: **design complete, implementation not started.** The scaffold below is a Tauri 2 +
React 19 + TypeScript starter with a placeholder counter UI.

## Design

- [Architecture decision records](docs/adr/README.md) — 17 ADRs covering the stack, how
  Claude Code is invoked, worktree strategy, dependency semantics, permission posture,
  testing, and MVP scope.
- [Task backlog](tasks/README.md) — 21 tasks; the table defines the order. The first ten
  are the MVP walking skeleton.
- [CLAUDE.md](CLAUDE.md) — working agreement for agents implementing this.

## Prerequisites

- Node.js 20+ and npm
- Rust stable (`rustup`)
- Platform toolchain per the [Tauri prerequisites](https://tauri.app/start/prerequisites/):
  - **macOS**: Xcode Command Line Tools
  - **Windows**: MSVC build tools + WebView2 runtime
  - **Linux**: `webkit2gtk-4.1`, `libappindicator3`, `librsvg2`, `patchelf`

## Development

```bash
npm install
npm run tauri dev
```

## Build

```bash
npm run tauri build
```

Bundles are written to `src-tauri/target/release/bundle/`. Tauri does not cross-compile — build each target on its own OS (or in CI).

## Layout

| Path                        | Purpose                              |
| --------------------------- | ------------------------------------ |
| `src/`                      | React frontend                       |
| `src-tauri/src/`            | Rust backend, entry point in `lib.rs` |
| `src-tauri/tauri.conf.json` | Window, bundle and app config        |
| `src-tauri/capabilities/`   | Permissions granted to the frontend  |
