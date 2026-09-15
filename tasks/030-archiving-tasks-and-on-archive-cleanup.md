---
id: "030"
title: Archiving tasks, and on-archive cleanup
milestone: v0.3
status: ready
depends_on: ["005", "016"]
adrs: ["0005", "0007", "0021", "0022", "0025"]
size: L
---

# Archiving tasks, and on-archive cleanup

## Goal

A task can be archived — from its card's checkbox in bulk, or from its detail panel one at a
time. It leaves the board, keeps its runs and its history, and can be unarchived from an
archive view. Archiving is also the moment a repository's configured cleanup runs: nothing,
Rimaia's own guarded worktree removal, or an executable the user named.

## Why now

Six weeks of nightly runs leave a board that is mostly cards nobody will look at again, and
the only tool for that today is `delete_task` — which cascades to every one of the task's
`runs` rows, i.e. destroys the month of cost and model history ADR-0022 exists to preserve.
Tidying the board and keeping the record are currently the same button, pointed opposite
ways.

Task 016 built the worktree guards and an auto-cleanup policy, but hung it on the move to
`done`, which is a statement about *review* — the moment you might still want to look.
The moment somebody is genuinely finished with a task is the moment they take it off the
board, and that moment does not exist yet.

## Scope

- `tasks.archived_at`, and `list_tasks` filtered on it with `Active` as the default, so the
  scheduler, the plan pass, the MCP tool and the board all exclude archived rows unedited
  (seam-contract D26.1).
- `archive_task`, `archive_tasks`, `unarchive_task` in `rimaia-core`, as Tauri commands, and
  as MCP tools — operator scope only (ADR-0025 point 8).
- Per-repository cleanup slot: `none` | `remove_worktree` | `script`, one field, mutually
  exclusive by construction; `set_repository_on_archive` with strict validation of the
  script path (absolute, exists, a file, executable).
- The script runner: argv of one, repository root as cwd, context in `RIMAIA_*` env,
  `CLAUDE_*` stripped, own process group, an **injected** timeout, output redacted and
  carried back (seam-contract D26.4 and D26.5).
- Board: bulk "Archive N selected" on the existing picked set, a "Show archive" toggle and
  an archive list with unarchive; an archive section in the task detail panel whose
  confirmation names what the repository's cleanup will do.
- Settings → Repositories: the cleanup slot, with the acknowledgement gate
  `worktree_auto_cleanup` already has, and copy that says a script gives up Rimaia's guards.
- The picked set is pruned when a card disappears, and its checkbox label stops saying
  "for planning" (seam-contract D26.6).

## Out of scope

- A global default cleanup script. ADR-0025 point 4 refuses one.
- Auto-archiving on any transition. Archiving is a person deciding; there is no policy
  setting and no schedule.
- Deleting anything an archive did not delete. `delete_task` is unchanged, and remains the
  only thing in the product that removes a `runs` row.
- Bulk *delete*. The picked set drives archiving and planning; deleting stays one card at a
  time behind its own confirmation.

## Acceptance criteria

- Archiving a `running` or `waiting_retry` task is refused, and there is no flag, force or
  confirmation anywhere in the codebase that makes it possible.
- Archiving a `queued` task takes it out of the next selection pass — asserted against
  `scheduler::selection`, not against `list_tasks`.
- An archived task keeps every `runs` row, every link and every dependency edge, and a task
  that depends on it still reports it by title as the blocker.
- Unarchiving returns the card to the column it was in.
- A bulk archive of ten cards where one is running archives nine and reports the tenth by
  title, with its reason.
- With `on_archive = remove_worktree`, a worktree with uncommitted changes is **refused**
  and the task is still archived; the refusal is reported.
- With `on_archive = script`, the script runs with the documented environment, its output is
  redacted, a non-zero exit is reported, and the task is still archived.
- A script that never exits is killed at the timeout — with an injected clock and no `sleep`
  in the test.
- A script path that is relative, missing, a directory or not executable is refused when the
  setting is written, not when the archive fires.
- `archive_task`, `archive_tasks`, `unarchive_task` and `set_repository_on_archive` are
  registered MCP tools and are refused for a run-scoped handle.
- `./scripts/check-command-wiring.sh` passes: both `generate_handler!` lists agree and every
  new `commands.ts` name is registered.

## Notes

ADR-0025 is the decision; read all of it. The three places this task is most likely to go
quietly wrong are in seam-contract D26: the filter default (D26.1), the fact that the script
bypasses every guard task 016 built and that this is deliberate (D20's 2026-09-15
amendment), and the shell rule — the setting holds **a path**, never a command line, because
splitting a command line correctly is `sh -c` with our own bugs (ADR-0025 point 5).

Deletion is still the one irreversible thing this app does, and archiving is now the one
thing that can *trigger* an irreversible action on the user's behalf. Task 016's rule
applies unchanged: if in doubt, refuse and explain.
