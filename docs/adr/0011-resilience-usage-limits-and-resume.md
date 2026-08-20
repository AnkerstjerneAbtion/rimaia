# 11. Resilience: usage-limit detection, backoff, session resume

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

An overnight queue on a Claude subscription will hit the plan's usage limit. That is
expected, not exceptional — it is the normal failure mode of the product's core use case.
When it happens the queue must not die; it must wait for the window to reset and continue
where it stopped.

Other failures need different handling: a transient API error deserves a quick retry, a
crashed CLI process deserves a resume, and a plan the agent genuinely cannot complete
deserves to stop and be reported rather than burning tokens in a loop.

Restarting a run from scratch on every failure would be both wasteful and destructive —
the worktree already has commits in it.

## Decision

Classify every run termination, then act on the class.

| Class | Signal | Action |
| --- | --- | --- |
| `success` | `result` event, `is_error: false` | Task → `in_review` |
| `usage_limit` | Usage-limit message; reset epoch parsed when present | Wait until reset (+ jitter), then resume. Unbounded attempts, capped by the run window |
| `transient` | Non-zero exit, API/network/overload error, empty stream | Exponential backoff (1m, 5m, 15m, 15m…), resume, max 5 attempts |
| `interrupted` | Process died, or app restarted while `running` | Resume once immediately, then treat as `transient` |
| `fatal` | Auth failure, missing CLI, invalid worktree, max turns, agent reported failure | No retry. Task → `run_state = failed`, error surfaced on the card |
| `cancelled` | User action | No retry |

**Retries resume, they do not restart.** Because the session id is pre-assigned with
`--session-id` (ADR-0004), every retry is `claude -p --resume <session-id>` with a short
continuation prompt, in the same worktree, on the same branch. Work already committed
stays; context already built is reused.

Details:

- **Usage-limit reset time.** When the CLI reports a reset timestamp, wait until then plus
  a small jitter. When it does not, fall back to a fixed 15-minute poll. Both paths are
  capped only by the run window, because a limit that resets in four hours should still be
  picked up at 3am.
- **Attempt records.** Each attempt is a row in `runs`, sharing the task's session id, so
  the history of an overnight task reads as the sequence of walls it hit.
- **The scheduler does not block on a waiting task.** A task in `waiting_retry` releases
  its concurrency slot; the queue continues with other tasks and comes back to it. In
  sequential mode a usage-limit wall pauses everything — correct, since the next task
  would hit the same wall.
- **A usage-limit hit pauses new starts globally** for the duration of the wait, in both
  modes. Starting a fresh task into a limited window just burns a start.
- **Startup reconciliation.** Runs left `running` by a crash are marked `interrupted` and
  offered for resume.
- Classification lives in one module with unit tests over captured CLI output, because it
  is the piece most likely to break on a CLI update and the one whose breakage is least
  visible — a misclassified `usage_limit` looks like a hard failure at 2am.

## Consequences

- An evening queue survives hitting the plan limit, which is the difference between the
  product working and not.
- Resume rather than restart means partial work is kept and tokens are not re-spent
  rebuilding context.
- Resumes accumulate context; a task that resumes many times may hit compaction or context
  limits. Claude Code handles compaction itself; `--max-turns` per attempt bounds runaway
  loops.
- Classification depends on CLI output shapes that may change between versions. Unknown
  terminations default to `transient` with limited attempts — retrying a fatal error a few
  times is cheaper than giving up on a recoverable one.
- The retry cap prevents a genuinely impossible task from consuming the whole night.

## Alternatives considered

- **Fixed 15-minute retry for everything.** Simple, and what the user first described.
  Wrong for auth failures (retried forever, pointlessly) and slow for a limit that already
  reset. The reset timestamp, when available, is strictly better information.
- **Restart from a clean worktree on retry.** Discards committed work and re-spends the
  tokens that produced it.
- **Give up on first failure and report in the morning.** Predictable, and wastes the
  entire night on the first transient blip.

---

## Amendment, 2026-08-20 — verified by spike against CLI 2.1.234

The retry-and-resume decision stands and the resume mechanism was confirmed end to end: a
four-step task was killed after step 2, resumed with `--resume` and a one-line continuation
prompt, and completed steps 3 and 4 with prior commits intact. Full write-up in
[`spike/FINDINGS.md`](../../spike/FINDINGS.md). Three corrections to the mechanics above:

### Usage limits are a structured event, not a message to grep

This ADR assumed we would parse a reset timestamp out of an error message, with a fixed
15-minute poll as fallback. Reality is better. **Every run** emits, unprompted and early:

```json
{"type": "rate_limit_event",
 "rate_limit_info": {"status": "allowed", "resetsAt": 1787224800,
                     "rateLimitType": "five_hour", "overageStatus": "rejected",
                     "isUsingOverage": false}}
```

A typed event with an epoch `resetsAt`, a `status`, and the window type — **on every run,
not only on failure**. Prefer it; keep message parsing only as a fallback.

This enables something not considered above: the scheduler can read limit state *before*
committing to a long task, and can schedule its next wake-up precisely rather than polling.
Worth exploring in task 014.

Not yet observed: the payload when `status` is not `"allowed"`. **The `usage_limit` fixture
is still missing** — capture it opportunistically the first time a real queue hits the wall.
Until then, that branch of the classifier is written against an assumed shape.

### Classify on `terminal_reason`, and expect a `result` even when killed

Captured signatures:

| Scenario | exit | `subtype` | `terminal_reason` | `is_error` |
| --- | --- | --- | --- | --- |
| Success | 0 | `success` | `completed` | false |
| Killed (SIGTERM) | 143 | `error_during_execution` | `aborted_streaming` | true |
| Turn limit | 1 | `error_max_turns` | `max_turns` | true |

`terminal_reason` is the cleanest discriminator and was not in the original design.

**A killed run still emits a `result` event before exiting** — the stream does not simply
stop, which is what this ADR assumed. And a killed process exits **143**, so
`status.code().is_none()` is the wrong "was it signalled" check; classify on exit code plus
the `result` event.

### `result` already carries the run metrics

`num_turns`, `total_cost_usd`, `duration_ms`, `usage`, `modelUsage` and `permission_denials`
all arrive on the terminal event — nothing to derive for the `runs` row in ADR-0013.
