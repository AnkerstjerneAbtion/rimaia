# Spike findings

Run 2026-08-20 against Claude Code **2.1.234**, git 2.50.0, rustc 1.91.1, macOS.

Purpose: verify the assumptions in ADR-0004 and ADR-0011 before task 001 builds on them,
and capture the first CLI fixtures for task 019.

**Verdict: all four assumptions hold. Two ADRs need amending — for things the spike found
that the design did not anticipate.**

---

## 1. The four questions

| Question | Answer |
| --- | --- |
| Does the `stream-json` contract look as ADR-0004 assumes? | **Yes**, with more event types than expected (§3) |
| Does `bypassPermissions` run unattended in a worktree? | **Yes.** Read, Edit, Bash, `git commit` — no prompts, no stalls |
| Is there a usage-limit signal, and does it carry a reset time? | **Better than assumed** — a structured event on every run (§4) |
| Does `--resume` continue a session whose process was killed? | **Yes**, cleanly (§5) |

End-to-end: a plan went in, a branch came out with 4 commits, 10 passing tests, and a
module doc comment. 24s and $0.15 for a small task on Sonnet.

## 2. Finding: the operator's own config leaks into every run

**This is the most consequential thing the spike found.** Spawned with no isolation flags,
the run inherited the operator's personal Claude Code environment:

| | Default | `--strict-mcp-config --setting-sources project,local` |
| --- | --- | --- |
| Tools exposed | **255** | 26 |
| MCP servers connected | 2 (Brewale, Google Drive) | 0 |
| `SessionStart` hooks fired | 4 | 0 |
| Cache-creation tokens | 16,455 | 3,179 |
| Cost, identical prompt | **$0.1061** | $0.0291 |

Same one-word prompt. **3.6× the cost and 5× the context, before doing any work.**

Worse than cost: a personal `SessionStart` hook injected an unrelated instruction into the
run's system context, and two personal MCP servers were connected and callable. An
unattended overnight queue would inherit whatever the operator happened to have configured
that day — including hooks that change how the agent writes commits.

Rimaia must isolate the run environment. `--setting-sources project,local` keeps the
repository's own `CLAUDE.md` and project settings — which we *want* — while dropping user
-level hooks, plugins, and output styles.

→ **Amend ADR-0004** with the required isolation flags.

### 2b. Nested-session env vars

Claude Code exports 13 `CLAUDE_*` / `CLAUDECODE` variables into its children. A run spawned
from a Claude Code session (which is how Rimaia will be developed and tested) inherits them
and does not behave like a fresh session. The runner must strip them explicitly — see
`INHERITED_CLAUDE_ENV` in `src/main.rs`.

## 3. The event stream

Observed types, in a real run:

```
system/init            session_id, model, permissionMode, cwd, tools[], mcp_servers[],
                       apiKeySource, claude_code_version, slash_commands, skills, agents
rate_limit_event       see §4
assistant              message.content[] — text and tool_use blocks
user                   tool results
system/thinking_tokens frequent, low-value for us
system/vcs_state_changed  fires on commit — useful, tells us the branch moved
system/hook_started
system/hook_response   only when hooks are enabled (see §2)
result                 terminal event
```

Notes for the parser (task 008):

- `system` carries **many** subtypes; several are undocumented. Switch on `subtype` and
  treat unknown ones as opaque. ADR-0004's tolerant-parsing rule is not theoretical —
  `thinking_tokens` and `vcs_state_changed` were not anticipated and neither is in the
  `--help` output.
- `apiKeySource: "none"` confirms subscription auth. ADR-0004's premise holds.
- `permissionMode` is echoed in `init`, so the runner can **verify** the mode it asked for
  was applied rather than assuming.
- `--session-id` is accepted and echoed back, so pre-assignment works (ADR-0004).
- Naive substring matching on `"type":"` mis-parses, because assistant events nest
  `"type":"message"` inside. Parse the JSON.

## 4. Usage limits are a structured event, not a message to grep

**Every run** emits a `rate_limit_event`, unprompted:

```json
{
  "type": "rate_limit_event",
  "rate_limit_info": {
    "status": "allowed",
    "resetsAt": 1787224800,
    "rateLimitType": "five_hour",
    "overageStatus": "rejected",
    "overageDisabledReason": "org_level_disabled",
    "isUsingOverage": false
  }
}
```

ADR-0011 assumed we would parse a reset timestamp out of an error message and fall back to
a fixed 15-minute poll when it was absent. Reality is much better: a typed event with an
epoch `resetsAt`, a `status`, and the window type — **emitted early in every run, not only
on failure**.

That enables something the ADR did not consider: the scheduler can read the limit state
*before* committing to a long task, and can schedule the next wake-up precisely.

Not observed: the payload when `status` is something other than `"allowed"`. The spike did
not hit a real limit. **The `usage_limit` fixture is still missing** — capture it
opportunistically the first time a real queue hits the wall.

→ **Amend ADR-0011**: prefer the structured event; keep message parsing only as a fallback.

## 5. Exit classification — four signatures captured

| Scenario | exit | `subtype` | `terminal_reason` | `is_error` |
| --- | --- | --- | --- | --- |
| Success | 0 | `success` | `completed` | false |
| Killed with SIGTERM | 143 | `error_during_execution` | `aborted_streaming` | true |
| Turn limit reached | 1 | `error_max_turns` | `max_turns` | true |
| Resume, then success | 0 | `success` | `completed` | false |

`terminal_reason` is the cleanest discriminator and was not in the design. A killed run
**still emits a `result` event** before exiting — the stream does not simply stop, which is
what I had assumed when writing ADR-0011.

`result` also carries `num_turns`, `total_cost_usd`, `duration_ms`, `usage`, `modelUsage`,
and `permission_denials` — everything the `runs` row in ADR-0013 needs, with no extra work.

## 6. Resume works, and it resumes rather than restarts

Task B was a four-step plan. Killed 25s in, after 2 of 4 steps and 2 commits. Resumed with
`--resume <session-id>` and a one-line continuation prompt — **not** the original plan.

Result: it picked up at step 3, completed steps 3 and 4, and left 4 commits and 10 passing
tests. Prior commits intact. Exactly the behaviour ADR-0011 depends on.

`--session-id` on the first run and `--resume` on the retry is the right shape.

## 7. Process supervision

- `Command::process_group(0)` plus `kill -TERM -<pgid>` kills the whole tree. **Zero
  orphaned processes** after the kill test.
- Prompt on stdin works; close stdin or the run hangs.
- Reading `stdout` with `BufReader::lines()` and flushing to disk per line gives a
  complete transcript even for a killed run — the file was intact and valid JSONL.
- A killed process exits **143** (128+SIGTERM), not a signal-death — so
  `status.code().is_none()` is the wrong check. Classify on exit code plus the `result`
  event.

## 8. Fixtures produced

In `fixtures/cli/`, for task 019:

| File | Scenario |
| --- | --- |
| `success.jsonl` | Clean implementation run with commit |
| `interrupted-sigterm.jsonl` | Killed mid-run |
| `resume-success.jsonl` | `--resume` completing an interrupted session |
| `max-turns.jsonl` | Turn-limit cutoff |
| `env-leak-default-settings.jsonl` | Operator config leaking in (§2) |
| `env-leak-isolated-settings.jsonl` | Same prompt, isolated |

Still needed: `usage_limit`, a transient API error, an auth failure. Capture opportunistically.

`fixtures/make-test-repo.sh` rebuilds the throwaway repository the runs went against.

## 9. What to change before task 001

1. **ADR-0004** — add required isolation flags: `--strict-mcp-config`,
   `--setting-sources project,local`, and stripping inherited `CLAUDE_*` env vars. Note
   `permissionMode` verification via the `init` event.
2. **ADR-0011** — replace message-grepping with the `rate_limit_event`; classify on
   `terminal_reason` + `subtype`; note that killed runs do emit a `result`.
3. **ADR-0013** — `result` already carries cost, turns, duration and usage; no derivation
   needed.
4. **Task 019** — fixtures exist; the harness should read this directory.
5. **Task 008** — parse JSON properly, switch on `system.subtype`, expect unknown subtypes.

## 10. Cost note

Small Sonnet tasks ran $0.03–$0.23 each. A 10-task evening queue is plausibly $2–5 on
Sonnet, more on Opus. Worth surfacing per-run cost in the UI from the start — `result`
gives it for free — and worth remembering that the §2 isolation fix cuts a fixed overhead
off **every** run.
