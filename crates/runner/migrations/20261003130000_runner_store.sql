-- The runner's own store: which runner this is, its settings, and what it has adopted
-- (ADR-0028 points 2, 3 and 5, task 040).
--
-- # A second file, beside rimaia.db
--
-- Everything that describes one machine lives here, where the board never sees it. The
-- board is another file in solo and another machine in team mode, so nothing below has a
-- foreign key into it. Seam-contract D28 part 6 fixes these statements column for column;
-- they are copied here unchanged, and only this header is the task's own. D4's amendment in
-- D28 freezes the file once it lands.
--
-- # Why the settings are key/value
--
-- runner_settings is D3's shape, moved: the ten keys 038's placement calls Runner, copied
-- byte for byte out of rimaia.db's settings table by the adoption step named `settings`. An
-- absent key means the default, exactly as it does on the board, so the copy writes no row
-- for a key the board did not hold and this file seeds nothing.
--
-- # Why adoptions is a table and not a version
--
-- Each one-time copy out of rimaia.db is Rust, not SQL, because a migration cannot know
-- where the board file is, and a headless runner has none. A row here is what makes a copy
-- one-time: the step runs only while its row is absent, and writes the row in the same
-- transaction as what it copied. A later task appends a step and gets a fresh row of its
-- own, with no migration.

-- Which runner this store is, reporting to which board (ADR-0030 point 5). server_url
-- is NULL in solo. The runner token is in the keychain, never here.
CREATE TABLE runner_identity (
    singleton  INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
    runner_id  TEXT NOT NULL,
    server_url TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE runner_settings (
    key   TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
);

-- One row per completed one-time copy out of rimaia.db.
CREATE TABLE adoptions (
    step       TEXT NOT NULL PRIMARY KEY,
    adopted_at TEXT NOT NULL
);
