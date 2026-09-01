# CLI fixture streams

Each file is a recording of one `claude` CLI invocation's stdout: line-delimited JSON
events, one per line. The seven real recordings are byte-for-byte captures — six against
Claude Code 2.1.234 (see `spike/FINDINGS.md`) and `strategy-proposal.jsonl` against 2.1.247
— and must never be re-recorded, reformatted or pretty-printed. The three synthetic ones
are edited copies of `success.jsonl`, built to exercise one specific parser edge case each.

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
