-- The whole schema (ADR-0003), in one file rather than seven.
--
-- Nothing has shipped, so the append-only rule has nothing to bite on yet, and
-- SQLite's DDL is transactional — a half-applied schema is not a state this can
-- reach. From the moment this ships it is frozen: never edit it again.
--
-- Two conventions run through every table below.
--
-- Timestamps are TEXT holding RFC 3339 UTC. That is exactly what sqlx writes for
-- `chrono::DateTime<Utc>` on SQLite, it sorts lexicographically, and it stays
-- legible to somebody poking at the file with the sqlite3 CLI — which ADR-0003
-- does not merely tolerate but counts as a feature.
--
-- Enums are TEXT with a CHECK naming the whole domain. The CHECK is not the
-- enforcement; the service is (ADR-0006), because three writers share this file
-- and a fourth is the user with a CLI. It is here because SQLite cannot add one
-- afterwards, which makes every domain below permanent — widening one costs a
-- rename-copy-drop rebuild against live data. That asymmetry is why value
-- domains an ADR has already closed are constrained here and shape rules a later
-- task still owns are not: an index or a unique constraint can be added in a
-- later migration, a CHECK cannot.
--
-- These are not STRICT tables, deliberately. STRICT permits only
-- INT/INTEGER/REAL/TEXT/BLOB/ANY as declared types, which forbids the BOOLEAN
-- decltype — and that decltype is what makes `sqlx::query!` infer `bool` rather
-- than `i64`. "Hardening" this later means taking a type override on every
-- boolean column in exchange for nothing.
--
-- Ids are `TEXT NOT NULL PRIMARY KEY` holding `Uuid::new_v4().to_string()`
-- (seam-contract D10, which also records why never the `uuid::Uuid` type). The
-- NOT NULL is load-bearing rather than decorative: SQLite permits NULL in a
-- non-INTEGER primary key, and sqlx reads the schema for nullability, so without
-- it every `SELECT id` comes back `Option<String>`.

-- Registered local git repositories (ADR-0005). The repository on disk is
-- authoritative — these rows record what Rimaia was told, and startup
-- reconciliation trusts the filesystem where the two disagree. That is also why
-- there is no `remote_url` column: `git remote get-url` answers it correctly
-- every time, and a cached copy can only be stale.
--
-- `path` carries no UNIQUE constraint. Whether two rows may name one directory
-- is task 003's validation rule, and a unique index — unlike a CHECK — can still
-- be added by a later migration if that rule wants a backstop.
CREATE TABLE repositories (
    id                    TEXT NOT NULL PRIMARY KEY,
    name                  TEXT NOT NULL,
    path                  TEXT NOT NULL,
    default_branch        TEXT NOT NULL,
    worktree_root         TEXT NOT NULL,
    -- ADR-0012's per-repository opt-in to `--permission-mode bypassPermissions`.
    -- Off is the only safe default, so the schema states it rather than trusting
    -- whichever writer inserts the row to remember.
    allow_unattended_runs BOOLEAN NOT NULL DEFAULT 0,
    created_at            TEXT NOT NULL
);

-- Key/value application settings (ADR-0003). Untyped and unconstrained on
-- purpose: which keys exist, what each holds, and what happens when one is
-- absent are business rules, and they belong to the typed accessor task 006
-- ships (seam-contract D3). This is storage and nothing else — task 002 seeds no
-- rows here.
CREATE TABLE settings (
    key   TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
);

-- The unit of handoff from planning to execution (ADR-0007). Two dimensions, two
-- fields: `board_column` says where the card is in the user's process,
-- `run_state` where it is in the machine's.
CREATE TABLE tasks (
    id                  TEXT NOT NULL PRIMARY KEY,
    -- RESTRICT, not CASCADE. ADR-0005 makes repository state authoritative and
    -- task 003 refuses to remove a repository that still has tasks, reporting
    -- how many; this is the backstop for a writer that is not that service — the
    -- MCP server, or the user with the sqlite3 CLI.
    repository_id       TEXT NOT NULL REFERENCES repositories (id) ON DELETE RESTRICT,
    title               TEXT NOT NULL,
    -- Nullable because `not_ready` means precisely "captured, plan missing or
    -- incomplete" (ADR-0007). NULL is that state; an empty string would be a
    -- second way to spell it.
    plan                TEXT,
    extra_instructions  TEXT,
    -- Stored as `board_column` because `column` is a SQL keyword. The Rust field
    -- and the JSON DTO both stay `column`; reads alias it back with
    -- `board_column AS "column: BoardColumn"`, which costs nothing, because an
    -- enum type override is needed on this column regardless.
    board_column        TEXT NOT NULL CHECK (board_column IN ('not_ready', 'ready', 'in_review', 'done')),
    -- Fractional priority within (repository_id, board_column): inserting
    -- between two cards takes the midpoint and rewrites no neighbours. Board
    -- order is execution order, and there is no separate priority field
    -- (ADR-0007).
    position            REAL NOT NULL,
    -- ADR-0007's seven, and only these seven. `interrupted` is deliberately not
    -- among them: a run that died with the app carries that word on its own
    -- `runs` row while the task lands `failed` and stays in `ready`
    -- (seam-contract D9).
    run_state           TEXT NOT NULL CHECK (run_state IN ('idle', 'queued', 'running', 'blocked', 'waiting_retry', 'failed', 'cancelled')),
    branch              TEXT,
    worktree_path       TEXT,
    -- ADR-0016's execution strategy. Shipped now because `model` and `effort`
    -- are meaningless without the mode that says whether to use them: NULL would
    -- otherwise have to mean both "left on the default" and "explicitly set to
    -- the default".
    strategy_mode       TEXT NOT NULL DEFAULT 'default' CHECK (strategy_mode IN ('default', 'manual', 'planned')),
    -- Free text, not enums. ADR-0016 populates both dropdowns from
    -- configuration because models ship faster than releases do, so a CHECK here
    -- would be a release blocker the first time Anthropic names something new.
    model               TEXT,
    effort              TEXT,
    -- The planner's proposal as JSON: phases, per-phase model and effort, agent
    -- counts, rationale (ADR-0016).
    strategy_plan       TEXT,
    strategy_source     TEXT CHECK (strategy_source IS NULL OR strategy_source IN ('user', 'planner')),
    strategy_updated_at TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);

-- The board read: one column of one repository, in priority order. Doubles as
-- the index the RESTRICT above needs — without a repository_id-leading index,
-- deleting a repository scans every task to decide whether to refuse.
CREATE INDEX idx_tasks_board ON tasks (repository_id, board_column, position);

-- The scheduler's selection predicate and startup reconciliation both filter on
-- run_state before anything else (ADR-0010).
CREATE INDEX idx_tasks_run_state ON tasks (run_state);

-- Zero or more external references per task — an Asana task, a GitHub issue, a
-- doc (ADR-0007). `position` is REAL for the same reason the board's is:
-- reordering must not rewrite the rows around it.
CREATE TABLE task_links (
    id       TEXT NOT NULL PRIMARY KEY,
    task_id  TEXT NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    label    TEXT NOT NULL,
    url      TEXT NOT NULL,
    position REAL NOT NULL
);

-- Every link of one task, in order — and the index the CASCADE above needs,
-- since SQLite otherwise scans this whole table once per deleted task.
CREATE INDEX idx_task_links_task ON task_links (task_id, position);

-- "task_id is blocked by depends_on_task_id" (ADR-0008). Nothing reads this
-- until task 011; it ships now because migrations are append-only and guessing
-- the shape right here is cheaper than a migration chain later.
CREATE TABLE task_dependencies (
    -- The two ends behave differently on purpose. Deleting the dependent takes
    -- its own outgoing edges with it. Deleting something others depend on is
    -- refused, which is ADR-0008's "deleting a task with dependents is refused;
    -- the edges must be removed first" enforced by the store and not only by the
    -- service.
    task_id            TEXT NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    depends_on_task_id TEXT NOT NULL REFERENCES tasks (id) ON DELETE RESTRICT,
    PRIMARY KEY (task_id, depends_on_task_id),
    -- The only cycle a single row can express. Longer ones need a walk of the
    -- graph and belong to task 011.
    CHECK (task_id <> depends_on_task_id)
);

-- "What does this task block?" — the reverse edge, for the blocked-task view and
-- for the RESTRICT above. The composite primary key's index does not serve
-- either, because depends_on_task_id is not its leftmost column.
CREATE INDEX idx_task_dependencies_depends_on ON task_dependencies (depends_on_task_id);

-- One row per attempt (ADR-0011), holding only what the UI queries. The raw
-- event stream is a JSONL file on disk with its path in `log_path`, which is how
-- ADR-0013 keeps megabytes of transcript out of every board query.
CREATE TABLE runs (
    id            TEXT NOT NULL PRIMARY KEY,
    -- CASCADE: transcripts and their rows are kept "until their task is deleted"
    -- (ADR-0013). Orphaned JSONL files are the reconciler's problem, not the
    -- foreign key's.
    task_id       TEXT NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    attempt       INTEGER NOT NULL,
    -- The coarse lifecycle the Runs view queries. ADR-0013 says this table holds
    -- a status and enumerates nothing, so: `running`, plus one value per terminal
    -- outcome, collapsing ADR-0011's six exit classes onto four. `usage_limit`,
    -- `transient` and `fatal` all land on `failed`, because whether a failure
    -- will be retried is the *task's* business (`run_state = waiting_retry`) and
    -- not this row's — the run itself is over either way. `interrupted` is its
    -- own value rather than a flavour of `failed`: seam-contract D9 puts that
    -- word on the run, and ADR-0010 marks runs left `running` by a crash with it
    -- at startup and offers them for resume. There is no `queued`, because a row
    -- exists here only once a process was spawned for it — task 008 writes no run
    -- state before that, and what is waiting to start is a `tasks.run_state`.
    status        TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'failed', 'cancelled', 'interrupted')),
    -- Generated by Rimaia before the spawn, so `--resume` works even if the
    -- process dies before its `init` event (ADR-0004). Every attempt of one task
    -- shares it.
    session_id    TEXT NOT NULL,
    -- The composed prompt verbatim (ADR-0009), so a morning review can see what
    -- the agent was actually asked rather than what it would be asked today.
    prompt        TEXT NOT NULL,
    started_at    TEXT NOT NULL,
    -- ended_at through cost_usd are NULL while the run is in flight.
    ended_at      TEXT,
    -- ADR-0011's classes. Classification reads `result.terminal_reason` together
    -- with `subtype`, never the exit code alone: a SIGTERM-killed run still emits
    -- a result and exits 143.
    exit_class    TEXT CHECK (exit_class IS NULL OR exit_class IN ('success', 'usage_limit', 'transient', 'interrupted', 'fatal', 'cancelled')),
    error_message TEXT,
    -- Both arrive on the terminal `result` event; neither is derived.
    num_turns     INTEGER,
    cost_usd      REAL,
    -- Known at row creation, since it is a pure function of the task and run ids
    -- (ADR-0013). A row whose file has vanished is marked at startup, not
    -- trusted — which is a reconciliation rule, not a reason to allow NULL here.
    log_path      TEXT NOT NULL,
    pr_url        TEXT,
    -- When the next attempt may start: the usage-limit reset plus jitter, or the
    -- current backoff step (ADR-0011).
    resume_after  TEXT
);

-- One row per attempt, enforced rather than assumed — two writers racing to
-- claim a task must not both record attempt 3. Its leftmost column is task_id,
-- so it is also the index the CASCADE above needs.
CREATE UNIQUE INDEX idx_runs_task_attempt ON runs (task_id, attempt);

-- Named run configurations (ADR-0010). Mode and concurrency are properties of
-- the run configuration, never of a task. Nothing reads this until task 013; it
-- ships now for the same reason task_dependencies does.
CREATE TABLE schedules (
    id              TEXT NOT NULL PRIMARY KEY,
    name            TEXT NOT NULL,
    -- ADR-0010's two. Not `tasks.strategy_mode`, which is ADR-0016's per-task
    -- model and effort selection and shares only the word.
    mode            TEXT NOT NULL CHECK (mode IN ('sequential', 'parallel')),
    -- A cron expression with a timezone, or a wall-clock time, or neither for
    -- "run now". Which combinations are legal is task 013's design and is
    -- deliberately not a CHECK: SQLite cannot drop one, so over-constraining a
    -- table nothing reads yet is strictly worse than under-constraining it.
    cron            TEXT,
    start_at        TEXT,
    max_concurrency INTEGER NOT NULL DEFAULT 2,
    enabled         BOOLEAN NOT NULL DEFAULT 1
);
