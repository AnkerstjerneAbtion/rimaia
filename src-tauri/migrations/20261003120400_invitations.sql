-- Invitation links into a team (task 051).
--
-- # The sixth of team mode's board files
--
-- Seam-contract D28 fixes this file's statements for task 051, and they are copied here
-- unchanged; only this header is the task's own. D4's amendment in D28 freezes the file
-- once it lands. It is additive: nothing else in the board schema changes.
--
-- # Only the hash
--
-- The link an owner copies carries an `rmi_` token: 256 random bits that admit whoever holds
-- them, once, within seven days (ADR-0029 point 4). Like every other secret on this board it
-- is stored as the lowercase hex SHA-256 of the whole string, computed by
-- `identity::secret::hash` and nowhere else, so this table, or a backup Litestream streams
-- of it (ADR-0037 point 2), lets nobody into a team. The token is shown to the owner once,
-- when it is made, and no read returns it again.
--
-- # Accepted and revoked rows stay
--
-- Using a link stamps `accepted_by` and `accepted_at`, and revoking one stamps `revoked_at`;
-- neither deletes the row. The rows are the team's record of who let whom in, which an owner
-- reads in the invitation history. Single use needs no lock of its own: acceptance is one
-- conditional `UPDATE` that only a row with neither stamp, and not yet expired, matches.
-- The rows go when their team does, through `ON DELETE CASCADE`, and a deleted account
-- leaves "a former member" in `created_by` and `accepted_by`, as it does on a task.

-- ADR-0029 point 4: single use, seven days, tied to no address, only the hash stored.
-- Accepted and revoked rows are kept as the team's record of who let whom in.
CREATE TABLE invitations (
    id          TEXT NOT NULL PRIMARY KEY,
    team_id     TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    token_hash  TEXT NOT NULL UNIQUE,
    role        TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    created_by  TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    accepted_by TEXT REFERENCES users (id) ON DELETE SET NULL,
    accepted_at TEXT,
    revoked_at  TEXT
);
CREATE INDEX idx_invitations_team ON invitations (team_id);
