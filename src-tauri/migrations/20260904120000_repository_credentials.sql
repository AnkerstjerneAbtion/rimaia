-- Task 022, ADR-0020: a repository may carry its own forge token.
--
-- **The secret is not here and never will be.** It lives in the OS keychain,
-- keyed by the repository id (`crates/core/src/credentials/`); these three
-- columns are the metadata a settings pane needs to say *that* a credential
-- exists, whose it is, and when it was added — none of which is sensitive, and
-- all of which the keychain has no good way to hold.
--
-- `credential_login` doubles as the flag: a repository with a login has a
-- credential configured, and one whose keychain item is then missing refuses to
-- start a run rather than silently falling back to the operator's ambient token
-- (ADR-0020 point 5). That is why the injection reads this column rather than
-- asking the keychain whether anything is there — a keychain that cannot be
-- reached must be a refusal, not an absence.
--
-- Seam-contract D4's sixth migration, and its 2026-09-04 amendment is the ask
-- this file is the answer to. The timestamp sorts after every migration already
-- on disk, which is D4's own 2026-09-02 lesson.

-- The login the token resolved to, as `gh api user` reported it. NULL means no
-- credential is configured for this repository.
ALTER TABLE repositories ADD COLUMN credential_login TEXT;

-- What the user called it — "fine-grained, rimaia only, expires March" — so a
-- pane listing four repositories says which token is which without showing any
-- of them.
ALTER TABLE repositories ADD COLUMN credential_label TEXT;

-- RFC 3339 UTC, like every other timestamp in this schema. When the credential
-- was stored, which is the only rotation signal there is: ADR-0020 declines
-- expiry scheduling, and task 018's doctor is where "this is six months old"
-- eventually belongs.
ALTER TABLE repositories ADD COLUMN credential_added_at TEXT;
