-- What one machine keeps about the board's work: its clones, its worktrees, its leases and
-- its schedules (ADR-0028 point 2, ADR-0033 points 2 and 3, ADR-0031 points 5 and 6, task 041).
--
-- # The whole file lands at once
--
-- Seam-contract D28 part 6 fixes these statements column for column; they are copied here
-- unchanged, and only this header is the task's own. D4's amendment in D28 freezes the file
-- when task 041 lands, which is why held_leases arrives here although nothing writes it until
-- task 043, and why task 066 adds no file of its own for the checkout readers it switches.
--
-- # Keyed by the board's ids, with no foreign key into the board
--
-- checkouts.repository_id and worktrees.task_id are the board's ids (ADR-0033 point 2), but
-- the board is another file in solo and another machine in team mode, so nothing below can
-- reference it. The one foreign key is internal: a worktree belongs to a checkout on this
-- machine, and a checkout that still has worktrees cannot be removed.
--
-- # Filled once, by Rust
--
-- The adoption step named `machine_state` copies every existing install's values out of
-- rimaia.db: each repository with a clone path becomes a checkout, allow_unattended_runs
-- arriving as unattended_consent, every tasks.worktree_path becomes a worktree, and
-- schedules is copied whole. A migration cannot know where the board file is, and a headless
-- runner has none, so this file creates the tables and copies nothing.

-- The per-repository runner half of ADR-0028 point 2, keyed by the board's repository
-- id (ADR-0033 point 2). unattended_consent is ADR-0032 point 4's runner consent.
CREATE TABLE checkouts (
    repository_id       TEXT NOT NULL PRIMARY KEY,
    path                TEXT NOT NULL,
    worktree_root       TEXT NOT NULL,
    max_concurrency     INTEGER NOT NULL DEFAULT 1,
    unattended_consent  BOOLEAN NOT NULL DEFAULT 0,
    on_archive          TEXT NOT NULL DEFAULT 'none'
                        CHECK (on_archive IN ('none', 'remove_worktree', 'script')),
    on_archive_script   TEXT,
    credential_login    TEXT,
    credential_label    TEXT,
    credential_added_at TEXT,
    created_at          TEXT NOT NULL
);

-- ADR-0033 point 3. fenced_at is ADR-0031 point 4's fence after "run elsewhere" (task
-- 057): the worktree is kept and never pushed from.
CREATE TABLE worktrees (
    task_id       TEXT NOT NULL PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES checkouts (repository_id) ON DELETE RESTRICT,
    path          TEXT NOT NULL,
    fenced_at     TEXT
);
CREATE INDEX idx_worktrees_repository ON worktrees (repository_id);

-- The leases this runner holds, so startup reconciles only its own (ADR-0031 point 5)
-- and a heartbeat can name each one as D31's LeaseRef, team included. Written from
-- task 043.
CREATE TABLE held_leases (
    task_id     TEXT NOT NULL PRIMARY KEY,
    team_id     TEXT NOT NULL,
    purpose     TEXT NOT NULL
                CHECK (purpose IN ('implementation', 'strategy', 'review', 'fix')),
    run_id      TEXT,
    generation  INTEGER NOT NULL,
    acquired_at TEXT NOT NULL
);

-- Task 013's table, moved whole (ADR-0031 point 6).
CREATE TABLE schedules (
    id              TEXT NOT NULL PRIMARY KEY,
    name            TEXT NOT NULL,
    mode            TEXT NOT NULL CHECK (mode IN ('sequential', 'parallel')),
    cron            TEXT,
    start_at        TEXT,
    max_concurrency INTEGER NOT NULL DEFAULT 2,
    enabled         BOOLEAN NOT NULL DEFAULT 1,
    timezone        TEXT,
    stop_at         TEXT,
    last_fired_at   TEXT,
    armed_at        TEXT
);
