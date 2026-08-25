-- The first-launch settings seed (ADR-0009, seam-contract D3 and D4).
--
-- The second and last migration of the MVP. D4 numbered both up front precisely
-- so two tasks in two worktrees could not reach for the same timestamp; a task
-- that believes it needs a third one stops and asks.
--
-- # Seed-once, not restore-every-launch
--
-- This runs exactly once, on the launch that applies it: sqlx records the
-- version in `_sqlx_migrations` and never runs the file again. So a user who
-- later empties the base-instructions box keeps an empty box. That is the
-- deliberate half of the choice — the alternative shape, an "insert if absent"
-- in the startup hook, silently rewrites the field on the next launch, and the
-- user who cleared it on purpose at 2am finds it back with nothing in the UI to
-- explain where it came from. Restoring the default is meant to be an explicit
-- action instead — this migration seeds
-- `rimaia_core::db::settings::DEFAULT_BASE_INSTRUCTIONS`, which that module's
-- tests assert is byte-for-byte the value below, precisely so a future "restore
-- the default" control has a value to restore — but no such control is wired
-- into Settings yet. Until one exists, a user who clears the box has no way
-- back through the UI.
--
-- `ON CONFLICT DO NOTHING` so a database somebody hand-primed with this key
-- through the sqlite3 CLI (ADR-0003 counts that as a supported writer) still
-- migrates rather than failing at the INSERT.
--
-- # Why only one key
--
-- `run_environment` deliberately has no row. Its absence *is* `inherit`
-- (ADR-0004's amendment), and the accessor already has to answer "what happens
-- when this key is missing" — seeding a row would give the default two
-- spellings that can disagree.
--
-- The text is task 006's, reproduced as four lines: one sentence per line. The
-- task file's blockquote wraps two of them to fit its own 90-column margin, and
-- that wrap is the document's formatting, not part of the instruction a user
-- sees in the editor.
INSERT INTO settings (key, value) VALUES (
    'base_instructions',
    'Commit as you work, with focused commits and clear messages.
Run the project''s tests and linters before you finish.
When the work is complete, push the branch and open a pull request describing what changed and why.
If you cannot complete the task, stop, commit what you have, and explain what is blocking you.'
)
ON CONFLICT (key) DO NOTHING;
