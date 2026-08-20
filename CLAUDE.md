# Rimaia — working agreement

Rimaia queues implementation plans and runs them unattended with Claude Code, in git
worktrees, on the user's own subscription. Kanban board in, reviewable branches out.

**Status: design complete, implementation not started.** The current `src/` and
`src-tauri/` contents are an unmodified Tauri starter with a counter UI.

## Read these before writing code

1. **[`docs/adr/`](docs/adr/README.md) — 17 ADRs. Read the ones your task lists.**
   They are not background reading; they are the decisions you are implementing. Every
   task file names its ADRs in front matter.
2. **[`tasks/`](tasks/README.md)** — the backlog, in order. **A task's acceptance criteria
   are the contract.** Done means all of them hold.

**If you disagree with an ADR, or a task needs a decision no ADR covers: stop and say so.**
Write a new ADR, or ask. Do not invent architecture in an implementation task and do not
silently deviate — the whole point of the ADRs is that the next agent inherits the same
decisions.

Do not renumber ADRs or tasks. Numbers are stable ids; the README tables define order.

## Layout

| Path | Contents |
| --- | --- |
| `crates/core/` | `rimaia-core` — all logic. **Must not depend on `tauri`** (ADR-0015) |
| `src-tauri/` | Tauri shell: commands, window, state wiring. Thin |
| `src/` | React 19 + TypeScript frontend |
| `docs/adr/` | Architecture decision records |
| `tasks/` | Task backlog |

## Commands

```bash
npm run tauri dev                     # run the app
npm run typecheck                     # tsc --noEmit
npm run test                          # vitest run
cargo test -p rimaia-core             # logic tests, no system deps needed
cargo fmt --all --check
cargo clippy -p rimaia-core -- -D warnings
cargo check --workspace               # includes the Tauri shell
```

`SQLX_OFFLINE=true` is set in CI. Re-run `cargo sqlx prepare` after changing any query and
commit the `.sqlx` cache.

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
- Unattended runs use `--permission-mode bypassPermissions` behind a per-repository
  opt-in. Do not weaken or widen this without amending ADR-0012.
- Worktrees live under the app data directory, never inside a repository.
- Board `position` is a fractional float; ordering is the priority mechanism. There is no
  separate priority field (ADR-0007).
- A dependency is satisfied when its run **succeeds**, not when a human marks it done
  (ADR-0008). This is deliberate and load-bearing.
