---
id: "029"
title: A development database that is not the user's
milestone: v0.3
status: ready
landed: "#29"
depends_on: ["018"]
adrs: ["0003", "0023"]
size: S
---

# A development database that is not the user's

## Goal

`RIMAIA_DATA_DIR` points a launch at a data directory of its own. Unset, nothing changes.
Set to an absolute path, the database, worktrees, run transcripts and logs all live under it,
and the doctor says so.

## Why now

On 2026-09-11 every branch of this repository, `main` included, refused to start:

```
migration 20260904120000 was previously applied but is missing in the resolved migrations
```

Task 022's unmerged branch had run the app once, and `sqlx` wrote that migration into the one
database every worktree shares. Repair meant dropping three columns and deleting a
`_sqlx_migrations` row out of the operator's real database by hand; it was safe only because
those columns happened to be NULL.

ADR-0023 has the full argument. The short version is that worktree-per-task is ADR-0005's
whole design, unmerged migrations are therefore normal, and a single shared schema underneath
them is a collision with a date on it rather than a risk. It will happen again the next time
any branch with a pending migration is run — task 022 itself is still unmerged, so the next
occurrence is already queued.

Two nearby tasks make this the moment. Task 018 shipped `checks::data_directory`, which is
where "which directory am I actually using" belongs and currently does not appear. Task 025
will put startup failures in front of a user who double-clicked a bundle — and the failure it
was written for is precisely this one, which is worth fixing at the cause as well as at the
presentation.

## Scope

**The resolution rule lives in `rimaia-core` and is pure.** Add to `crates/core/src/paths.rs`
a function taking the override candidate and the platform fallback and returning the directory
or a `RimaiaError`. The shell reads the environment and asks Tauri for the fallback; core
decides between them. ADR-0015 is the reason — the rule is then covered by
`cargo test -p rimaia-core` with no Tauri app in the picture, and `AppPaths::new` keeps taking
a directory it does not have to discover.

**The shell wires it in the setup hook, before `AppPaths::new`.** `src-tauri/src/lib.rs`
already resolves `app.path().app_data_dir()?` there; the override is read alongside it and the
two are handed to the new function. `paths.create_all()` is already idempotent and already
runs next, so a directory that does not exist yet is not a special case.

**A relative path is a refusal.** So is a value starting with `~`. Both fail at startup naming
the variable and the value, and nothing is created first — a refusal that has already made a
directory is worse than the mistake it was reporting.

This one failure does **not** go through `log_startup_failure`, which is what this task
originally assumed. Both of that helper's outputs are unavailable this early: there is no
`db_file` to name, since it is derived from the directory that just failed to resolve, and
`logging::init` has not run, so a `tracing` call has no subscriber and is dropped rather than
written. Propagating the error is the whole of the report — D11's stderr half — which is why
its text has to name the variable and the value itself. Found by running the binary; a helper
written for it first looked correct and logged nothing.

**The doctor's `data_directory` row reports the path it probed, and whether the value came
from the environment.** One line of detail on a check that already exists. This is what keeps
the override from being a silent relocation, so it is in scope rather than deferred.

**`CLAUDE.md` gains the habit.** One line under Commands or Gotchas: run the app from a
worktree with `RIMAIA_DATA_DIR` set somewhere scratch, and why — a branch carrying an unmerged
migration will otherwise write it into the database every other branch reads.

## Out of scope

- **Any Settings control, picker or in-app switcher.** ADR-0023 is explicit that this is a
  development affordance. An end-user profile feature needs a UI, a way to move data between
  profiles and a story about which one a scheduled run uses; none of that is this.
- **Changing the default.** With the variable unset the resolved path is byte-for-byte what it
  is today. A test asserts this rather than trusting it.
- **Making the migrator tolerant of unknown applied migrations.** Argued and rejected in
  ADR-0023 — it trades one clear startup refusal for silent schema drift against the `.sqlx/`
  cache.
- **Repairing an already-diverged database.** The one on this machine was repaired by hand on
  2026-09-11. A general "your database is ahead of this build" recovery tool is a real idea and
  a different task; note it in the backlog if it keeps coming up.
- **Per-worktree automatic derivation.** Also argued and rejected in ADR-0023.

## Acceptance criteria

- With no override, the resolver returns the platform fallback unchanged — asserted by a test
  on the pure function, not by launching the app.
- With an absolute override, `db_file`, `worktrees_dir`, `runs_dir` and `logs_dir` all resolve
  underneath it, and `create_all` produces them.
- A relative override, and one beginning with `~`, each fail with a `RimaiaError` naming the
  variable, and neither creates any directory. Both cases are tested.
- A directory containing spaces survives intact — `paths.rs` already has this test for the
  fallback and it must hold for the override.
- The doctor's `data_directory` result carries the resolved path and reports whether it was
  inherited from the environment.
- Two worktrees on branches with *different* migration sets, each launched with its own
  `RIMAIA_DATA_DIR`, both start. This is the criterion the task exists for; it can be checked
  by hand and the PR says so.
- `CLAUDE.md` documents the variable and the reason.
- No migration, no schema change, no change to any query — so no `.sqlx/` regeneration.

## Notes

**The tempting smaller version is to read the variable straight into `AppPaths::new` in the
shell and skip core entirely.** It is four lines and it works. It also puts the one piece of
logic with real edge cases — relative, tilde, empty-string, non-UTF-8 — in the one crate the
test strategy cannot reach without a Tauri app, which is how it ends up with no tests at all.
The pure function is barely larger and is the difference between a rule and a habit.

**An empty-string value deserves a deliberate answer.** `RIMAIA_DATA_DIR=` is what an unset
variable looks like to a half-written shell script. Treating it as "unset" is defensible;
treating it as a relative path and refusing is also defensible. Pick one, and put the reason in
the code rather than leaving the next reader to infer it from behaviour.
