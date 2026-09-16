# 25. Archiving a task, and what an archive is allowed to clean up

- **Status:** Accepted
- **Date:** 2026-09-15

## Context

There is exactly one way to get a card off the board: `delete_task`, from the detail
drawer, one card at a time. It is irreversible and it cascades — the task's links, its
outgoing dependency edges and **every one of its `runs` rows** go with it. ADR-0022
part 2 spends its whole argument on making a `runs` row survive pruning, and then
seam-contract D20 point 5 has to write down the one exception: deleting a task is "a
person saying *this never happened*", so it takes the history too.

That is the right shape for a mistake. It is the wrong shape for the common case, which
is a task that went fine. The record is worth keeping — it is a month of what the queue
cost and what it ran as — and the card is not. A board that has been used nightly for
six weeks is mostly cards nobody will look at again, and the only tool for that today
destroys the thing worth keeping in order to tidy the thing that is not.

The second half of the problem is disk. A finished task owns a full checkout, and
ADR-0005 puts it under the app data directory where nobody trips over it. Task 016 built
the inventory and the guards to reclaim those, and a policy —
`worktree_auto_cleanup` — that fires on the move to `done`. But `done` is a
statement about review, not about being finished with the directory: the move to `done`
is exactly when a person might still want to look. The moment a person is genuinely
finished with a task is the moment they take it off the board, and that moment does not
exist yet.

Some repositories also need more than "delete the checkout" — a container to stop, a
volume to drop, a scratch database to delete, a cache directory outside the worktree
entirely. Rimaia cannot know what those are. ADR-0005 already anticipated this shape and
filed it as post-MVP: "a per-repository *files to copy* and *setup command* hook,
mirroring what Conductor does." This is that hook, arriving on a different trigger.

## Decision

**A task can be archived: it leaves the board, keeps everything, and can come back. A
repository chooses what one archive cleans up.**

### 1. Archive is a third axis, not a fifth column

`tasks.archived_at TEXT NULL`. `NULL` means on the board; a timestamp means archived, and
it is the timestamp the archive view sorts by.

ADR-0007 refused to make execution state a fifth column and gave the reason: the column
says where a task is in *your* process, and a second question needs a second field. This
is a third question — "is this still on the board at all" — and it is orthogonal to both.
A `done` task and a `not_ready` task can each be archived, and an unarchived task returns
to the column it was always in.

The schema agrees in a way that is worth stating because it is load-bearing:
`board_column`'s `CHECK` is four literals, SQLite cannot widen a `CHECK` without a
rename-copy-drop rebuild, and two tests already pin the string `'archived'` as an
*invalid* column value on purpose — `crates/core/tests/store.rs`'s
`an_unrecognised_board_column_is_refused`, and `src/lib/board.test.ts`'s "drops a card
whose column is none of the four". A fifth column would have to delete both of those
tests to land, which is the schema telling you the answer.

### 2. Archiving preserves everything deleting destroys

The `runs` rows, the transcripts, the links, the dependency edges in both directions.
Nothing about an archive is a cascade. ADR-0022's "a `runs` row is the permanent record
of one attempt" gets its second guarantee here: **`delete_task` remains the only thing in
this product that removes one.**

Two consequences follow, and both are deliberate:

- **Analytics keeps counting an archived task's runs.** What a month cost does not change
  because somebody tidied the board afterwards.
- **A dependency on an archived task still blocks.** ADR-0008 makes a dependency
  satisfied by a *successful run*, not by a human marking anything, and archiving is a
  human marking something. The blocked card keeps naming its blocker by title
  (seam-contract D12's `blocking_title`), so a blocked task never becomes mysteriously
  stuck — the name is there, and the archive view is where you find it.

### 3. Archiving never runs on a live task, and there is no flag for it

`running` and `waiting_retry` refuse. No override, no `force`, no confirmation that makes
it available. This is D20 point 1's unoverridable guard, reached for the same reason:
a Claude Code process is writing in that directory, or is about to be, and archiving is
the trigger for something that may delete it. "Are you sure?" has no answer that improves
a half-deleted checkout.

`queued` is deliberately **allowed**, and archiving is how you take a task out of
tonight's queue without moving it backwards on the board. It works because the scheduler
has no task read of its own — `scheduler::selection::plan` calls `tasks::list_tasks`, so
a task stops being selected the moment it is stamped.

### 4. The on-archive action is per repository, and it is one slot

`repositories.on_archive`: `none` (default) · `remove_worktree` · `script`.

**One slot, three states, mutually exclusive by construction.** The alternative — a
checkbox *and* a script field, both live — would let a user configure Rimaia's guarded
removal to run against a directory their script had already deleted, and would make
"what happens when I archive" a two-field question. It is one field:

- `remove_worktree` is Rimaia's own cleanup, reached through
  `worktree::cleanup::remove_worktree` with `RemovalAuthorization::default()` — every
  force off, the branch always kept, **all four of D20's guards intact**. It is the
  same posture `auto_remove_on_done` runs with, for the same reason: an automatic action
  gets strictly less authority than a human clicking a button, because there is nobody
  present to read the refusal it would otherwise be overriding.
- `script` means **Rimaia touches nothing itself**. The script owns cleanup, including
  the worktree, including the branch. It therefore also gives up every guard in D20
  point 1 — it can delete a directory with uncommitted changes and unpushed commits and
  Rimaia will not have looked. That is the deal, and the Settings copy has to say so in
  those words rather than in a footnote.

There is no global default. A cleanup command is a fact about one repository's
infrastructure, and a global one would either be useless or would run the wrong teardown
against the wrong project.

### 5. A script is a path to an executable, never a command line

CLAUDE.md's rule is "no `sh -c`; build argument vectors — repository paths contain
spaces". Storing a command *line* and splitting it into a vector would satisfy the letter
and break the rule: splitting on whitespace is wrong the first time a path has a space in
it, and splitting correctly means implementing shell quoting, which is `sh -c` with extra
steps and our own bugs.

So the setting holds **one absolute path to one executable file**, validated when it is
written: absolute, exists, is a file, has an executable bit. A user who wants a pipeline
writes it in their own script, with their own `#!` line, where the shell they get is the
shell they chose.

Context arrives as **environment, not arguments** — `RIMAIA_TASK_ID`,
`RIMAIA_TASK_TITLE`, `RIMAIA_REPOSITORY_PATH`, `RIMAIA_BRANCH`, `RIMAIA_WORKTREE_PATH`.
Environment because it is extensible without breaking a positional contract, and because
a script that ignores a variable it does not know about is the normal case, while a
script that mis-reads `$4` is silent corruption.

Two rules carry over from the runner unchanged. Inherited `CLAUDE_*` variables are
stripped — those are process identity, not user configuration, and that rule was never
runner-specific. And the child is spawned in its own **process group**, so a script that
spawns children can be stopped by one signal rather than leaking them.

**The script gets a wall-clock timeout, which is new in this codebase.** Nothing else
here has one: `git` calls are bounded by git, and a run is bounded by ADR-0010's window
and a turn budget instead, deliberately. An archive hook has neither, and an archive that
hangs blocks the queue behind a `git` lock nobody can see. Two minutes, then `TERM` to
the process group, then the runner's existing grace period, then `KILL`.

### 6. The archive commits before the action runs, and the action cannot undo it

Stamp the row, commit, publish, *then* clean up. D20 point 3's argument, applied one
level up: the archive has already happened and been published by the time the action
starts, and a cleanup a guard declined must not be able to report the archive as having
failed.

It differs from `auto_remove_on_done` in exactly one way, and the difference is about who
asked. Automatic cleanup on `done` is silent — it logs a refusal and says nothing,
because the user was moving a card and did not ask about disk. **An archive's action is
reported**, because the user clicked a button whose label said what it would do. The
outcome — nothing, bytes freed, the script's exit code, or a failure — rides back on the
archive's own result and is rendered.

### 7. Bulk archives report; a single archive errors

Taken verbatim from D20 point 2, because it is the same situation. Archiving nine cards
must not be stopped by the tenth being mid-run, and must say which one it was. Archiving
one card is a question about one card, and a refusal *is* the answer.

### 8. Over MCP: yes, operator only

ADR-0021 point 1 makes a Tauri command without a tool a defect. Point 5's standing
exception is about `delete_task`, and it is argued from **irreversibility** — archiving
is reversible, so the exception does not reach it. `archive_task`, `archive_tasks` and
`unarchive_task` ship as tools.

They are `RunAccess::Refused` on point 4's second clause. Archiving is not merely a board
edit: it fires whatever the repository configured, which may be an arbitrary executable,
and a run-scoped agent can reach its own card. Configuring the action
(`set_repository_on_archive`) is "reconfigures the installation" verbatim.

## Consequences

- The board gains a way to stay small that does not cost history. Six weeks of nightly
  runs stop being a board problem.
- **This product now executes a program the user named.** That is a real change in what
  Rimaia is, and it should be read next to ADR-0012 rather than on its own: the same
  machine owner who opts a repository in to `bypassPermissions` is the one who names the
  script, in the same settings pane, with the same trust. It is not reachable by a run,
  and it is not reachable by a remote client — the MCP server is loopback-only and this
  is operator-scope on top of that.
- **A script's guarantees are the script's.** Rimaia's four guards protect the
  `remove_worktree` preset and nothing else. A user who writes a script writes their own
  refusals.
- A seventh migration, which seam-contract D4 requires a task to stop and ask for. The
  ask and the answer are recorded in D4's amendment rather than here.
- `list_tasks` grows a third axis of filtering whose **default is "not archived"**, so
  every existing caller — the scheduler, the plan pass, the MCP tool, the board — excludes
  archived rows without being edited. A default of "everything" would have put archived
  tasks back in the run queue and nobody would have noticed until a night run.
- The first wall-clock timeout in the codebase. It is scoped to this one subprocess and
  it is not a precedent for putting one on a run, which ADR-0010 and the turn budget
  bound on purpose.

## Alternatives considered

- **A fifth `archived` column.** Two tests pin `'archived'` as an invalid column value,
  the `CHECK` cannot be widened in place, and ADR-0007 already rejected the shape when the
  candidate was `in_progress`. It also answers the wrong question: an archived task still
  *has* a column, and unarchiving would have to guess which one.
- **Soft-delete instead of archive** — reuse `delete_task`, add `deleted_at`, hide it.
  Same column, different word, and a much worse one: "deleted" invites a later prune pass
  to make it real, which walks straight back into destroying `runs` rows.
- **A global cleanup script.** One field, less UI. But the thing being cleaned up is a
  repository's infrastructure, and one script that must branch on `$RIMAIA_REPOSITORY_PATH`
  is a per-repository setting the user has to implement themselves, in bash.
- **Both a checkbox and a script, independently.** Rejected in the body: it makes the
  question two-field and lets Rimaia's guarded removal run against something a script has
  already deleted.
- **Store a command line and split it.** Rejected in the body: correct splitting is shell
  quoting, which CLAUDE.md forbids for a reason that starts with repository paths having
  spaces in them.
- **Let the on-archive action fail the archive.** Attractive because it sounds safe — if
  cleanup did not work, do not tidy the card away. But it means a repository with a broken
  script has an unarchivable board, and it puts a subprocess's exit code in charge of a
  database transaction that already committed.
