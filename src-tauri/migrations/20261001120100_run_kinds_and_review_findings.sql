-- Runs have a kind, and what a reviewer found has somewhere to live
-- (ADR-0017, seam-contract D29 and D30, task 035).
--
-- # The second of team mode's files
--
-- Seam-contract D28 fixes this file's statements column for column in its
-- part 6, as amended on 2026-09-30. They are copied here unchanged; only this
-- header is the task's own. D4's amendment in D28 freezes the file once it
-- lands, and task 038's rebuild of `runs` redeclares `kind`,
-- `findings_recorded_at` and `idx_runs_task_kind` exactly as written below.
--
-- # Three columns this task writes and does not read
--
-- `tasks.review_instructions`, `tasks.review_config` and
-- `repositories.review_config` are task 021's. That task has no file of its
-- own, so its columns ride in this one. Nothing reads them until 021 lands; a
-- NULL in any of them is "nothing set", which is the truth about every row
-- written before then.
--
-- # Every existing row was an implementation attempt
--
-- `kind`'s DEFAULT is there for the rows already on disk, and for nothing
-- else: before this file a run could only be an implementation attempt, so the
-- backfill is exact rather than a guess. Production code never relies on it.
-- `start_run` names the kind in its INSERT, so a review started by a caller
-- that forgot to say so cannot be recorded as an implementation by default
-- (D29 point 1). 'strategy' is missing from the CHECK on purpose: the planner
-- writes no row (D17.5).

ALTER TABLE runs ADD COLUMN kind TEXT NOT NULL DEFAULT 'implementation'
    CHECK (kind IN ('implementation', 'review', 'fix'));

-- One attempt sequence per task across kinds: idx_runs_task_attempt stays UNIQUE on
-- (task_id, attempt), and start_run keeps computing max(attempt) + 1. What each reader
-- does with kind is D29's.
CREATE INDEX idx_runs_task_kind ON runs (task_id, kind, attempt);

-- D30 point 7's witness: a clean review is an explicit call. Set once, by
-- record_review_findings, in the same transaction as the rows it writes, including
-- when it writes none. NULL on every row that is not a review, and on a review that
-- never called.
ALTER TABLE runs ADD COLUMN findings_recorded_at TEXT;

-- Task 021's, riding here because 021 has no file of its own. review_instructions is
-- content (ADR-0032 point 3). review_config is a JSON ReviewConfig whose every field is
-- optional and inherits when absent; NULL inherits all of it.
ALTER TABLE tasks ADD COLUMN review_instructions TEXT;
ALTER TABLE tasks ADD COLUMN review_config TEXT;
ALTER TABLE repositories ADD COLUMN review_config TEXT;

CREATE TABLE review_findings (
    id                 TEXT NOT NULL PRIMARY KEY,
    task_id            TEXT NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    review_run_id      TEXT NOT NULL REFERENCES runs (id) ON DELETE CASCADE,
    ordinal            INTEGER NOT NULL,  -- the reviewer's order within one call, from 0
    severity           TEXT NOT NULL CHECK (severity IN ('critical', 'high', 'medium', 'low')),
    title              TEXT NOT NULL,
    body               TEXT NOT NULL,
    file               TEXT,     -- repository-relative; NULL for the change as a whole
    line               INTEGER,
    fingerprint        TEXT,     -- 021's key for "the same finding again"
    status             TEXT NOT NULL DEFAULT 'open'
                       CHECK (status IN ('open', 'fixed', 'rejected')),
    resolution         TEXT,     -- what the fix run did, or why it declined
    resolved_by_run_id TEXT REFERENCES runs (id) ON DELETE SET NULL,
    created_at         TEXT NOT NULL,
    resolved_at        TEXT,
    CHECK (status <> 'rejected' OR resolution IS NOT NULL)
);
CREATE INDEX idx_review_findings_task ON review_findings (task_id, status);
CREATE UNIQUE INDEX idx_review_findings_review_run ON review_findings (review_run_id, ordinal);
CREATE INDEX idx_review_findings_resolved_by ON review_findings (resolved_by_run_id);
