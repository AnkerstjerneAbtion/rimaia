-- The board gets an owner, and the one rebuild that gives it one (task 038).
--
-- Teams, users and runners (ADR-0028, ADR-0029, ADR-0030, ADR-0031), and the
-- solo identity an existing install is adopted under (seam-contract D28).
--
-- # The third of team mode's files, and the only one that rebuilds
--
-- Seam-contract D28 fixes this file's statements column for column in its
-- part 6, with task 034's dated amendment applied to both settings lists. They
-- are copied here unchanged; only this header is the task's own. D4's
-- amendment in D28 freezes the file once it lands: every later task's scratch
-- database has applied it, and sqlx refuses a changed checksum.
--
-- # Why this file rebuilds at all
--
-- Three things SQLite's `ALTER TABLE` cannot do arrive together: a `NOT NULL`
-- `team_id` with no constant default on `repositories` and `tasks`, a
-- table-level foreign key `(repository_id, team_id)` on `tasks`, and three
-- relaxed `NOT NULL`s (`repositories.path`, `repositories.worktree_root`,
-- `runs.log_path`), which a board that other machines read cannot fill. Each
-- needs the new-table, copy, drop, rename procedure. It happens here, once,
-- because this is where `team_id` arrives; every later file in the set is
-- additive precisely so that nothing else has to.
--
-- The composite key goes in now because this is the last chance to put a
-- table-level constraint on `tasks`. It makes the store itself refuse a task
-- in another team than its repository, for the writer that is not the service
-- (the MCP server, or the user with the sqlite3 CLI), as the initial schema's
-- `RESTRICT` already does for a repository with tasks.
--
-- # Why foreign keys must be off, and who turns them off
--
-- With enforcement on, `DROP TABLE tasks` is an implicit `DELETE FROM tasks`,
-- and that fires every `ON DELETE CASCADE`: D28 part 1 measured `runs` and
-- `task_links` coming out empty and the file reporting success. `PRAGMA
-- foreign_keys` does nothing inside a transaction, sqlx 0.8.6 wraps every
-- SQLite migration in one, and it ignores `-- no-transaction` on this driver.
-- So `rimaia_core::db::migrate` turns enforcement off on its connection before
-- the migrator runs, checks `PRAGMA foreign_key_check` after, and turns it back
-- on. This file never touches the pragma, and its first line is its title, not
-- that marker: sqlx reads only the start of a file, so even a mention there
-- would opt it out.
--
-- # Why it guards itself
--
-- Applied any other way (`cargo sqlx migrate run`, whose connection turns
-- foreign keys on, or the sqlite3 CLI), the rebuild would cascade. The first
-- guard refuses unless enforcement is off or `tasks` is empty; an empty `tasks`
-- has nothing to cascade, which is why CLAUDE.md's prepare loop still applies
-- this file to `target/sqlx-prepare.db` unchanged. The second refuses a board
-- that already dangles, because ADR-0003 counts the sqlite3 CLI as a writer and
-- the copy would otherwise have to guess which team a task whose repository is
-- gone belongs to. The third, at the end, repeats the count, so a copy that
-- left anything dangling rolls the whole file back and the next launch retries
-- against the untouched board. A plain SQL file can only fail on a condition
-- through a `CHECK`, so each guard is a temporary table whose named constraint
-- carries the message SQLite reports.
--
-- # Why it never renames the old table
--
-- With `legacy_alter_table` off, SQLite's default, renaming `tasks` to
-- `tasks_old` rewrites the `REFERENCES tasks` in `runs`, `task_links`,
-- `task_dependencies`, `review_findings` and `review_bundles` to name
-- `tasks_old`, and dropping it leaves all of them pointing at nothing. Renaming
-- the *new* table into place rewrites nothing, and every child's `REFERENCES`
-- resolves to it. Each copy names its columns on both sides, never `*`: `ADD
-- COLUMN` appended columns in migration order, and the new tables declare them
-- in another. Every column redeclares its type, default and `CHECK` as the
-- files before this one left it, so no row that was legal can fail the copy.
-- Retired columns stay; ADR-0028 point 5 drops them a release later (065), and
-- the release in between is the rollback window.
--
-- # Why it adopts rather than always creating
--
-- ADR-0029 point 2's solo team and user exist on every desktop install, but
-- the hosted server applies these same files (ADR-0028 point 1). A file that
-- always created them would start every server with a team nobody belongs to.
-- And the app cannot create them first: `Migrator::run` applies every pending
-- file in one call, and the `NOT NULL team_id` copy below needs the team row in
-- this transaction. So the file adopts exactly when the board holds something a
-- team must own (a repository, or any `settings` row other than the one
-- 20260820120100_seed_settings.sql wrote, compared byte for byte), and a fresh
-- board, the server's, the test harness's or a new install's, adopts nothing.
-- `rimaia_core::identity::ensure_solo` creates the same five rows at first
-- launch instead (D28 part 3). Ids are generated in SQL in the shape
-- `Uuid::new_v4()` writes (D10), spelled out at each use because plain SQL
-- cannot define a function, with `random() & 3` because `abs(random())` can
-- overflow.
--
-- `settings` is copied, not moved: user keys to `user_settings`, team keys to
-- `team_settings` by exclusion, so a key nobody listed lands with the team
-- rather than being lost when 065 drops `settings`. Runner keys are task 040's
-- to copy into `runner.db`. Both lists match `db::settings::USER_KEYS` and
-- `RUNNER_KEYS`, and a test drives its expectations from those.

-- 1. Guards (parts 1 and 2).
CREATE TEMP TABLE rebuild_guard (
    ok INTEGER NOT NULL,
    CONSTRAINT "team_mode_board needs foreign_keys OFF: apply it through rimaia_core::db::migrate"
        CHECK (ok = 1)
);
INSERT INTO rebuild_guard (ok)
SELECT foreign_keys = 0 OR NOT EXISTS (SELECT 1 FROM tasks) FROM pragma_foreign_keys;
DROP TABLE rebuild_guard;

CREATE TEMP TABLE dangling_guard (
    violations INTEGER NOT NULL,
    CONSTRAINT "team_mode_board found a dangling reference before it began: run PRAGMA foreign_key_check"
        CHECK (violations = 0)
);
INSERT INTO dangling_guard (violations) SELECT count(*) FROM pragma_foreign_key_check;
DROP TABLE dangling_guard;

-- 2. Who (ADR-0029, ADR-0030).
CREATE TABLE users (
    id                TEXT NOT NULL PRIMARY KEY,
    identity_provider TEXT,   -- 'github' today; no CHECK, ADR-0030 point 1 expects more
    provider_subject  TEXT,   -- the provider's stable id, never the login
    login             TEXT NOT NULL,
    avatar_url        TEXT,
    created_at        TEXT NOT NULL,
    CHECK ((identity_provider IS NULL) = (provider_subject IS NULL))
);
CREATE UNIQUE INDEX idx_users_identity ON users (identity_provider, provider_subject)
    WHERE identity_provider IS NOT NULL;

CREATE TABLE teams (
    id               TEXT NOT NULL PRIMARY KEY,
    name             TEXT NOT NULL,
    personal_user_id TEXT UNIQUE REFERENCES users (id) ON DELETE RESTRICT,
    created_at       TEXT NOT NULL
);

CREATE TABLE team_memberships (
    team_id    TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role       TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    created_at TEXT NOT NULL,
    PRIMARY KEY (team_id, user_id)
);
CREATE INDEX idx_team_memberships_user ON team_memberships (user_id);

CREATE TABLE runners (
    id           TEXT NOT NULL PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    label        TEXT NOT NULL,
    provider     TEXT NOT NULL,  -- ProviderId::as_str(); no CHECK (ADR-0026)
    app_version  TEXT,           -- reported by heartbeat (053), read by the updater (063)
    paired_at    TEXT NOT NULL,
    last_seen_at TEXT,
    unpaired_at  TEXT            -- the row outlives unpairing; runs keep naming it
);
CREATE INDEX idx_runners_user ON runners (user_id);

CREATE TABLE solo_identity (
    singleton  INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
    team_id    TEXT NOT NULL REFERENCES teams (id) ON DELETE RESTRICT,
    user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    runner_id  TEXT NOT NULL REFERENCES runners (id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL
);

CREATE TABLE team_settings (
    team_id TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    key     TEXT NOT NULL,
    value   TEXT NOT NULL,
    PRIMARY KEY (team_id, key)
);

CREATE TABLE user_settings (
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    key     TEXT NOT NULL,
    value   TEXT NOT NULL,
    PRIMARY KEY (user_id, key)
);

-- 3. Adoption (part 3): zero rows on a fresh board, one on an existing one.
CREATE TEMP TABLE solo_adoption AS
SELECT lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4'
         || substr(lower(hex(randomblob(2))), 2) || '-'
         || substr('89ab', 1 + (random() & 3), 1) || substr(lower(hex(randomblob(2))), 2)
         || '-' || lower(hex(randomblob(6))) AS team_id,
       lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4'
         || substr(lower(hex(randomblob(2))), 2) || '-'
         || substr('89ab', 1 + (random() & 3), 1) || substr(lower(hex(randomblob(2))), 2)
         || '-' || lower(hex(randomblob(6))) AS user_id,
       lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4'
         || substr(lower(hex(randomblob(2))), 2) || '-'
         || substr('89ab', 1 + (random() & 3), 1) || substr(lower(hex(randomblob(2))), 2)
         || '-' || lower(hex(randomblob(6))) AS runner_id,
       strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00' AS adopted_at
 WHERE EXISTS (SELECT 1 FROM repositories)
    OR EXISTS (SELECT 1 FROM settings WHERE key <> 'base_instructions')
    OR NOT EXISTS (
           SELECT 1 FROM settings
            WHERE key = 'base_instructions'
              AND value = 'Commit as you work, with focused commits and clear messages.
Run the project''s tests and linters before you finish.
When the work is complete, push the branch and open a pull request describing what changed and why.
If you cannot complete the task, stop, commit what you have, and explain what is blocking you.'
       );

INSERT INTO users (id, login, created_at)
SELECT user_id, 'solo', adopted_at FROM solo_adoption;
INSERT INTO teams (id, name, personal_user_id, created_at)
SELECT team_id, 'Personal', user_id, adopted_at FROM solo_adoption;
INSERT INTO team_memberships (team_id, user_id, role, created_at)
SELECT team_id, user_id, 'owner', adopted_at FROM solo_adoption;
INSERT INTO runners (id, user_id, label, provider, paired_at)
SELECT runner_id, user_id, 'This computer', 'claude-code', adopted_at FROM solo_adoption;
INSERT INTO solo_identity (singleton, team_id, user_id, runner_id, created_at)
SELECT 1, team_id, user_id, runner_id, adopted_at FROM solo_adoption;

INSERT INTO user_settings (user_id, key, value)
SELECT a.user_id, s.key, s.value FROM settings AS s CROSS JOIN solo_adoption AS a
 WHERE s.key IN ('subscription_monthly_usd', 'review_digest_seen_through');
INSERT INTO team_settings (team_id, key, value)
SELECT a.team_id, s.key, s.value FROM settings AS s CROSS JOIN solo_adoption AS a
 WHERE s.key NOT IN ('subscription_monthly_usd', 'review_digest_seen_through',
                     'run_environment', 'mcp_port', 'max_concurrency', 'schedule_mode',
                     'queue_state', 'active_run_window', 'usage_limit_pause_until',
                     'worktree_auto_cleanup', 'doctor_dismissals', 'onboarding_dismissed');

-- 4. repositories. allow_unattended_runs keeps its name and now means the team ceiling
-- (ADR-0032 point 4); task 041 copies it into runner.db as the runner's consent. path,
-- worktree_root, max_concurrency, credential_*, on_archive and on_archive_script are
-- retired: read until 066, dropped by 065.
CREATE TABLE repositories_new (
    id                    TEXT NOT NULL PRIMARY KEY,
    team_id               TEXT NOT NULL REFERENCES teams (id) ON DELETE RESTRICT,
    name                  TEXT NOT NULL,
    path                  TEXT,
    default_branch        TEXT NOT NULL,
    worktree_root         TEXT,
    allow_unattended_runs BOOLEAN NOT NULL DEFAULT 0,
    created_at            TEXT NOT NULL,
    max_concurrency       INTEGER NOT NULL DEFAULT 1,
    credential_login      TEXT,
    credential_label      TEXT,
    credential_added_at   TEXT,
    on_archive            TEXT NOT NULL DEFAULT 'none'
                          CHECK (on_archive IN ('none', 'remove_worktree', 'script')),
    on_archive_script     TEXT,
    review_config         TEXT
);
INSERT INTO repositories_new (
    id, team_id, name, path, default_branch, worktree_root, allow_unattended_runs,
    created_at, max_concurrency, credential_login, credential_label, credential_added_at,
    on_archive, on_archive_script, review_config)
SELECT
    id, (SELECT team_id FROM solo_adoption), name, path, default_branch, worktree_root,
    allow_unattended_runs, created_at, max_concurrency, credential_login,
    credential_label, credential_added_at, on_archive, on_archive_script, review_config
  FROM repositories;
DROP TABLE repositories;
ALTER TABLE repositories_new RENAME TO repositories;
CREATE INDEX idx_repositories_team ON repositories (team_id);
CREATE UNIQUE INDEX idx_repositories_id_team ON repositories (id, team_id);

-- 5. tasks. The repository reference becomes (repository_id, team_id), so the store
-- itself refuses a task in another team than its repository (ADR-0029 point 5), and a
-- repository changing team under its tasks. worktree_path is retired (066, 065).
CREATE TABLE tasks_new (
    id                  TEXT NOT NULL PRIMARY KEY,
    team_id             TEXT NOT NULL REFERENCES teams (id) ON DELETE RESTRICT,
    repository_id       TEXT NOT NULL,
    title               TEXT NOT NULL,
    plan                TEXT,
    extra_instructions  TEXT,
    board_column        TEXT NOT NULL
                        CHECK (board_column IN ('not_ready', 'ready', 'in_review', 'done')),
    position            REAL NOT NULL,
    run_state           TEXT NOT NULL
                        CHECK (run_state IN ('idle', 'queued', 'running', 'blocked',
                                             'waiting_retry', 'failed', 'cancelled')),
    branch              TEXT,
    worktree_path       TEXT,
    strategy_mode       TEXT NOT NULL DEFAULT 'default'
                        CHECK (strategy_mode IN ('default', 'manual', 'planned')),
    model               TEXT,
    effort              TEXT,
    strategy_plan       TEXT,
    strategy_source     TEXT
                        CHECK (strategy_source IS NULL
                               OR strategy_source IN ('user', 'planner')),
    strategy_updated_at TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    source              TEXT NOT NULL DEFAULT 'ui' CHECK (source IN ('ui', 'mcp', 'system')),
    archived_at         TEXT,
    review_instructions TEXT,
    review_config       TEXT,
    FOREIGN KEY (repository_id, team_id) REFERENCES repositories (id, team_id)
        ON DELETE RESTRICT
);
INSERT INTO tasks_new (
    id, team_id, repository_id, title, plan, extra_instructions, board_column, position,
    run_state, branch, worktree_path, strategy_mode, model, effort, strategy_plan,
    strategy_source, strategy_updated_at, created_at, updated_at, source, archived_at,
    review_instructions, review_config)
SELECT
    id, (SELECT team_id FROM solo_adoption), repository_id, title, plan,
    extra_instructions, board_column, position, run_state, branch, worktree_path,
    strategy_mode, model, effort, strategy_plan, strategy_source, strategy_updated_at,
    created_at, updated_at, source, archived_at, review_instructions, review_config
  FROM tasks;
DROP TABLE tasks;
ALTER TABLE tasks_new RENAME TO tasks;
CREATE INDEX idx_tasks_board ON tasks (repository_id, board_column, position);
CREATE INDEX idx_tasks_run_state ON tasks (run_state);
CREATE INDEX idx_tasks_team ON tasks (team_id, board_column, position);

-- 6. runs. log_path is relaxed and retired (ADR-0028 point 2; 056 replaces it, 065
-- drops it). Every existing run ran on this machine, so it names the solo runner.
CREATE TABLE runs_new (
    id                    TEXT NOT NULL PRIMARY KEY,
    task_id               TEXT NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    attempt               INTEGER NOT NULL,
    status                TEXT NOT NULL
                          CHECK (status IN ('running', 'succeeded', 'failed',
                                            'cancelled', 'interrupted')),
    session_id            TEXT NOT NULL,
    prompt                TEXT NOT NULL,
    started_at            TEXT NOT NULL,
    ended_at              TEXT,
    exit_class            TEXT
                          CHECK (exit_class IS NULL
                                 OR exit_class IN ('success', 'usage_limit', 'transient',
                                                   'interrupted', 'fatal', 'cancelled')),
    error_message         TEXT,
    num_turns             INTEGER,
    cost_usd              REAL,
    log_path              TEXT,
    pr_url                TEXT,
    resume_after          TEXT,
    base_ref              TEXT,
    model                 TEXT,
    effort                TEXT,
    run_environment       TEXT,
    input_tokens          INTEGER,
    output_tokens         INTEGER,
    cache_read_tokens     INTEGER,
    cache_creation_tokens INTEGER,
    head_sha              TEXT,
    base_sha              TEXT,
    kind                  TEXT NOT NULL DEFAULT 'implementation'
                          CHECK (kind IN ('implementation', 'review', 'fix')),
    findings_recorded_at  TEXT,
    runner_id             TEXT REFERENCES runners (id) ON DELETE SET NULL
);
INSERT INTO runs_new (
    id, task_id, attempt, status, session_id, prompt, started_at, ended_at, exit_class,
    error_message, num_turns, cost_usd, log_path, pr_url, resume_after, base_ref, model,
    effort, run_environment, input_tokens, output_tokens, cache_read_tokens,
    cache_creation_tokens, head_sha, base_sha, kind, findings_recorded_at, runner_id)
SELECT
    id, task_id, attempt, status, session_id, prompt, started_at, ended_at, exit_class,
    error_message, num_turns, cost_usd, log_path, pr_url, resume_after, base_ref, model,
    effort, run_environment, input_tokens, output_tokens, cache_read_tokens,
    cache_creation_tokens, head_sha, base_sha, kind, findings_recorded_at,
    (SELECT runner_id FROM solo_adoption)
  FROM runs;
DROP TABLE runs;
ALTER TABLE runs_new RENAME TO runs;
CREATE UNIQUE INDEX idx_runs_task_attempt ON runs (task_id, attempt);
CREATE INDEX idx_runs_task_kind ON runs (task_id, kind, attempt);
CREATE INDEX idx_runs_runner ON runs (runner_id);

-- 7. The copy left nothing dangling, or the whole file rolls back.
CREATE TEMP TABLE copy_guard (
    violations INTEGER NOT NULL,
    CONSTRAINT "team_mode_board left a dangling reference: run PRAGMA foreign_key_check"
        CHECK (violations = 0)
);
INSERT INTO copy_guard (violations) SELECT count(*) FROM pragma_foreign_key_check;
DROP TABLE copy_guard;
DROP TABLE solo_adoption;
