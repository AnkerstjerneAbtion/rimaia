---
id: "018"
title: Preflight doctor and packaging
milestone: v0.3
status: ready
landed: "#17"
depends_on: ["008"]
adrs: ["0004", "0012"]
size: S
---

# Preflight doctor and packaging

## Goal

Catch environment problems at launch instead of at 2am, and produce a distributable build.

## Why now

Unattended runs fail expensively. Every check here converts a wasted night into a
five-second warning.

## Scope

**Doctor** — a startup check, re-runnable from Settings:

| Check | Failure mode it prevents |
| --- | --- |
| `claude` on `PATH`, version ≥ minimum | Every run fails immediately |
| `claude` authenticated | Every run fails with an auth error |
| `git` available and version sufficient for worktrees | Worktree creation fails |
| `gh` present and authenticated (per repository) | Base instructions ask for a PR that cannot be opened |
| App data directory writable | Nothing persists |
| Free disk space above a threshold | Worktrees and logs fail mid-run |
| Registered repository paths still exist and are valid | Runs fail at worktree creation |
| MCP port free | Silent handoff failure from other sessions |

- Each result is pass / warn / fail with a specific remediation string.
- **Fails block queue start** with an explanation. Warnings do not.
- The doctor runs automatically before a scheduled queue starts (task 013), so a broken
  environment is reported in the evening rather than discovered in the morning.

**Packaging**

- Tauri bundle configuration: app id, product name, icons, version.
- macOS build and signing notes in the README; Windows and Linux documented as
  build-on-target (Tauri does not cross-compile).
- README rewritten for the actual product: what it does, prerequisites, first-run setup
  (register a repo, enable unattended runs, add the MCP server), and the ADR index.
- A first-run screen that walks: register a repository → enable unattended runs → set base
  instructions → add the MCP server.

## Acceptance criteria

- Renaming the `claude` binary produces a blocking doctor failure with a useful message,
  not a mid-run crash.
- An unauthenticated `gh` produces a warning naming the affected repository.
- Queue start is blocked while any check fails, and the reason is on screen.
- `npm run tauri build` produces a working macOS bundle that launches, finds its data
  directory, and runs a task.
- README is accurate for someone setting this up from scratch.

## Notes

Every check on that list corresponds to a way an overnight queue can waste a night. That
is the criterion for adding more.
