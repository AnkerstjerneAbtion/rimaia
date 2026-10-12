# CLI fixture streams

Each file is a recording of one `claude` CLI invocation's stdout: line-delimited JSON
events, one per line. The nine real recordings are byte-for-byte captures — six against
Claude Code 2.1.234 (see `spike/FINDINGS.md`), `strategy-proposal.jsonl` against 2.1.247,
and the two `run-scoped-server-*.jsonl` against 2.1.287 — and must never be re-recorded,
reformatted or pretty-printed. The three synthetic ones
are edited copies of `success.jsonl`, built to exercise one specific parser edge case each.

The last two are a third kind and are kept apart from both: they synthesize a payload
**nobody has observed**, which is a weaker claim than the other two sections make. Read
that section before trusting anything in them.

## Recorded (real, do not edit)

- `success.jsonl` — a clean run: init, a couple of tool turns, a `vcs_state_changed`
  commit event, terminating in a `result` with `subtype: "success"`.
- `interrupted-sigterm.jsonl` — a run killed with SIGTERM mid-turn. Still emits a
  terminal `result` event (per ADR-0004: exit code alone is not a safe signal).
- `max-turns.jsonl` — a run that hits the turn limit; `result.subtype` reflects that,
  not a crash.
- `resume-success.jsonl` — a `--resume` invocation of a prior session, succeeding.
- `env-leak-default-settings.jsonl` / `env-leak-isolated-settings.jsonl` — the same
  probe run once with inherited settings and once with `--strict-mcp-config
  --setting-sources project,local`, used to confirm what each mode does and doesn't
  leak into the run environment.
- `strategy-proposal.jsonl` — task 020's planner: a strategy run that reads a task's plan
  and proposes Sonnet at `high` effort with a three-phase multi-agent workflow, by calling
  `mcp__rimaia__set_task_strategy` once and printing nothing else. Replayed by the planner
  branch of `tests/runner_strategy.rs`'s
  `a_planned_task_runs_a_strategy_run_before_its_implementation_run`, which then asserts
  the implementation run it precedes spawns with `--model sonnet --effort high`. See
  below — this one was captured under conditions the others were not.
- `run-scoped-server-name.jsonl` / `run-scoped-server-allowed.jsonl` — task 035's two CLI
  facts about the run-scoped server name: the bare `--disallowedTools mcp__rimaia` rule
  denies `rimaia` and not `rimaia-run`, and `--allowedTools` matches a hyphenated server.
  See their own section below.

## What `strategy-proposal.jsonl` settles, and what it had to fake

The only recording made after the spike, and the only one in the corpus spawned with
`--effort` — ADR-0004 lists the flag as verified, and until this file nothing had exercised
it. Captured with `--model haiku --effort low` (the catalogue's planner budget) against
2.1.247, in a checkout `make-test-repo.sh` built, with
`--permission-mode acceptEdits --strict-mcp-config --setting-sources project,local
--max-turns 6` — the strategy run's shape.

Two things it settles about `--effort`, neither of which any test can:

- **`system/init` carries no effort field.** The applied model is echoed (as a canonical id
  — `claude-haiku-4-5-20251001`, not the `haiku` that was passed) and so is
  `permissionMode`, but effort is not, so there is nothing in the stream to verify it
  against.
- **An unrecognised value is not rejected at argv parse.** `--effort banana` prints
  `Warning: Unknown --effort value 'banana' — ignoring it and using the default effort.`
  on **stderr**, then runs at the default and exits 0. The stdout stream carries no trace
  at all, so a catalogue typo is visible only in the captured stderr.

Two departures from the invocation Rimaia will actually make, both forced by capturing from
a shell rather than from the runner:

- The MCP server was a stand-in speaking JSON-RPC over **stdio**, not Rimaia's scoped
  `http` handle. The stream does not know the difference: `mcp_servers` reports
  `{"name": "rimaia", "status": "connected"}` and the call arrives as
  `mcp__rimaia__set_task_strategy` either way.
- It needed `--allowedTools mcp__rimaia__set_task_strategy`. Under `acceptEdits` alone the
  same run was refused — a `system/permission_denied` event, an `is_error` tool result, and
  the tool call listed in `result.permission_denials`. A planner that cannot call the one
  tool it exists to call is a run that always falls back, so the strategy run's argv needs
  the allow-list as well as the permission mode.

One incidental thing a reader of the stream will notice: the planner spends its first turn
on `ToolSearch` to load the MCP tool's schema before it can call it, so the four turns in
`result.num_turns` are not four attempts at deciding. A `max-turns` budget for the planner
has to pay for that turn.

## What the two `run-scoped-server-*.jsonl` recordings settle, and what they had to fake

Task 035 serves a run's own MCP handle as `rimaia-run` and keeps `rimaia` for the operator's
surface, which every spawned run is denied by `--disallowedTools` (seam-contract D30). That
rests on two facts about the CLI that no unit test can establish, so both were recorded
(D30 point 8) against Claude Code 2.1.287, which the doctor accepts, in a checkout
`make-test-repo.sh` built, from a shell with every `CLAUDE*` variable unset.

**What was faked.** Rimaia's scoped `http` handle was replaced by two stdio stand-ins in one
`--mcp-config` document. Each speaks JSON-RPC over newline-delimited stdio, lists the tools
it was given on its command line, and answers every `tools/call` with the text
`ok: <server> answered <tool>`. They are not committed, as `strategy-proposal.jsonl`'s was
not; this is the whole of them:

- `rimaia` lists `get_task` and `set_task_strategy`, and answers anything it is sent.
- `rimaia-run` lists `set_task_strategy`, and answers it.

The document, with `standin.py` the stand-in's path:

```json
{"mcpServers":{"rimaia":{"type":"stdio","command":"standin.py","args":["rimaia","get_task","set_task_strategy"]},"rimaia-run":{"type":"stdio","command":"standin.py","args":["rimaia-run","set_task_strategy"]}}}
```

- `run-scoped-server-name.jsonl` pins the denial. The exact argv, `<config>` being the
  document above:

  ```
  claude -p 'Call the tool mcp__rimaia__get_task exactly once with task_id "t-1". Whatever happens, then call the tool mcp__rimaia-run__set_task_strategy exactly once with task_id "t-1". Do nothing else, and then stop.' --output-format stream-json --verbose --model haiku --effort low --permission-mode bypassPermissions --strict-mcp-config --setting-sources project,local --mcp-config <config> --disallowedTools mcp__rimaia --max-turns 8
  ```

  `bypassPermissions` approves an unlisted call, and there is deliberately no
  `--allowedTools`, so the outcome can only come from the bare rule. What it shows: `system/init`
  reports both servers `connected`, yet its `tools` holds `mcp__rimaia-run__set_task_strategy`
  and no `mcp__rimaia__*` tool at all. The denial does not surface as a refused call: the
  rule removes the server's tools from the session, so a `ToolSearch` for
  `mcp__rimaia__get_task` answers `No matching deferred tools found`, the tool is never
  called, and `result.permission_denials` is empty. `mcp__rimaia-run__set_task_strategy` is
  called and answered. The rule matches the server segment exactly, not as a prefix, so
  `rimaia_tool_surface` keeps the bare `mcp__rimaia` entry.
  `the_bare_server_rule_denies_its_own_server_and_not_rimaia_run` in
  `tests/runner_process.rs` reads it.
- `run-scoped-server-allowed.jsonl` pins the planner's shape:

  ```
  claude -p 'Call the tool mcp__rimaia-run__set_task_strategy exactly once with task_id "t-1". Do nothing else, and then stop.' --output-format stream-json --verbose --model haiku --effort low --permission-mode acceptEdits --strict-mcp-config --setting-sources project,local --mcp-config <config> --allowedTools mcp__rimaia-run__set_task_strategy --disallowedTools mcp__rimaia --max-turns 8
  ```

  Under `acceptEdits` an unallowed MCP call is refused (the section above records it), so the
  call succeeding, with `permission_denials` empty, shows that `--allowedTools` matches a
  hyphenated server segment. Without it the respelled planner could fall back on every run
  and nothing would fail.
  `an_allowed_tool_at_a_hyphenated_server_is_callable_under_accept_edits` reads it.

Two incidental things a reader of the streams will notice. The first line of
`run-scoped-server-name.jsonl` and its last are `system/session_state_changed` events, which
the other recording does not carry; the parser keeps them as unknown events, as ADR-0004
asks. And both carry a `rate_limit_event` whose `status` is `allowed_warning`, with
`rateLimitType: "seven_day"` at a utilization of 0.86: the account was past the warning
threshold of its weekly window when they were made. That is the first status other than
`"allowed"` any real recording in this corpus carries. Both runs completed, so neither
classifies as a usage limit.

**Two statuses are therefore known not to be limits: `"allowed"` and `"allowed_warning"`.**
Claude's usage parser reads both as allowed (ADR-0011's 2026-10-05 amendment); every other
word is still a wall, as the section on the unobserved payload below explains. Reading
`allowed_warning` as a wall would classify a run that died without a `result` past the
threshold as a usage limit, and hold the queue until the weekly window resets.
`a_run_past_the_warning_threshold_that_dies_without_a_result_is_not_a_usage_limit` in
`tests/runner_outcome.rs` replays `run-scoped-server-name.jsonl` with its `result` dropped to
pin that.

A `MINIMUM_VERSION` bump re-records both with the argv above.

## Synthesized (edited copies of `success.jsonl`)

- `malformed-line.jsonl` — line 6 (a `system`/`thinking_tokens` event) is cut off
  mid-string and is not valid JSON; every other line is untouched. A parser must skip
  or error only that one line and keep processing the rest of the stream.
- `unknown-event-type.jsonl` — adds a `system` event with an unrecognized `subtype`
  (`context_compaction`) and an event with a top-level `type` no current parser knows
  (`telemetry_ping`), both valid JSON, inserted before the terminal `result`. Per
  ADR-0004, unknown event types/subtypes must be persisted and ignored, never fatal.
- `truncated-stream.jsonl` — cut after 15 well-formed lines, then the writer stops
  mid-object on line 16 (no closing braces, invalid JSON) with no trailing newline and
  no `result` event at all. Mimics a process killed while writing its own output; a
  parser must treat this as an incomplete/unterminated run, not a parse crash.

## Synthesized against an unobserved payload (ADR-0011's named gap)

- `usage-limit.jsonl` / `usage-limit-no-reset.jsonl` — edited copies of
  **`interrupted-sigterm.jsonl`**, not of `success.jsonl`: a run stopped at a wall does
  not complete, so the terminal `result` it needs is the aborted one. Two changes, both
  inside the `rate_limit_event` that recording already carries — `rate_limit_info.status`
  from `"allowed"` to `"rejected"`, and a pinned `resetsAt` of `1787209200`
  (2026-08-20T07:00:00Z), removed entirely in the `-no-reset` variant so the
  fixed-15-minute fallback path has something to run against. No new fields, no new event
  types, nothing else touched.

**These are different from the three above, and the difference is why they get their own
section.** `malformed-line`, `unknown-event-type` and `truncated-stream` synthesize a
*shape* — a cut line, an unknown type — against payloads that were all really recorded.
These synthesize a **value nobody has ever seen**. `spike/FINDINGS.md` §4 and ADR-0011's
2026-08-20 amendment both record it: the `rate_limit_event` payload when `status` is
something other than `"allowed"` was never observed, so `"rejected"` is a guess. It is
the least bad guess available — the word appears in the corpus already, as
`overageStatus`, so nothing in these files is vocabulary that exists nowhere — but it is
still a guess.

**The invented word is not load-bearing, and that is deliberate.**
`runner::outcome::Termination::hit_a_usage_limit` matches on "the status is not
`allowed`" (nor `allowed_warning`, above) and never on any particular value, so a real capture carrying `"limited"`,
`"blocked"` or something nobody has thought of classifies identically.
`a_status_the_corpus_never_saw_still_reads_as_a_usage_limit` in
`tests/runner_outcome.rs` asserts exactly that, over five words, which is what makes the
guess safe to ship.

`tests/harness.rs` lists these under `SYNTHESIZED_UNOBSERVED` rather than `RECORDED`, and
`the_usage_limit_fixtures_are_labelled_unobserved_rather_than_recorded` fails if anyone
moves them.

**Replace both byte-for-byte the first time a real queue hits the wall**, capture the
stream, and delete this section. Capturing it is a human's job and it is the one thing
that turns this branch of the classifier from assumed to proven.
