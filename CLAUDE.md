# Rimaia — working agreement

Rimaia queues implementation plans and runs them unattended with Claude Code, in git
worktrees, on the user's own subscription. Kanban board in, reviewable branches out.

**Status: the MVP walking skeleton is implemented (tasks 001–009, 019 landed); it is awaiting**
**its first real unattended run.** `src/` is the Board/Runs/Settings app wired to the store and
the run queue. `src-tauri/` is the scheduler, process runner and queue commands task 009 added,
on top of task 001's command surface.

## Read these before writing code

1. **[`docs/adr/`](docs/adr/README.md) — 18 ADRs. Read the ones your task lists.**
   They are not background reading; they are the decisions you are implementing. Every
   task file names its ADRs in front matter.
2. **[`tasks/`](tasks/README.md)** — the backlog, in order. **A task's acceptance criteria
   are the contract.** Done means all of them hold. A task with a `landed:` line in its front
   matter is already built — read it for context, do not implement it again.
3. **[`docs/seam-contract.md`](docs/seam-contract.md)** — decisions too small or too local
   to be an ADR, but shared by two or more tasks that would otherwise each have to guess.
   Same "may not deviate silently" rule as an ADR. Read the entries your task's row in its
   "How to use this" table lists.
4. **[`spike/FINDINGS.md`](spike/FINDINGS.md)** — what a throwaway probe actually measured
   against Claude Code 2.1.234, before any of this was built. Read it before touching the
   runner (task 008) or the classifier (task 014). ADR-0004 and ADR-0011 carry amendments
   from it. `spike/` itself is throwaway — delete it once task 019 has promoted its
   fixtures.

**If you disagree with an ADR, or a task needs a decision no ADR covers: stop and say so.**
Write a new ADR, or ask. Do not invent architecture in an implementation task and do not
silently deviate — the whole point of the ADRs is that the next agent inherits the same
decisions.

Do not renumber ADRs or tasks. Numbers are stable ids; the README tables define order.

**When you finish a task, mark it landed.** Open the PR first — the number does not exist until
you do — then push one more commit to the same branch adding `landed: "#N"` to the task's front
matter and filling its `Landed` cell in [`tasks/README.md`](tasks/README.md). Do this even when
the PR carries several tasks; each gets its own line.

Do not reach for `status:` instead. It says whether a task is ready to be *started*, and a
finished task is still `ready` — nothing about it became unready. Two dimensions, two fields,
for the same reason ADR-0007 keeps `run_state` off the board's columns. Without the marker the
backlog cannot tell a task nobody has begun from one that shipped a month ago, and task 010
imports this file into Rimaia itself: unmarked, ten finished tasks arrive in the `ready` column,
which is the run queue.

## Layout

| Path | Contents |
| --- | --- |
| `crates/core/` | `rimaia-core` — all logic. **Must not depend on `tauri`** (ADR-0015) |
| `crates/core/tests/fixtures/` | Recorded `stream-json` CLI streams and the test-repo builder |
| `crates/runner/` | `rimaia-runner`: the runner loop, `runner.db` and the headless binary (ADR-0027, ADR-0028) |
| `crates/runner/migrations/` | `runner.db` migrations. Board migrations stay in `src-tauri/migrations/` |
| `src-tauri/` | Tauri shell: commands, window, state wiring. Thin |
| `src-tauri/migrations/` | SQLite migrations (ADR-0003). The test harness applies these too |
| `src/` | React 19 + TypeScript frontend |
| `docs/adr/` | Architecture decision records |
| `tasks/` | Task backlog |

## Commands

```bash
npm run tauri dev                     # run the app
npm run typecheck                     # tsc --noEmit
npm run test                          # vitest run
npm run build                         # tsc && vite build — the only thing that compiles the CSS
cargo test -p rimaia-core             # logic tests, no system deps needed
cargo test -p rimaia-runner           # runner loop and runner.db, no system deps needed
cargo fmt --all --check
cargo clippy -p rimaia-core --all-targets -- -D warnings
cargo clippy -p rimaia-runner --all-targets -- -D warnings
cargo check --workspace --all-targets # includes the Tauri shell
./scripts/check-command-wiring.sh     # both generate_handler! lists agree, and every commands.ts name is registered
```

**These are exactly the commands `.github/workflows/ci.yml` runs.** Keep them identical.
`--all-targets` is load-bearing, not decoration: without it the `testing` feature is off
and clippy never compiles `crates/core/src/testing/` or any `#[cfg(test)]` module, so a
warning that reddens CI passes locally.

Running the same command is only half of it — you have to run it with the same compiler.
`rust-toolchain.toml` pins one exactly, and rustup fetches it on both sides, so `cargo`
inside this repo is the pinned version whatever your default toolchain is. Two things
follow. **Invoke `cargo` through rustup**, not through a Homebrew or distro `cargo`: those
ignore the toolchain file, and a shadowed `PATH` is how clippy passes locally and fails CI.
And **bump the version only in `rust-toolchain.toml`** — `ci.yml` deliberately does not name
one. A new stable's widened lints are a real change; let them land as a deliberate bump with
its own CI run, not as a surprise on someone's branch.

`cargo test -p rimaia-core` needs **no** `--features testing`. `crates/core/Cargo.toml`
dev-depends on itself with that feature on, which is what makes the harness visible to
tests without shipping it to consumers. Do not add a feature flag to the CI invocation —
it would diverge from the command above for no gain.

`SQLX_OFFLINE=true` is set in CI, so clippy, `cargo test` and `cargo check` all compile the
query macros against a checked-in cache instead of a live database. The board's queries use
`crates/core/.sqlx/` and the runner's use `crates/runner/.sqlx/`. There is no cache at the
workspace root, and a test fails if one appears. After changing any query, or any migration a
query reads, in either crate, regenerate both caches and commit them:

```bash
# Absolute paths, passed with -D and never exported (seam-contract D33).
BOARD_DB="sqlite:$PWD/target/sqlx-prepare-board.db?mode=rwc"
RUNNER_DB="sqlite:$PWD/target/sqlx-prepare-runner.db?mode=rwc"
rm -f target/sqlx-prepare-*.db*

# 1. Board: rimaia-core against src-tauri/migrations, into crates/core/.sqlx
cargo sqlx migrate run --source src-tauri/migrations -D "$BOARD_DB"
(cd crates/core && cargo sqlx prepare -D "$BOARD_DB" -- --all-targets)

# 2. Runner: rimaia-runner against crates/runner/migrations, into crates/runner/.sqlx.
#    The check builds rimaia-core offline from the cache step 1 just wrote, so the
#    prepare below does not rebuild it online against a database with no board tables.
#    Errors from rimaia-runner in the check come from its stale cache, which the next
#    line regenerates.
cargo sqlx migrate run --source crates/runner/migrations -D "$RUNNER_DB"
SQLX_OFFLINE=true cargo check -p rimaia-runner --all-targets || true
(cd crates/runner && cargo sqlx prepare -D "$RUNNER_DB" -- --all-targets)
```

`--all-targets` matters here for the same reason it does for clippy: the integration tests
hold queries too, and a cache generated without them compiles locally and fails CI, not
your machine. Install the matching CLI once — the version must track the `sqlx` version in
`Cargo.toml`:

```bash
cargo install sqlx-cli --version 0.8.6 --no-default-features --features rustls,sqlite
```

## Look at the UI you changed

**If you changed anything under `src/`, take screenshots and look at them before you finish.**
Passing `typecheck`, `vitest` and `build` proves the code compiles and the DOM is right; it
proves nothing about whether the screen is legible, and jsdom has no layout engine. CI does not
run this — it is yours to run.

```bash
npx playwright install webkit            # once per machine; npm ci downloads no browser
npm run screenshot                       # -> .screenshots/latest/, 10 views x 2 schemes x 2 widths
npm run screenshot -- --label before     # a named set; run it before you start, again after
npm run screenshot -- --grep runs        # a narrowed run overwrites only what it writes
```

It renders the dev server's fixture entry (`fixtures.html`, scenarios in `src/dev/fixtures/`) in
headless WebKit on a free port — never 1420 — with no Rust, database or `RIMAIA_DATA_DIR`.
Files are `<scenario>--<view>--<scheme>--<width>.png`, so a before/after pair is two files to
open. Read the PNGs; you can see images.

What to look for, in **both** colour schemes and at **both** widths (1440 and 1024): contrast of
text and badges against their surface, overflow and clipping, wrapping of long titles, and
whether state (running, blocked, failed, dismissed) is distinguishable **without colour**. The
`busy` scenario seeds the ugly cases on purpose. You tend to grade your own work generously:
name what is wrong before what is right.

**A new command needs a fixture row** in `src/dev/fixtures/answers.ts`, or
`src/dev/fixtures/fixtures.test.ts` fails. A new field on a type the seed builds fails
`npm run typecheck` until the seed follows. Writes in fixture mode change nothing; the
screenshots are for looking at, not for diffing.

## Testing (ADR-0015)

Logic-first. Vitest for the frontend, `cargo test` for Rust. **No E2E.**

**These modules must have tests, and a change to one without a change to its tests is
incomplete:** prompt composition · outcome classification · event-stream parsing · retry
and backoff policy · position/rebalance math · run-state transitions · dependency cycles
and base-ref resolution · worktree operations · MCP handlers · tenant isolation.

Rules:

- **Fake the clock. Never fake git or the filesystem.** Git runs against real repos in
  `tempfile::TempDir`. A mocked git proves your mock works.
- **The Claude CLI is faked by replaying recorded fixture streams**, not by mocking a
  trait. Fixtures live in `crates/core/tests/fixtures/`.
- **No `sleep` in tests.** Ever. Inject the clock.
- Bug fix → failing test first.
- Name tests for behaviour: `usage_limit_without_reset_time_falls_back_to_fixed_poll`.
- Assert exact strings for prompt composition, not substrings.

## Conventions

- **No `String` errors across the Tauri boundary.** One `thiserror` type that serializes to
  something the UI can render.
- **No `sh -c`.** Build argument vectors — repository paths contain spaces.
- **Business rules live in `rimaia-core` services.** Tauri commands and MCP handlers are
  thin adapters over the same functions. If a rule is enforced in only one of them, that
  is a bug (ADR-0006).
- **Every service reads and writes through its context's scope** (ADR-0029 point 5): an id
  outside it is answered exactly as a never-issued one, and an MCP tool is not done until
  it has a case in `crates/core/tests/tenant_isolation.rs` (046 extends this to every
  command).
- **Migrations are append-only** once shipped. Never edit a migration that has run.
- **Enums, not strings**, for `column`, `run_state`, `exit_class`, `strategy_mode`.
- **Tolerant parsing of CLI output.** Unknown event types are persisted and ignored, never
  fatal. A Claude Code update must not break a queue (ADR-0004).
- Comments explain intent, constraints, and non-obvious decisions. A comment that restates
  what the code does means the code needs a better name.
- Match the surrounding code's naming, idiom, and comment density.

## Gotchas

- `claude` CLI is a **prerequisite**, not a dependency. Verify it at startup; never bundle
  it (ADR-0004).
- **Runs inherit the operator's Claude Code config by default** — their MCP servers are
  capability. One Settings toggle, `run_environment`: `inherit` (default) or
  `strict_local` (`--strict-mcp-config --setting-sources project,local`). Inheriting costs
  ~3.6× per run, so surface per-run cost near the toggle.
- **Always strip inherited `CLAUDE_*` env vars**, regardless of that setting.
  `CLAUDE_CODE_SESSION_ID` and friends are process identity, not user config.
- **Classify runs on `result.terminal_reason` + `subtype`**, not on exit code alone. A
  SIGTERM-killed run still emits a `result` and exits 143.
- Usage limits arrive as a typed `rate_limit_event` with an epoch `resetsAt`, on every
  run. Do not grep error messages for it.
- Unattended runs use `--permission-mode bypassPermissions` behind a per-repository
  opt-in. Do not weaken or widen this without amending ADR-0012.
- Worktrees live under the app data directory, never inside a repository.
- **Run the app from a worktree with `RIMAIA_DATA_DIR` set to a scratch directory** —
  `RIMAIA_DATA_DIR=/tmp/rimaia-<branch> npm run tauri dev`. Every worktree otherwise resolves
  the *same* data directory, so a branch carrying an unmerged migration writes it into the one
  database every other branch reads, and every branch without that file then refuses to start
  (ADR-0023). It relocates `runner.db` as well as `rimaia.db` (ADR-0028 point 4). It must be
  an absolute path; a relative one or an unexpanded `~` is refused at startup rather than
  guessed at.
- **Board migrations are applied only through `db::migrate`**, which turns foreign keys off
  around the migrator, checks `PRAGMA foreign_key_check` after, and turns them back on
  (seam-contract D28 part 1). Never apply one to a real `rimaia.db` with `cargo sqlx migrate
  run` or the sqlite3 CLI: with enforcement on, a table rebuild's `DROP TABLE` cascades and
  deletes every child row. No migration file begins with `-- no-transaction`. The prepare
  recipe above still works, because the rebuild's guard passes on an empty `tasks`.
- **Machine state lives in `runner.db`, behind `rimaia_core::machine`; its rules stay in
  core, and no board DTO carries an absolute path.** The runner keys, schedules, checkouts
  and worktree records are reached through a `MachineContext` (`AppState.machine`,
  `TestContext::machine()`), never the board's context. `rimaia-runner` stores them and
  decides nothing (seam-contract D31's 2026-10-10 amendments). A run's transcript path is
  derived from its ids, never read off `runs.log_path`.
- Board `position` is a fractional float; ordering is the priority mechanism. There is no
  separate priority field (ADR-0007).
- A dependency is satisfied when its run **succeeds**, not when a human marks it done
  (ADR-0008). This is deliberate and load-bearing.
