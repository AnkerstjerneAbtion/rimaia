-- Assignment, revisions and consent to run someone's content on your machine (task 045).
--
-- # The fourth of team mode's board files
--
-- Seam-contract D28 fixes this file's statements for task 045, and they are copied here
-- unchanged; only this header is the task's own. D4's amendment in D28 freezes the file
-- once it lands.
--
-- # Who writes what
--
-- Every column below that carries a revision, an author or the written-during-a-run mark
-- is written by one helper per content kind (`tasks::service` for the plan,
-- `review_loop::config` for a task's review instructions, `db::settings` for a team
-- setting), and a write that stores the same text again bumps nothing. `acceptances` and
-- `trusted_authors` are written only by `consent`, for the context's own actor.
-- `runners.eligibility` and `runner_pool_teams` are written only by
-- `consent::set_runner_eligibility`, for the runner's owner (ADR-0032 points 1 to 3 and 6).
--
-- # What an existing board becomes
--
-- Everything already written was the solo user's, so every author is the solo user, and
-- every revision starts at 1. Nothing is assigned: in a personal team an unassigned task
-- is its owner's own, which the eligibility rule states. `repositories.allow_unattended_runs`
-- is not touched. It already means the team ceiling, and the runner's own consent was
-- copied from it into `checkouts.unattended_consent` by an earlier launch; a backfill here
-- would run before that copy on an install that upgrades across both, and grant consent
-- nobody gave. A board with no solo identity (a server's) has nothing to attribute, and
-- the subqueries below leave its authors NULL.

-- Attribution (ADR-0030 point 8) and assignment (ADR-0032 point 1). SET NULL: a deleted
-- account leaves "a former member", never a deleted task.
ALTER TABLE tasks ADD COLUMN created_by TEXT REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE tasks ADD COLUMN assignee_id TEXT REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE tasks ADD COLUMN assigned_by TEXT REFERENCES users (id) ON DELETE SET NULL;
CREATE INDEX idx_tasks_assignee ON tasks (assignee_id);

-- ADR-0032 point 3's revisions, each with its author and point 6's mark. plan_revision
-- covers plan and extra_instructions together.
ALTER TABLE tasks ADD COLUMN plan_revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE tasks ADD COLUMN plan_updated_by TEXT REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE tasks ADD COLUMN plan_written_during_run BOOLEAN NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN review_instructions_revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE tasks ADD COLUMN review_instructions_updated_by TEXT
    REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE tasks ADD COLUMN review_instructions_written_during_run BOOLEAN NOT NULL DEFAULT 0;

-- The same facts per team setting; consent reads them for base_instructions and
-- review_instructions.
ALTER TABLE team_settings ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE team_settings ADD COLUMN updated_by TEXT REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE team_settings ADD COLUMN updated_at TEXT;
ALTER TABLE team_settings ADD COLUMN written_during_run BOOLEAN NOT NULL DEFAULT 0;

-- Everything that exists was written by the solo user, when there is one. assignee_id
-- is deliberately not backfilled: ADR-0032 point 2's "in a personal team every task is
-- the owner's own" is the eligibility rule's to state, not an assignment nobody made.
UPDATE tasks
   SET created_by      = (SELECT user_id FROM solo_identity),
       plan_updated_by = (SELECT user_id FROM solo_identity),
       review_instructions_updated_by =
           CASE WHEN review_instructions IS NULL THEN NULL
                ELSE (SELECT user_id FROM solo_identity) END;
UPDATE team_settings
   SET updated_by = (SELECT user_id FROM solo_identity)
 WHERE team_id = (SELECT team_id FROM solo_identity);

-- (user, content, revision). task_id is NULL exactly for the two team-wide pieces.
-- revision is the integer revision in decimal for plan and instructions, the review
-- run's id for review_findings, and the commit for base_commit, whose task_id is the
-- dependency that produced it.
CREATE TABLE acceptances (
    id          TEXT NOT NULL PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    team_id     TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    task_id     TEXT REFERENCES tasks (id) ON DELETE CASCADE,
    content     TEXT NOT NULL
                CHECK (content IN ('plan', 'task_review_instructions', 'base_instructions',
                                   'review_instructions', 'review_findings', 'base_commit')),
    revision    TEXT NOT NULL,
    accepted_at TEXT NOT NULL,
    CHECK ((task_id IS NULL) = (content IN ('base_instructions', 'review_instructions')))
);
CREATE UNIQUE INDEX idx_acceptances_task_content
    ON acceptances (user_id, task_id, content, revision) WHERE task_id IS NOT NULL;
CREATE UNIQUE INDEX idx_acceptances_team_content
    ON acceptances (user_id, team_id, content, revision) WHERE task_id IS NULL;
CREATE INDEX idx_acceptances_task ON acceptances (task_id);
CREATE INDEX idx_acceptances_team ON acceptances (team_id);

-- The trust list: personal, per team, off unless a row exists.
CREATE TABLE trusted_authors (
    user_id         TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    team_id         TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    trusted_user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at      TEXT NOT NULL,
    PRIMARY KEY (user_id, team_id, trusted_user_id),
    CHECK (user_id <> trusted_user_id)
);
CREATE INDEX idx_trusted_authors_team ON trusted_authors (team_id);
CREATE INDEX idx_trusted_authors_trusted ON trusted_authors (trusted_user_id);

-- ADR-0032 point 2, on the board so the claim can apply it.
ALTER TABLE runners ADD COLUMN eligibility TEXT NOT NULL DEFAULT 'assigned'
    CHECK (eligibility IN ('assigned', 'assigned_then_pool'));
CREATE TABLE runner_pool_teams (
    runner_id TEXT NOT NULL REFERENCES runners (id) ON DELETE CASCADE,
    team_id   TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    PRIMARY KEY (runner_id, team_id)
);
CREATE INDEX idx_runner_pool_teams_team ON runner_pool_teams (team_id);
