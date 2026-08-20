# CLI fixture streams

Each file is a recording of one `claude` CLI invocation's stdout: line-delimited JSON
events, one per line. The six real recordings are byte-for-byte captures against Claude
Code 2.1.234 (see `spike/FINDINGS.md`) and must never be re-recorded, reformatted or
pretty-printed. The three synthetic ones are edited copies of `success.jsonl`, built to
exercise one specific parser edge case each.

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
