---
id: "007"
title: Git worktree service
milestone: mvp
status: ready
depends_on: ["003"]
adrs: ["0005"]
size: M
---

# Git worktree service

## Goal

Create, inspect, and remove the per-task git worktree and branch that a run executes in.

## Why now

Task 008 spawns Claude Code with a worktree as its working directory. Nothing runs without
this.

## Scope

**`worktree/` module**, all git operations via subprocess with captured stdout/stderr:

- `prepare(task, repo) -> Worktree` —
  - `git fetch --prune` on the repo (best effort; offline is a warning, not a failure)
  - resolve the base ref: default branch, or a dependency's branch once 011 lands
  - `git worktree add <path> -b <branch> <base-ref>`
  - path `<worktree_root>/<task-id>/`, branch `rimaia/<task-id>-<slug>`
  - branch-name collision resolved by numeric suffix, never by reuse
  - idempotent: if the worktree already exists and is valid, return it unchanged (this is
    what makes retries resume in place)
- `status(task) -> WorktreeStatus` — exists, branch, ahead/behind base, dirty, commit
  count, diff stat.
- `diff_summary(task)` — files changed, insertions, deletions, and the list of commits on
  the branch. Feeds the review view (013 in ADR-0013's ordering, task 015 here).
- `remove(task, delete_branch: bool)` — `git worktree remove` (with `--force` only on
  explicit user confirmation), then `git worktree prune`.
- `reconcile()` at startup — worktree paths recorded in the database that no longer exist
  on disk are cleared and the task's run state corrected.

**Safety**

- Refuse to create a worktree inside the repository working tree.
- Refuse to operate on a path outside the configured `worktree_root`.
- Every git invocation logs its command line and exit status at debug level.

**UI**

- Task detail shows branch, worktree path, and status; "Open in Finder/Explorer" and
  "Copy path" actions.

## Out of scope

- Automatic cleanup policy (016).
- Submodule and LFS handling — documented as a known limitation.

## Acceptance criteria

- Creating a worktree yields a real checkout on the correct branch from the correct base.
- Calling `prepare` twice returns the same worktree and does not error.
- Slugs longer than the branch-name limit are truncated safely; a colliding branch name
  gets a suffix rather than reusing the branch.
- Removal cleans up both the directory and git's worktree metadata.
- Deleting the worktree directory behind the app's back is reconciled at next startup
  instead of causing a run to fail confusingly.
- Repository paths containing spaces work (argument passing, not shell strings).

## Notes

Never shell out through `sh -c`. Build argument vectors so paths with spaces and quotes
cannot become injection.
