-- Runner leases, the generation that fences them, and the pin a retry keeps (task 043).
--
-- # The third of team mode's board files
--
-- Seam-contract D28 fixes this file's statements for task 043, and they are copied here
-- unchanged; only this header is the task's own. D4's amendment in D28 freezes the file
-- once it lands.
--
-- # Who writes what
--
-- `board::lease` is the only writer of `runner_leases` and of `tasks.pinned_runner_id`,
-- and the claim transaction is the only writer of `tasks.lease_generation`, which it only
-- ever increments (ADR-0031 points 1, 3 and 4; D31 point 3). `strategy_requested_at` and
-- `strategy_requested_by` are task 060's: they are created here, and nothing writes or
-- reads them until then. No `Task` field carries any of the four columns; card
-- presentation is task 061's.
--
-- Every existing task starts at generation 0 with no lease and no pin, which is the truth
-- about a board that has never held a lease. A task an older build left `running` or
-- `queued` has no lease row, and startup's solo arm reconciles it as it always was
-- (`scheduler::reconcile::reconcile_unrecorded`).

-- ADR-0031 point 1. The primary key is the store's own "two machines never run one task
-- at once". run_id is NULL for 'strategy' (the planner writes no runs row, D17.5) and,
-- for the other purposes, until start_run writes the row. expires_at NULL is a solo
-- lease, which never expires (ADR-0031 point 5).
CREATE TABLE runner_leases (
    task_id     TEXT NOT NULL PRIMARY KEY REFERENCES tasks (id) ON DELETE CASCADE,
    purpose     TEXT NOT NULL
                CHECK (purpose IN ('implementation', 'strategy', 'review', 'fix')),
    run_id      TEXT REFERENCES runs (id) ON DELETE CASCADE,
    runner_id   TEXT NOT NULL REFERENCES runners (id) ON DELETE RESTRICT,
    generation  INTEGER NOT NULL,
    acquired_at TEXT NOT NULL,
    expires_at  TEXT,
    CHECK (purpose <> 'strategy' OR run_id IS NULL)
);
CREATE INDEX idx_runner_leases_runner ON runner_leases (runner_id);
CREATE INDEX idx_runner_leases_run ON runner_leases (run_id);
CREATE INDEX idx_runner_leases_expiry ON runner_leases (expires_at)
    WHERE expires_at IS NOT NULL;

-- Monotonic per task across leases: a claim increments it and copies it onto the new
-- lease in the same transaction (ADR-0031 point 3's fencing).
ALTER TABLE tasks ADD COLUMN lease_generation INTEGER NOT NULL DEFAULT 0;
-- ADR-0031 point 4: after an expiry or a retry, only this runner may claim the task.
ALTER TABLE tasks ADD COLUMN pinned_runner_id TEXT REFERENCES runners (id) ON DELETE SET NULL;
CREATE INDEX idx_tasks_pinned_runner ON tasks (pinned_runner_id);
-- ADR-0035 point 6: a hosted plan_task_strategy records a request that a runner claims
-- with purpose 'strategy'. Written by task 060.
ALTER TABLE tasks ADD COLUMN strategy_requested_at TEXT;
ALTER TABLE tasks ADD COLUMN strategy_requested_by TEXT REFERENCES users (id) ON DELETE SET NULL;
