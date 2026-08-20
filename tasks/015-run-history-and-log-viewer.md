---
id: "015"
title: Run history and log viewer
milestone: v0.3
status: ready
depends_on: ["009"]
adrs: ["0013"]
size: M
---

# Run history and log viewer

## Goal

Make a finished run reviewable: what changed, what was committed, whether a PR exists, and
— when that raises a question — what the agent actually did.

## Why now

The morning review is the product's purpose. Task 008 shows a live run; this is the
after-the-fact view the user spends their morning in.

## Scope

**Run detail view**, in this order (ADR-0013 — diff first, transcript last):

1. Outcome: status, exit class, duration, turn count, cost, attempt number.
2. **Diff summary**: files changed, insertions, deletions, per-file breakdown.
3. **Commits** made on the branch, with messages.
4. **PR link** when one was opened.
5. The exact composed prompt the run received.
6. Transcript: paginated, readable rendering of the JSONL — assistant messages, tool
   calls with inputs, tool results (collapsed by default, expandable), errors highlighted.

**History**

- Per-task list of all runs, newest first, with outcome and timing.
- Global Runs view with filters: repository, outcome, date range.
- Text search within a transcript.
- "Open raw log" (reveals the JSONL file) and "Copy log path" — so a transcript can be fed
  to another Claude session for analysis.

**Housekeeping**

- Total run-log size shown in Settings, with a prune action (by age or by task).
- Startup reconciliation: `runs` rows whose log file is missing are marked, not trusted.

## Out of scope

- Inline diff viewing with syntax highlighting. Summary plus "open in editor" is enough
  for now; a full diff viewer is its own task if the summary proves insufficient.

## Acceptance criteria

- A completed run shows correct diff stats and commit list, matching `git` output for the
  branch.
- A 50MB transcript opens without freezing the UI (pagination or virtualization).
- Transcript search finds text in tool inputs as well as assistant messages.
- A run whose log file was deleted shows a clear "log unavailable" state rather than
  erroring.
- Prune removes files and updates the reported size.

## Notes

Diff and commits before transcript is the whole design. Reviewing means looking at the
change; the transcript is for when the change raises a question.
