# CLI fixture streams

Each file is a recording of one `claude` CLI invocation's stdout: line-delimited JSON
events, one per line. The seven real recordings are byte-for-byte captures — six against
Claude Code 2.1.234 (see `spike/FINDINGS.md`) and `strategy-proposal.jsonl` against 2.1.247
— and must never be re-recorded, reformatted or pretty-printed. The three synthetic ones
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
`allowed`" and never on any particular value, so a real capture carrying `"limited"`,
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
