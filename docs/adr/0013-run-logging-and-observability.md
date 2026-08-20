# 13. Run logging: JSONL transcripts plus indexed summaries

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

The morning review is the product. Its quality depends entirely on how well the user can
answer, in a few seconds per task: what did this run do, did it work, and if not, where
did it go wrong.

A headless Claude Code run emits a stream of JSON events — system init, assistant messages
with tool calls, tool results, and a final result with cost and turn counts. A long
implementation run produces a lot of them.

Two demands pull in opposite directions: complete detail for debugging a 2am failure, and
a fast board that shows twelve tasks without loading megabytes of transcript.

## Decision

Split the two.

### Transcripts are files

The raw event stream is written verbatim to
`<app-data>/runs/<task-id>/<run-id>.jsonl`, one JSON object per line, appended as events
arrive and flushed continuously so a crashed run still leaves a readable transcript. The
path is recorded on the `runs` row.

### Summaries are rows

The `runs` table holds only what the UI queries: status, session id, attempt number, start
and end time, exit classification (ADR-0011), turn count, cost, the composed prompt
(ADR-0009), a short outcome summary, and the extracted PR URL when the agent opened one.

### Live view

While a run is active, the backend emits Tauri events for a bounded ring buffer of recent
activity — current tool call, last assistant message, elapsed time, turn count. The Runs
view tails this. Completed runs are read from the JSONL file on demand, paginated.

### What the run view shows

For each run, in this order: outcome and error class, the git diff summary
(files changed, insertions, deletions), the commits made on the branch, the PR link if
present, then the transcript. **The diff and the commits come first, because that is what
review is actually about**; the transcript is for when the diff raises a question.

### Retention

Transcripts are kept until their task is deleted or the user prunes. The UI reports total
run-log size alongside worktree size.

## Consequences

- The board and the review view stay fast regardless of transcript size, because they only
  touch indexed columns.
- A crash mid-run still leaves a complete transcript up to the crash — the most valuable
  case.
- Transcripts are plain JSONL: greppable, diffable, and pipeable into another Claude
  session for "explain what went wrong here".
- Two storage locations to keep consistent. Reconciled at startup like worktrees: a `runs`
  row pointing at a missing file is marked, not trusted.
- Disk grows with run history. Acceptable for a local tool with explicit pruning.
- The diff-first ordering is what makes the morning review fast, and it means the run view
  depends on git operations, not just on stored events.

## Alternatives considered

- **All events as rows in SQLite.** Queryable transcripts, and thousands of rows per run
  with the board's queries competing against them for the single writer.
- **Transcript in a `TEXT` column on `runs`.** Simple, and forces every board query to
  either project carefully or drag megabytes.
- **Log to stdout only.** Nothing to review in the morning, which is the entire product.
