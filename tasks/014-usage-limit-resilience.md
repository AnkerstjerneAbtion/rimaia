---
id: "014"
title: Usage-limit resilience and resume
milestone: v0.2
status: ready
depends_on: ["009", "019"]
adrs: ["0011", "0015"]
size: L
---

# Usage-limit resilience and resume

## Goal

Survive hitting the subscription usage limit — and transient failures — by waiting and
resuming the session rather than failing the task or restarting its work.

## Why now

An overnight queue *will* hit the plan limit. Without this, the night ends at the first
wall. With it, the queue picks up when the window resets.

## Scope

**Classification** (`runner/outcome.rs`, extending task 008)

- Act on the full ADR-0011 class set: `success`, `usage_limit`, `transient`,
  `interrupted`, `fatal`, `cancelled`.
- Parse the usage-limit reset timestamp from CLI output when present; fall back to a fixed
  15-minute poll when it is not.
- Unit tests over **checked-in fixture output** for every class. This is the module most
  exposed to a CLI update, and its failure mode — a misclassified `usage_limit` looking
  like a hard failure at 2am — is the least visible.

**Retry policy**

| Class | Behaviour |
| --- | --- |
| `usage_limit` | Wait until reset (+ jitter), then resume. Unbounded attempts, bounded by the run window |
| `transient` | Backoff 1m → 5m → 15m → 15m…, resume, max 5 attempts |
| `interrupted` | Resume once immediately, then treat as `transient` |
| `fatal` / `cancelled` | No retry |

**Resume, not restart**

- Retries invoke `claude -p --resume <session-id>` in the same worktree, on the same
  branch, with the short continuation prompt from task 006.
- Each attempt is its own `runs` row sharing the session id, so a task's history reads as
  the sequence of walls it hit.
- `--max-turns` per attempt bounds runaway loops.

**Scheduler integration**

- A `waiting_retry` task releases its concurrency slot; the queue proceeds with others and
  returns to it at the scheduled time.
- **A usage-limit hit pauses new starts globally** for the wait duration, in both modes —
  starting a fresh task into a limited window just burns a start.
- Startup reconciliation offers resume for runs left `running` by a crash.

**UI**

- Card badge showing `waiting_retry` with the time it will resume.
- Run history shows every attempt with its exit class and wait.
- Manual "Retry now" and "Give up" per task.

## Acceptance criteria

- A run that hits the usage limit schedules a resume at the reported reset time and
  completes when it fires.
- With no reported reset time, it retries every 15 minutes and succeeds once the window
  reopens.
- A resumed run continues in the same worktree with prior commits intact — verified by
  checking commit history across attempts.
- A fatal error (bad auth, missing binary) is not retried.
- Transient retries stop at the cap and the task lands in `failed` with the reason.
- Simulated failures for each class are covered by tests without needing a real limit.
- A retry-policy test exercising a 15-minute backoff completes in milliseconds.

## Notes

The fixture harness from task 019 replays recorded CLI streams, so retry behaviour is
testable without waiting on a real usage limit — and the injected `Clock` means a
15-minute backoff test finishes in milliseconds. Add any missing scenario as a new fixture
rather than reaching for a mock.
