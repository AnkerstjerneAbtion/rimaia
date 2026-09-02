-- The v0.2 batch: four tasks' columns, one file (seam-contract D4's 2026-09-02
-- amendment).
--
-- # Why four tasks share one migration
--
-- D4 exists because two agents in two worktrees each reaching for "the next
-- timestamp" collide on one, silently, and append-only (ADR-0003) means a
-- renumber afterwards is not available as a repair. The remedy D4 chose is a
-- named list, and the way to stay inside it with four tasks landing together is
-- one named file rather than four unnamed ones. Every column below is inert
-- until the task that owns it lands -- the same bet the initial schema already
-- made with `task_dependencies` and `schedules`, and for the same reason.
--
-- # The retired name
--
-- D4's 2026-09-01 amendment reserved `20260828120000_dependencies_and_parallel`
-- for tasks 011 and 012 and asserted the backfill was "timestamped after the
-- name reserved for tasks 011 and 012 ... so the two cannot collide". The
-- backfill shipped and the reservation did not, which makes that sentence false
-- rather than merely unlucky: `20260828120000` now sorts *before* an
-- already-applied migration.
--
-- sqlx would not have objected. `Migrator::run` iterates the source in version
-- order and applies whatever `_sqlx_migrations` does not already list;
-- `validate_applied_migrations` errors only on the reverse case, an applied
-- version with no file (sqlx-core 0.8.6, src/migrate/migrator.rs). So the
-- reserved name would have applied cleanly -- fourth on a fresh install, fifth
-- on every existing one. That divergence is the objection, not a failure: this
-- database is meant to be legible to any SQLite tool (ADR-0003), and a
-- `_sqlx_migrations` table whose version order does not describe the order the
-- schema was actually built in stops being legible in exactly the way D4's body
-- is about. `cargo sqlx migrate run --target-version` would also refuse
-- outright, with `VersionTooOld`.
--
-- Nothing was ever written under the retired name, so retiring it costs nothing
-- and serves the collision-avoidance purpose the reservation had.
--
-- # Why every column here is nullable, and none carries a CHECK
--
-- The initial schema's header states the asymmetry: an index or a unique
-- constraint can be added later, a CHECK cannot, so a domain a later task still
-- owns is left unconstrained rather than guessed at. `schedules` already made
-- that choice explicitly for `cron`/`start_at` ("which combinations are legal is
-- task 013's design and is deliberately not a CHECK"), and the four columns
-- added to it below are the rest of the same design.
--
-- `repositories.max_concurrency` is the one exception to nullable, because
-- ADR-0010 fixes its default rather than leaving it open: per-repository
-- concurrency is capped at 1 even in parallel mode unless the user opts out.
-- NOT NULL DEFAULT 1 is that rule in the schema, and SQLite's ADD COLUMN
-- backfills it in O(1) without writing a row.

-- Task 011, ADR-0008. The branch this attempt was actually created from: the
-- repository's default branch, or the branch of the highest-position dependency
-- when the task has any.
--
-- On the run rather than on the task, because it is a fact about one attempt.
-- A task's dependencies can change between attempts, and a morning review asking
-- "what was this built on top of" is asking about the attempt it is reading, not
-- about what the resolver would answer today.
ALTER TABLE runs ADD COLUMN base_ref TEXT;

-- Task 012, ADR-0010: "Per-repository concurrency is capped at 1 even in
-- parallel mode unless the user explicitly opts out. Two agents in two worktrees
-- of the same repo is safe for git, but they will fight over ports, test
-- databases, and lockfiles."
--
-- So the opt-out is per repository and the default is the cap, not the global
-- `max_concurrency` -- which lives in `settings` (seam-contract D3's shape,
-- owned by `scheduler::capacity`) because ADR-0010 calls mode and concurrency
-- properties of the run configuration rather than of a repository.
ALTER TABLE repositories ADD COLUMN max_concurrency INTEGER NOT NULL DEFAULT 1;

-- Task 013, ADR-0010: "Recurring -- a cron expression **with a timezone**".
--
-- An IANA name ('Europe/Copenhagen'), nullable in the schema and required by the
-- service for every row it writes. A NOT NULL DEFAULT 'UTC' would let a nightly
-- schedule be created silently in the wrong zone, which is precisely the failure
-- the DST acceptance criterion exists to catch: a wrong answer that looks like
-- an answer is worse here than a refusal.
ALTER TABLE schedules ADD COLUMN timezone TEXT;

-- Task 013. A **local wall-clock time of day**, 'HH:MM', resolved through the
-- schedule's own `timezone` -- not an instant.
--
-- "Stop at 06:00" is the sentence the user says, and a recurring window needs a
-- repeating stop, which an absolute instant cannot express. A duration column
-- would express it but would move the stop time whenever the start moved, and
-- would make a window crossing spring-forward end at the wrong hour. Resolving a
-- local time through the zone means such a window is seven real hours and still
-- ends at 06:00 local, which is what was set.
ALTER TABLE schedules ADD COLUMN stop_at TEXT;

-- Task 013. When the schedule **actually** fired, never when it was due.
--
-- This is what makes ADR-0010's "the scheduler fires late rather than skipping"
-- work without becoming a re-fire loop: the machine was asleep, the occurrence
-- is missed, one fire happens on wake, and this column is what stops the next
-- pass firing it again a second later.
ALTER TABLE schedules ADD COLUMN last_fired_at TEXT;

-- Task 013. The instant from which missed occurrences count -- set on create and
-- re-set on every enable.
--
-- Without it, a nightly 22:00 schedule created at 23:00 fires immediately for
-- the occurrence it "missed" an hour before it existed, and a schedule disabled
-- for a month fires the second it is re-enabled. The recurring search's baseline
-- is max(last_fired_at, armed_at).
ALTER TABLE schedules ADD COLUMN armed_at TEXT;

-- Task 024's capture half, ADR-0022: seven nullable columns on `runs`, written
-- once by `finish_run` and never updated.
--
-- These are here now, ahead of the page that reads them, because ADR-0022's
-- forcing detail is that **history cannot be backfilled**. The model a run used
-- and the tokens it spent exist only in the moment the run ends; every night the
-- queue runs without these columns is a night permanently missing from any chart
-- drawn later. Deferring the capture until the page exists does not delay the
-- cost, it decides that the first N months are blank.
--
-- **NULL means "not recorded", never zero.** Every row written before this
-- migration honestly has none, and a run that dies before its `result` event
-- never learns them. A view that averages a NULL as zero is lying about the
-- past; seam-contract D18 states the rule and binds the tasks that read these.
--
-- `model`, `effort` and `run_environment` cannot be derived later because each
-- has a present tense that moves: `tasks.model` is rewritten by a planner or a
-- human (ADR-0016), and `run_environment` was a setting when the run started and
-- settings change. The four token counts come off the terminal `result` event's
-- `usage` object and exist nowhere else once the transcript is pruned -- which
-- task 015 is designed to do, and which ADR-0022 part 2 permits precisely
-- because the row survives it.
ALTER TABLE runs ADD COLUMN model TEXT;
ALTER TABLE runs ADD COLUMN effort TEXT;
ALTER TABLE runs ADD COLUMN run_environment TEXT;
ALTER TABLE runs ADD COLUMN input_tokens INTEGER;
ALTER TABLE runs ADD COLUMN output_tokens INTEGER;
ALTER TABLE runs ADD COLUMN cache_read_tokens INTEGER;
ALTER TABLE runs ADD COLUMN cache_creation_tokens INTEGER;
