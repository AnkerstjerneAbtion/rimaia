-- Tasks that chose a model before `strategy_mode` was ever written (task 020).
--
-- # Why this exists
--
-- Until this branch, `run_task` spawned with `tasks.model` and `tasks.effort`
-- unconditionally, and nothing ever wrote `tasks.strategy_mode` — it sat at its
-- schema default of 'default' on every row. Task 020 gives that column meaning:
-- ADR-0016's `default` mode means "use Rimaia's configured default", so a task
-- in it deliberately ignores its own model and effort and resolves through the
-- repository and global defaults instead (seam-contract D17.6).
--
-- Correct going forward, and silently destructive backwards. Every card whose
-- model was picked through the old panel is stored as (model = 'opus',
-- strategy_mode = 'default') — a combination that used to mean "use opus" and
-- now means "ignore opus". Without this backfill those runs quietly change
-- model, while the panel still displays the one the user chose. Six such rows
-- existed on the development machine, one of them at opus/high.
--
-- `manual` is exactly what those rows mean: a human chose this pair by hand.
--
-- # The fourth migration
--
-- Seam-contract D4 caps the count and says a task that needs another stops and
-- asks. This one asked, and D4 carries a second amendment naming it. It is
-- timestamped after the name reserved for tasks 011 and 012
-- (20260828120000_dependencies_and_parallel.sql) so the two cannot collide, and
-- it adds no column, no index and no constraint — it is a one-time repair of
-- data whose meaning this branch changed underneath it.
--
-- # Why it is safe to run twice
--
-- The WHERE clause is its own idempotence: after this runs, no row matches it.
-- Rows written after the migration already carry a mode `update_task` chose.
UPDATE tasks
   SET strategy_mode = 'manual'
 WHERE strategy_mode = 'default'
   AND (model IS NOT NULL OR effort IS NOT NULL);
