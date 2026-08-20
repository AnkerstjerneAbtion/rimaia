---
id: "019"
title: Test harness and CI
milestone: mvp
status: ready
depends_on: ["001"]
adrs: ["0015"]
size: M
---

# Test harness and CI

## Goal

Make the testing strategy real: test tooling on both sides, the fixture harness that
everything downstream depends on, a throwaway repository to run against, and a green CI.

## Why now

Second task, immediately after the skeleton. Every task from 002 onward is supposed to
ship with tests; the harness has to exist before that is a reasonable ask. Retrofitting a
fixture harness at task 014 means task 008 was written untested.

`.github/workflows/ci.yml` already exists and is green, but most of it is **inert**: the
Rust jobs and the frontend test step are gated on `Cargo.toml` and `vitest.config.ts`
existing, and emit a skip notice instead of running. Task 001 activates the Rust gates;
this task activates the test gate and makes every step do real work.

**A job that skips is not a job that passes.** The acceptance criterion below is that no
step reports a skip notice.

The pre-implementation spike already produced six fixtures and the test repository — see
[`spike/FINDINGS.md`](../../spike/FINDINGS.md). This task promotes them into
`crates/core/tests/`, it does not re-record them.

## Scope

**Rust**

- Workspace split per ADR-0015 is done in task 001; this task adds the test scaffolding.
- `dev-dependencies`: `tempfile`, `pretty_assertions`, `insta` (optional, for large
  composed-prompt assertions), `tokio-test`.
- In-memory SQLite test helper: fresh migrated database per test, returned as a pool.
- **`Clock` trait** with a real implementation and a controllable test implementation.
  Anything that schedules, waits, or timestamps takes a `Clock`. This is what keeps retry
  tests instant (ADR-0015).
- **Temp git repository builder** — a helper that creates a real repo with commits,
  branches, and optionally a remote, for worktree and diff tests.

**CLI fixture harness** — the important part:

- Recorded `stream-json` output under `crates/core/tests/fixtures/cli/`, one file per
  scenario.
- **Six fixtures already exist** in `spike/fixtures/cli/` — `success`,
  `interrupted-sigterm`, `resume-success`, `max-turns`, and the two `env-leak-*` settings
  comparisons. Move them across; do not re-record them.
- Still to capture, opportunistically from real runs: `usage_limit` (a non-`allowed`
  `rate_limit_event`), a transient API error, an auth failure. Synthesize malformed-line,
  unknown-event-type and truncated-stream cases by editing copies of `success.jsonl`.
- A fake CLI runner that replays a fixture through the same parsing and classification
  path a real process uses, so tasks 008 and 014 can be tested without spawning anything.

**Frontend**

- `vitest`, `@testing-library/react`, `@testing-library/user-event`, `jsdom`.
- Vitest config sharing the Vite pipeline; setup file with RTL cleanup.
- `npm run typecheck` and `npm run test` scripts (CI already calls both).
- One meaningful test per layer as a pattern to copy — a pure helper and a component with
  real logic.

**Test repository**

- A small, real repository with a passing test suite and two or three obvious tasks to
  implement — ground truth for tasks 007, 008 and 009.
- `spike/fixtures/make-test-repo.sh` already builds one (a Rust crate with a `slugify`
  helper and a test). Promote it; the spike ran all four scenarios against it.

## Out of scope

- E2E tooling (ADR-0015 non-goal).
- Release/bundle workflow (task 018).

## Acceptance criteria

- All three CI jobs pass on a pull request **with every step actually running** — no
  `::notice::` skip lines in the log for missing `Cargo.toml` or Vitest config.
- `cargo test -p rimaia-core` runs on a machine with **no** WebKit or GTK installed.
- Adding a new fixture requires no changes outside the fixtures directory.
- The temp-repo builder produces a repository that `git worktree add` succeeds against.
- `npm run test` passes with the example tests.

## Moved out of this task

Two acceptance criteria originally in task 019 were moved to the tasks that own them:

- **Fixture classification** (to tasks/008-claude-code-runner.md): "The fixture harness
  classifies every checked-in scenario correctly" was split from the fixture criterion
  above. The outcome classifier is owned by task 008, which now tests against the fixture
  corpus. Task 019 still delivers the replay plumbing and TestClock that task 008 will
  use.
- **Retry backoff test** (to tasks/014-usage-limit-resilience.md): "A retry-policy test
  exercising a 15-minute backoff completes in milliseconds" moved verbatim, since retry
  policy is task 014's responsibility and the TestClock injection it needs is part of the
  harness task 019 provides.

## Notes

The fixture harness is worth more than the retry code it will test. Build it properly:
tasks 008, 014, and 017 all depend on being able to replay a Claude Code run without
spending tokens or waiting for a real usage limit.
