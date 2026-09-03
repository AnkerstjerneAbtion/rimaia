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
cargo fmt --all --check
cargo clippy -p rimaia-core --all-targets -- -D warnings
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
query macros against the checked-in `.sqlx/` cache at the workspace root instead of a live
database. After changing any query — or any migration a query reads — regenerate and
commit it:

```bash
export DATABASE_URL="sqlite:target/sqlx-prepare.db?mode=rwc"
cargo sqlx migrate run --source src-tauri/migrations
cargo sqlx prepare --workspace -- --all-targets
```

`--all-targets` matters here for the same reason it does for clippy: the integration tests
hold queries too, and a cache generated without them compiles locally and fails CI, not
your machine. Install the matching CLI once — the version must track the `sqlx` version in
`Cargo.toml`:

```bash
cargo install sqlx-cli --version 0.8.6 --no-default-features --features rustls,sqlite
```

## Testing (ADR-0015)

Logic-first. Vitest for the frontend, `cargo test` for Rust. **No E2E.**

**These modules must have tests, and a change to one without a change to its tests is
incomplete:** prompt composition · outcome classification · event-stream parsing · retry
and backoff policy · position/rebalance math · run-state transitions · dependency cycles
and base-ref resolution · worktree operations · MCP handlers.

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
- Board `position` is a fractional float; ordering is the priority mechanism. There is no
  separate priority field (ADR-0007).
- A dependency is satisfied when its run **succeeds**, not when a human marks it done
  (ADR-0008). This is deliberate and load-bearing.
