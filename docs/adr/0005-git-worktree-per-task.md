# 5. One git worktree and branch per task

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

Rimaia runs agents against real repositories, potentially several at once, while the user
may still have the same repository checked out and be working in it. Requirements:

- A run must never touch the user's working copy or `HEAD`.
- Parallel runs on the same repository must not collide.
- A failed or half-finished run must leave inspectable state, not a dirty main checkout.
- The user must be able to review the result — as a branch, a diff, or a PR.

Git worktrees give exactly this: multiple working directories over one object store, each
on its own branch, sharing history and fetch state.

## Decision

Every task run gets its **own git worktree on its own branch**.

- **Location:** worktrees live outside the repository, under the app data directory:
  `<app-data>/worktrees/<repo-slug>/<task-id>/`. Never inside the repo, so they can't be
  accidentally staged, and never near the user's own worktrees.
- **Branch naming:** `rimaia/<task-id>-<slugified-title>`, truncated to a safe length.
  The task id prefix guarantees uniqueness; the slug makes it readable in a PR list.
- **Base ref:** the repository's configured default branch, unless the task has
  dependencies — see ADR-0008 for branch chaining.
- **Creation** is `git worktree add <path> -b <branch> <base-ref>` after a fetch, with
  branch-name collisions resolved by suffixing rather than reusing.
- **Reuse:** a retried or resumed run reuses the existing worktree and branch. Retries
  continue work in place — they do not start from a clean tree (ADR-0011).
- **Cleanup** is explicit and never automatic on failure. A worktree is removed when the
  task reaches `Done` and the user (or a policy setting) approves cleanup, or on demand
  from the UI. Removal is `git worktree remove` plus `git worktree prune`; the branch is
  left alone unless the user asks for it to go.
- **Repository state on disk is authoritative.** The database records the worktree path
  and branch; on startup, entries whose worktree has vanished are reconciled rather than
  trusted.

## Consequences

- The user's checkout is untouched. They can keep working while runs proceed.
- Parallel runs are naturally isolated, which is what makes ADR-0010's parallel mode safe.
- Disk usage grows with retained worktrees. Each is a full checkout (objects are shared,
  files are not). The UI surfaces worktree count and size, and cleanup is one action.
- Repositories with submodules, LFS, or heavy `postCheckout` hooks are slower to
  materialize and may need extra setup. Out of scope for MVP; documented as a limitation.
- Some repos need per-worktree setup that a fresh checkout lacks — `.env` files, installed
  dependencies. Post-MVP: a per-repository "files to copy" and "setup command" hook,
  mirroring what Conductor does.
- `git worktree` requires the repo to be a real git repository with a valid default
  branch. Registration validates this up front (task 003).

## Alternatives considered

- **Clone per task.** Full copy of history for every task; slow and wasteful on large
  repos, and diverges from the user's remotes.
- **Run in the user's checkout on a branch.** Simple, and immediately wrong: it fights the
  user for `HEAD`, serializes all runs, and one failed run leaves their working copy
  dirty.
- **Containerize each run.** Better isolation, but it breaks subscription auth, local
  toolchains, and the user's `~/.claude` configuration — the things that make the run
  behave like their own Claude Code.
