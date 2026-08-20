# 15. Testing strategy and core/shell crate split

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

Rimaia runs unattended overnight. Its failures are expensive in a specific way: nobody is
watching, and a wrong decision at 2am costs the whole night. The modules where that
happens are known in advance — prompt composition, outcome classification, retry policy,
dependency resolution, ordering — and they are all pure logic over data.

They are also, in the scaffolded layout, buried inside a Tauri crate. Testing them means
compiling Tauri, which on Linux means WebKit system dependencies, which makes CI slow and
local `cargo test` heavy. That is a bad trade for logic that has no UI dependency at all.

The user's position: test everything, but logic first. No end-to-end tests yet.

## Decision

### Split the workspace: `rimaia-core` and the Tauri shell

Restructure into a Cargo workspace:

```
crates/core/     rimaia-core — all logic. No tauri dependency.
                 db, tasks, repo, worktree, runner, scheduler, mcp, prompt
src-tauri/       the shell. Commands, window, tray, state wiring. Thin.
src/             React frontend
```

`rimaia-core` depends on `sqlx`, `tokio`, `serde`, `git` via subprocess — nothing that
needs a display server. `cargo test -p rimaia-core` runs anywhere with no system
dependencies.

This is what ADR-0004's runner seam, ADR-0006's "MCP is a thin adapter", and task 004's
"service takes `&SqlitePool`, no `AppHandle`" were already reaching for. Making it a crate
boundary means the compiler enforces it instead of review.

### Test layers

| Layer | Tool | Scope |
| --- | --- | --- |
| Rust unit | `cargo test`, colocated `#[cfg(test)]` | Pure functions: composition, classification, backoff, position math |
| Rust integration | `cargo test`, `crates/core/tests/` | Service layer against in-memory SQLite; git ops against real temp repos |
| Frontend unit | **Vitest** | Pure TS: state reducers, ordering helpers, formatting, the command wrapper |
| Frontend component | **Vitest + React Testing Library**, jsdom | Components with real logic: board drag results, task form validation |
| End-to-end | **None, deliberately** | See non-goals |

### What must have tests

Not a coverage percentage — a list. These are the modules whose failure is silent and
overnight:

- `prompt::compose_*` — exact expected strings, not substrings (ADR-0009)
- `runner::outcome` — every exit class, against **checked-in fixture CLI streams**
- `runner::events` — malformed lines, unknown event types, truncated streams
- `scheduler` retry and backoff policy — every class from ADR-0011
- `tasks::position_between` and rebalance
- `tasks::set_run_state` — every legal and illegal transition
- `dependencies` — cycle detection, base-ref resolution, multi-dependency case
- `worktree` — create, idempotent re-create, remove, reconcile
- MCP tool handlers — same invariant produces the same rejection as the UI path

A pull request touching one of these without touching its tests is incomplete.

### Rules

1. **Fake the clock, never the filesystem or git.** Time is an injected `Clock` trait so
   retry tests are instant. Git runs against real repositories created in `tempfile::
   TempDir` — a mocked git tells you your mock works.
2. **No `sleep` in tests.** A test that sleeps is a test that will flake in CI.
3. **The CLI is faked by replaying recorded output**, not by mocking a trait. Task 019
   builds the fixture harness; task 014 depends on it.
4. **Every bug fix gets the failing test first.** Especially in the classifier — that is
   where a regression is invisible until an overnight queue dies.
5. Tests are named for the behaviour, not the function:
   `usage_limit_without_reset_time_falls_back_to_fixed_poll`.

### CI

GitHub Actions on push to `main` and on every pull request:

- `cargo fmt --all --check`
- `cargo clippy -p rimaia-core -- -D warnings`
- `cargo test -p rimaia-core` (with `SQLX_OFFLINE=true`)
- `cargo check --workspace` including the Tauri shell, with system deps installed
- `tsc --noEmit`
- `vitest run`

CI does **not** build release bundles. That belongs in a release workflow (task 018).

## Consequences

- Logic tests run in seconds, locally and in CI, with no WebKit anywhere.
- The Tauri boundary stops being a place where business rules can accidentally live,
  because `rimaia-core` cannot import `tauri`.
- The MCP server, being in `core`, is reusable outside the desktop app if that is ever
  wanted. Not a goal, but no longer blocked.
- The crate split is refactoring that must happen in **task 001**, before there is
  anything to move. Task 001 is amended accordingly.
- No E2E means the wiring between frontend and backend is only covered by manual use.
  Accepted for now — see non-goals.

## Non-goals, for now

- **No end-to-end / WebDriver tests.** Tauri's E2E story is `tauri-driver` plus
  WebdriverIO; setup and maintenance cost exceeds the value while the UI is still moving.
  Revisit if a wiring bug ships twice.
- **No coverage threshold.** A percentage gate rewards testing getters. The list above is
  the gate.
- **No snapshot tests of rendered UI.** They fail on every design change and catch
  nothing that matters here.

## Alternatives considered

- **Keep everything in `src-tauri` and install WebKit in CI.** Works, and makes every
  `cargo test` pay for a GUI toolkit it does not use — plus leaves the service boundary
  enforced only by convention.
- **Jest instead of Vitest.** Vite is already the build tool; Vitest shares its transform
  pipeline and config. No reason to run two.
- **E2E from the start.** The highest-value tests here are over logic that E2E covers
  slowly and flakily.
