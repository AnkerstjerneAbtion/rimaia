-- What a run left behind, recorded as data rather than as a place to go and
-- look (ADR-0033 points 4, 5 and 7, ADR-0036, task 033).
--
-- # The first of team mode's files
--
-- Seam-contract D28 names thirteen files before any of them is written, and
-- fixes this one's statements column for column in its part 6. They are copied
-- here unchanged; only this header is the task's own. D4's amendment in D28
-- freezes the file once it lands, and task 038's rebuild of `runs` copies both
-- columns and redeclares `review_bundles` exactly as written below.
--
-- # Why a run has to record this at all
--
-- Until now the run detail view asked git, every time it opened, what the
-- task's branch contained. That answers about the branch as it is now rather
-- than as the attempt left it, and it fails outright once the worktree, the
-- branch or the clone has moved. A board that other machines read cannot run
-- git at all (ADR-0033 point 7). So the runner measures the branch once, at the
-- finish, while the worktree is guaranteed to still be there, and the read
-- returns what it measured.
--
-- # History cannot be backfilled
--
-- ADR-0022's argument, again: every row written before this file has NULL in
-- both columns and no bundle, and that is the truth about it (D18). A bundle
-- computed now for an old run would describe today's branch and be labelled as
-- the past.

-- The commit the worktree's HEAD was on when the run ended, and the commit it started
-- from (ADR-0033 points 4, 5 and 7). NULL is "not recorded" (D18). base_ref keeps the
-- branch name it has always held; base_sha is what that name resolved to.
ALTER TABLE runs ADD COLUMN head_sha TEXT;
ALTER TABLE runs ADD COLUMN base_sha TEXT;

-- ADR-0033 point 7. A table of its own so the patch never rides a board read; the PR URL
-- stays on runs.pr_url and both commits on runs. files and commits are JSON arrays of
-- task 033's serde types, read and written only through them.
CREATE TABLE review_bundles (
    run_id          TEXT NOT NULL PRIMARY KEY REFERENCES runs (id) ON DELETE CASCADE,
    files_changed   INTEGER NOT NULL,
    insertions      INTEGER NOT NULL,
    deletions       INTEGER NOT NULL,
    files           TEXT NOT NULL,
    commits         TEXT NOT NULL,
    patch           TEXT,                        -- up to the cap; NULL once pruned
    patch_bytes     INTEGER NOT NULL,            -- the whole patch, before the cap
    patch_truncated BOOLEAN NOT NULL DEFAULT 0,
    patch_pruned_at TEXT,                        -- ADR-0036 point 6
    created_at      TEXT NOT NULL
);
