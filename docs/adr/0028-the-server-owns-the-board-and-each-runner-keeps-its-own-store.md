# 28. The server owns the board's database, and each runner keeps its own store

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

ADR-0003 chose SQLite because one process owned the file and a few async writers inside that
process shared one pool. ADR-0027 splits that process into a server and runners, which raises
two questions:

1. What does the server store its data in?
2. Where do the things that describe one machine go?

The board's database is full of things that are only true on one machine:

- **Absolute local paths:** `repositories.path`, `repositories.worktree_root`,
  `tasks.worktree_path` and `runs.log_path`.
- **Per-machine repository settings:** `on_archive` and `on_archive_script` (ADR-0025),
  `max_concurrency`, `allow_unattended_runs` and the `credential_*` metadata (ADR-0020).
- **Per-machine run configuration:** the whole `schedules` table (task 013).

The key/value `settings` table mixes team intent with machine state (point 2 lists every
key). In a shared database, a path is meaningless on every other machine. A setting like
`queue_state` becomes a single switch that one person flips for the whole team.

On the storage engine: `rimaia-core` has about ninety compile-checked query macros and a
couple of dozen unchecked queries, a `.sqlx` cache typed as SQLite, and `BEGIN IMMEDIATE` as
the locking primitive under `set_run_state`. None of it sits behind an abstraction. Services
take a `SqlitePool`.

## Decision

### 1. The server keeps SQLite

The server is one process that owns one file. That is the condition ADR-0003 was argued
under, so its reasoning carries over unchanged: `sqlx`, WAL, `foreign_keys`, `busy_timeout`,
one pool, migrations embedded and applied at startup, append-only.

The write load stays inside what ADR-0003 claimed SQLite handles well:

- a few hundred task edits and a few hundred run transitions a day for a busy team;
- one heartbeat request per active runner every thirty seconds, which renews that runner's
  leases in one short transaction (ADR-0031).

**Revisit when any of these becomes true:**

- more than one server process must write at once (high availability, horizontal scaling);
- write contention shows up as `SQLITE_BUSY` reaching a client;
- a hosting constraint rules out a persistent local disk.

Moving to Postgres is then a decision with a known cost: every query macro, the offline cache
and the `BEGIN IMMEDIATE` locking. That cost is paid when it buys something.

### 2. Every piece of state is placed by what it describes

The test for any key or column: if two members could reasonably want different values because
their machines, subscriptions or preferences differ, it does not belong to the team.

| Team (server) | Runner (runner store) | User (server, per user) |
| --- | --- | --- |
| `base_instructions` | `run_environment`, `mcp_port` | `subscription_monthly_usd` (task 024) |
| `strategy_catalogue`, `strategy_default`, `strategy_approval` | `max_concurrency`, `schedule_mode`, `schedules` table | the trust list (ADR-0032) |
| `max_turns` (a team ceiling; a runner may set lower) | `queue_state`, `active_run_window`, `usage_limit_pause_until` | |
| `disallowed_tools` (a team floor; a runner may add more) | `worktree_auto_cleanup`, `doctor_dismissals`, `onboarding_dismissed` | |
| transcript retention (ADR-0036) | per repository: clone path, `worktree_root`, `max_concurrency`, `on_archive`, `on_archive_script`, `credential_*` metadata | |
| review loop (ADR-0017): `review_instructions`, enabled, `max_review_loops`, blocking severity | ceiling on model and effort for every run, the review phase's included (ADR-0032 point 3) | |
| per repository: remote, default branch, unattended ceiling (ADR-0032) | per repository: unattended consent (ADR-0032) | |

`max_turns` and `disallowed_tools` are split rather than placed, because each has a direction
that is safe to override. A runner may make its own runs stricter. It may never make them
looser than the team decided. The effective value is the stricter of the two.

`allow_unattended_runs` splits into the team ceiling and the runner's consent (ADR-0032). When
an existing install migrates, its value becomes both, so a solo user's opt-ins carry over
unchanged.

**No local path reaches the board.** Two replacements:

- **Where a task's worktree lives.** A run records `runs.runner_id` (ADR-0031). "Which machine
  has this worktree" is a question about a runner, not a path. The path is in that runner's
  store.
- **Where a transcript lives.** A new `runs.transcript_key` names the server's copy
  (ADR-0036). The runner's local copy is recorded in its own store. `runs.log_path` stops being
  read.

A test asserts that no board DTO carries an absolute path.

### 3. Each runner has its own SQLite store

The store is a separate file in the runner's data directory, with its own migration set under
`crates/runner/migrations/` and its own offline query cache. It holds:

- the checkout mapping: team repository to local clone, plus the per-repository runner
  settings above (ADR-0033);
- worktree paths, local transcript paths and the leases the runner holds (ADR-0031);
- runner settings (the table above) and credential metadata (ADR-0020). Secrets stay in the
  keychain;
- an **outbox** of reports and transcript chunks the server has not acknowledged yet, so a
  runner that loses its connection mid-run loses nothing (ADR-0036).

A second file rather than more tables in one file, because the two schemas version
independently. A connected runner never has the board's tables, and a headless runner never
needs them. Mixing them would put ADR-0023's collision (one branch's migration breaking every
other branch) between two products that release on different days.

### 4. Solo mode has both files, in one data directory

`rimaia.db` stays the board. `runner.db` is new beside it. `RIMAIA_DATA_DIR` (ADR-0023)
relocates both, so a development launch isolates both, as it does today.

### 5. Existing installs migrate in two releases

1. **Copy and relax.** On the first launch of the new version, a runner migration copies
   everything the table above assigns to a runner into `runner.db`. Board migrations then do
   three things:
   - **Rebuild `repositories` and `runs`** with the local-path columns nullable. `path`,
     `worktree_root` and `log_path` are `NOT NULL` with no default, and a repository
     registered from the browser has no path. SQLite cannot relax a constraint in place, so
     this is the rename-copy-drop rebuild `db::models` warns about. It is done once, in its
     own migration.
   - **Add `team_settings(team_id, key, value)`** as a new table. It does not alter
     `settings`, whose single-column key cannot take a team without a rebuild.
   - **Add the team, user and assignment columns** of ADR-0029 to ADR-0032.
2. **Drop.** A migration in a following release drops the retired columns and the old
   `settings` rows. The release in between is what makes a rollback possible.

Neither step edits a shipped migration. The existing installation becomes a solo server with
one implicit team (ADR-0029) and keeps every task, run and setting it had.

### 6. Migrations are still named up front

Seam-contract D4's reasoning (two worktrees reaching for the same next timestamp collide
silently) applies to both migration sets. The tasks that implement this record get their
migration names written into the seam contract before they start, as D4's amendments do today.

## Consequences

- **Most of the query code survives.** The server keeps the pool, the macros, the offline
  cache and the locking. What changes is which columns and keys the queries touch.
- **Two migration sets, two offline caches, two data files.** CLAUDE.md's single
  `DATABASE_URL` and root `.sqlx` prepare step becomes one per crate, and CI runs both. More
  moving parts. Each one exists because the two products ship and roll back independently.
- **A board uploaded from solo is possible later.** Seam-contract D10 makes every id a UUIDv4
  string, and ADR-0029 gives the solo team and user ordinary generated ids, so solo ids do not
  collide with a server's. "Move my solo board into a team" is an export and import, not a
  renumbering. Deliberately not decided here.
- **Backups matter now.** In solo mode a lost file is one person's history. On a server it is
  a team's. ADR-0037 makes continuous backup part of hosting, not an afterthought.

## Alternatives considered

- **Postgres on the server from day one.** The conventional choice for a hosted multi-user
  app, and it would make multiple server processes possible. Rejected now: it means porting
  every query and the offline cache, and replacing `BEGIN IMMEDIATE`, to support a scale the
  hosted instance has not reached. It stays the named next step in point 1.
- **An abstraction over both engines (a repository trait, or `sqlx::Any`).** Lets solo stay
  on SQLite while the server uses Postgres. Rejected because it gives up the compile-time
  checking the codebase relies on. `sqlx::Any` has no offline macros, and a hand-written
  repository layer is two implementations of every query, which is the "same invariant, two
  implementations" failure ADR-0006 and ADR-0018 exist to prevent.
- **Keep machine-local columns on the board, keyed by runner.** A `task_worktrees(task_id,
  runner_id, path)` table on the server works. But it ships every member's filesystem layout
  to the team, and it makes the server the store for facts only one machine can check. The
  runner is already the only thing that can tell whether a path exists.
- **Runner state in a config file instead of SQLite.** Simpler for a handful of settings.
  Wrong for the outbox and the leases, which need transactions and must survive a crash
  mid-write. Those are exactly ADR-0003's reasons for SQLite in the first place.
