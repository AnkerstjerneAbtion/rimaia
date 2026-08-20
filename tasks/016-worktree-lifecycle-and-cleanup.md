---
id: "016"
title: Worktree lifecycle and cleanup
milestone: v0.3
status: ready
depends_on: ["007", "009"]
adrs: ["0005"]
size: S
---

# Worktree lifecycle and cleanup

## Goal

Keep worktrees from accumulating without ever deleting work the user has not finished
with.

## Why now

Weeks of nightly runs leave dozens of full checkouts on disk. This becomes a real problem
once the queue is used daily, and not before.

## Scope

- Settings → Storage: every worktree with its task, branch, size, last activity, and
  whether its branch is merged into the repository's default branch.
- Cleanup actions:
  - Remove a single worktree (keeping or deleting its branch — separate, explicit choices)
  - Remove all worktrees for tasks in `done`
  - Remove all worktrees whose branch is merged into the default branch
- Optional policy setting: auto-remove the worktree when a task moves to `done`. **Off by
  default.**
- Guards, all of them:
  - Refuse to remove a worktree with uncommitted changes unless explicitly forced, with
    the change count shown
  - Refuse to remove a worktree with unpushed commits unless explicitly forced
  - Never remove a worktree for a `running` or `waiting_retry` task
  - Never delete a branch that is not merged, without a separate confirmation
- `git worktree prune` after removal, and stale-entry cleanup for worktrees deleted
  outside the app.
- Total worktree disk usage shown alongside run-log usage from task 015.

## Acceptance criteria

- Cleanup frees the expected disk space and leaves no stale `git worktree list` entries.
- Every guard above is enforced and tested, including the uncommitted-changes case.
- Auto-cleanup is off by default; enabling it requires acknowledging what it deletes.
- Deleting a worktree directory outside the app is reconciled at next startup.

## Notes

Deletion is the one irreversible thing this app does. Every guard here earns its place —
if in doubt, refuse and explain.
