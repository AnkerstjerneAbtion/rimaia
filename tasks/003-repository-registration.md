---
id: "003"
title: Repository registration
milestone: mvp
status: ready
depends_on: ["002"]
adrs: ["0002", "0005", "0012"]
size: S
---

# Repository registration

## Goal

Let the user register a local git repository, validated, so tasks have something to run
against — and gate unattended execution behind an explicit per-repository opt-in.

## Why now

Task 007 needs a validated repo path and default branch to create worktrees from.

## Scope

- Settings → Repositories: list, add (native folder picker), edit, remove.
- Validation on add, with specific errors rather than a generic failure:
  - path exists and is a directory
  - it is a git repository (not a worktree of another one)
  - it has at least one commit
  - a default branch can be determined
- Default branch detection: `origin/HEAD` if present, else `main`, else `master`, else the
  current branch. Always editable afterwards.
- Derived display name from the directory name; editable.
- `worktree_root` defaults to `<app-data>/worktrees/<repo-slug>/`, shown and overridable.
- Detect the remote URL and whether `gh` is available and authenticated for it — used by
  base instructions that ask for a PR. A missing `gh` is a warning on the repo, not an
  error.
- **Unattended runs opt-in** (ADR-0012): off by default. Enabling it requires confirming a
  dialog that states plainly that agents will run arbitrary commands in this repository's
  worktrees without asking. Tasks in a repository without the opt-in cannot be started;
  the run button explains why.
- Removing a repository with tasks: refuse, and say how many tasks reference it.

## Out of scope

- Cloning from a remote. Local paths only.
- GitHub organization or project import.

## Acceptance criteria

- A valid repo registers, showing correct default branch and remote.
- Each invalid case produces its own specific message.
- The unattended opt-in dialog states the permission scope in plain language and defaults
  to off.
- Attempting to run a task in a non-opted repository is blocked with a clear reason.
- Registration survives restart.

## Notes

Use `git` via subprocess rather than a git library. Every environment that can run Claude
Code has `git`, worktree semantics match exactly what the user would see in their own
shell, and there is no library version skew to debug at 2am.
