-- Sign-in sessions, API tokens and runner pairing codes (task 047).
--
-- # The fifth of team mode's board files
--
-- Seam-contract D28 fixes this file's statements for task 047, and they are copied here
-- unchanged; only this header is the task's own. D4's amendment in D28 freezes the file
-- once it lands. It is additive: nothing else in the board schema changes.
--
-- # Only hashes
--
-- Every secret a client holds (a session cookie, an `rmd_`, `rmr_` or `rmp_` token, a
-- pairing code) is stored as the lowercase hex SHA-256 of the whole string, computed by
-- `identity::secret::hash` and nowhere else. The secrets are 256 random bits, so the hash
-- is enough to find the row and useless to anyone who reads it: a copy of this file, or of
-- any backup Litestream streams of it (ADR-0037 point 2), authenticates nobody. The pairing
-- code is the one short secret, and it is single use, lives ten minutes and is rate
-- limited (ADR-0030 point 5).
--
-- # Revoking deletes the row
--
-- A revoked session or token is a deleted row, never a flag, so the next request presents
-- a hash that no longer exists and is refused exactly as a never-issued one is. There is no
-- revoked state for a query to forget to filter on. A runner token is the exception that
-- keeps the rule: unpairing deletes the token and stamps `runners.unpaired_at`, because the
-- runner row outlives it for the runs that name it (`identity::tokens::unpair_runner`).
--
-- # What has no table
--
-- The OAuth `state`, the PKCE verifier and the desktop's one-time code (ADR-0030 point 4)
-- live in server memory for at most ten minutes (`identity::pending`). Written here, they
-- would ride in every backup for seconds of use; a restart mid-sign-in costs one click
-- instead (D28, "Sign-in state stays in memory"). There is no CSRF column either: the
-- token is derived from the session secret on every request.

-- ADR-0030 point 2. The cookie carries a random secret; only its hex SHA-256 is stored.
-- id is the handle the account page revokes by.
CREATE TABLE sessions (
    id           TEXT NOT NULL PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    secret_hash  TEXT NOT NULL UNIQUE,
    user_agent   TEXT,
    created_at   TEXT NOT NULL,
    last_used_at TEXT NOT NULL
);
CREATE INDEX idx_sessions_user ON sessions (user_id);

-- ADR-0030 point 3: one table, three kinds. secret_hash is the hex SHA-256 of the whole
-- token, prefix included. Revoking deletes the row.
CREATE TABLE api_tokens (
    id             TEXT NOT NULL PRIMARY KEY,
    user_id        TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind           TEXT NOT NULL CHECK (kind IN ('desktop', 'runner', 'personal')),
    secret_hash    TEXT NOT NULL UNIQUE,
    label          TEXT NOT NULL,
    runner_id      TEXT REFERENCES runners (id) ON DELETE CASCADE,
    created_at     TEXT NOT NULL,
    last_used_at   TEXT,
    last_used_from TEXT,
    expires_at     TEXT,
    CHECK ((kind = 'runner') = (runner_id IS NOT NULL))
);
CREATE INDEX idx_api_tokens_user ON api_tokens (user_id);
CREATE INDEX idx_api_tokens_runner ON api_tokens (runner_id);

-- ADR-0030 point 6. No rows: every team the token's user belongs to.
CREATE TABLE api_token_teams (
    token_id TEXT NOT NULL REFERENCES api_tokens (id) ON DELETE CASCADE,
    team_id  TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    PRIMARY KEY (token_id, team_id)
);
CREATE INDEX idx_api_token_teams_team ON api_token_teams (team_id);

-- ADR-0030 point 5: single use, ten minutes, deleted when redeemed.
CREATE TABLE pairing_codes (
    code_hash  TEXT NOT NULL PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
CREATE INDEX idx_pairing_codes_user ON pairing_codes (user_id);
