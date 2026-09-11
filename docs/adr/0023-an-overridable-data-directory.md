# 23. An overridable data directory, so one branch cannot break every other

- **Status:** Accepted
- **Date:** 2026-09-11

## Context

ADR-0003 puts the database in the platform application data directory, one file per machine.
That is right for a shipped app: a user has one Rimaia, and migrations only ever run forward.

It is wrong for developing Rimaia, and the failure is not hypothetical. It happened on
2026-09-11.

The directory is derived from the bundle identifier — `app.path().app_data_dir()` in
`src-tauri/src/lib.rs` — so every git worktree of this repository resolves the *same* path.
Task 022's branch added migration `20260904120000`, ran the app once, and `sqlx` recorded it
in the shared `_sqlx_migrations` table. From that moment every branch without that file,
including `main`, aborted at startup:

```
migration 20260904120000 was previously applied but is missing in the resolved migrations
```

Three things make that worse than an inconvenience.

**ADR-0005 makes it the normal case.** One worktree per task is the product's core workflow,
not a habit we could drop. Branches carrying unmerged migrations are the working pattern here,
and the backlog has more than one of them pending at any time. A shared mutable schema under a
worktree-per-task workflow is a collision waiting for a date.

**The failure mode is the worst one available.** D11 deliberately chose process exit over a
modal, and task 025 has not landed, so what a developer actually sees is a non-unwinding Rust
panic and no window — and what a double-clicked bundle shows is nothing at all. The error
message is accurate and nobody is looking at the stream it is written to.

**Repair costs real data.** Recovery was: back up the file, drop the three columns the
migration added, delete the `_sqlx_migrations` row. That was safe only because the columns
happened to be entirely NULL. A branch whose migration backfills or drops something would push
a half-finished experiment into every other branch's database, with no honest way back — the
migration is unmerged, so there is nothing to roll forward to either.

There is a milder, constant version of the same problem underneath the dramatic one:
developing against the database the operator actually queues work in means every test task,
cancelled run and mis-drag lands in real history.

## Decision

**The shell resolves the data directory from `RIMAIA_DATA_DIR` when that variable is set, and
from Tauri's application data directory when it is not.**

Five things settle what that means.

1. **It is read once, in the setup hook, before `AppPaths::new`.** Core never learns the
   variable exists. `AppPaths::new` already takes the directory as an argument precisely so the
   OS-specific lookup stays in the shell (ADR-0015), and this decision does not move that line
   — it changes what the shell hands in, nothing else.

2. **The decision of *what* the path is, is pure, and lives in core.** A
   `resolve_data_dir(override: Option<&OsStr>, fallback: PathBuf)` in `crates/core/src/paths.rs`
   takes both candidates and returns the answer or an error. The shell reads the environment and
   asks Tauri; core decides. That keeps the rule under `cargo test -p rimaia-core` rather than
   behind a Tauri app instance, which is the whole reason for the crate split.

3. **A relative path is an error, not a guess.** Resolved against the current directory it
   would mean one thing under `npm run tauri dev` and another under a double-clicked bundle,
   and the failure would be a *second* empty database rather than a message. `~` is not
   expanded either — the shell that set the variable is the thing that expands tildes, and a
   literal `~` directory appearing in a home folder is a worse outcome than a refusal.

4. **The doctor reports the live path and where it came from.** `checks::data_directory`
   already probes the directory for writability; it gains the path it probed and whether the
   value was inherited from the environment. Without this the override is a silent relocation,
   which is the failure the whole entry is trying to stop.

5. **No default changes.** An installed Rimaia with no variable set behaves exactly as ADR-0003
   describes. This entry adds a door; it does not move the house.

### Why a variable rather than the two obvious alternatives

**Rather than a debug-build identifier** (`com.rimaia.app.dev` under `cfg!(debug_assertions)`):
it separates development from the installed app but not worktree from worktree, and
worktree-from-worktree is the collision that actually occurred. Two debug builds on two
branches still share one directory, so the bug survives the fix. It also relocates every
existing developer's database silently at the moment they pull, which is a surprising thing for
a build profile to decide.

**Rather than deriving the directory from the worktree path:** correct for every collision, and
unexplainable. A database that changes because you are in a different directory produces "where
did my tasks go" at exactly the moment someone is trying to reproduce a bug, and it offers no
way to deliberately *ask* for the shared one.

**Rather than making the migrator tolerant of unknown applied migrations:** rejected, and worth
saying why, because ADR-0004's "tolerant parsing of CLI output" looks like a precedent and is
not one. An unknown CLI event can be persisted and ignored because nothing depends on its
shape. An unknown *migration* means the tables are not the shape the queries were compiled
against, and `SQLX_OFFLINE` compiles those queries against the checked-in `.sqlx/` cache — so
the mismatch would resurface as a runtime failure on some arbitrary query later, instead of one
clear refusal at startup. Turning a loud correct error into silent schema drift is a worse
trade than any amount of inconvenience.

An environment variable is worse than both alternatives at being automatic and better than both
at being **legible**: the value is visible, the default is unchanged, and "point this run at a
scratch database" is one word on a command line. Its honest cost is that it is opt-in — an
unset variable is precisely today's behaviour — so this entry does not by itself prevent the
collision. It makes prevention possible, cheap and explicable; `CLAUDE.md` makes it routine.

## Consequences

- Migration divergence between branches stops being shared-state corruption and becomes a local
  matter. A branch with an unmerged migration can be run, reviewed and thrown away without
  touching anything else.
- `CLAUDE.md` gains the one line that makes this a habit: run the app from a worktree with
  `RIMAIA_DATA_DIR` pointed somewhere scratch.
- A developer who wants their real board inside a dev build copies the file across once,
  deliberately, and knows they have done it.
- `RIMAIA_LOG` already exists in `src-tauri/src/logging.rs`, so `RIMAIA_*` is an established
  prefix rather than a new convention.
- **This is a development affordance, not an end-user feature.** It is documented in the
  repository, not in the app, and gets no Settings control. If a genuine end-user need for
  several profiles ever appears it will want a picker, a migration story and a way to move data
  between them — a different decision with a UI attached, and this is not it.
- ADR-0003's Decision section still illustrated the path with `dev.rimaia.app`, a stale
  identifier its own 2026-08-20 amendment had already corrected to `com.rimaia.app` — and that
  amendment's point stands: the path was an illustration, never a string Rimaia formats.
  Brought in line in place, as a factual correction rather than a change of decision. This
  entry changes what the shell resolves, which is the same seam the amendment describes.
