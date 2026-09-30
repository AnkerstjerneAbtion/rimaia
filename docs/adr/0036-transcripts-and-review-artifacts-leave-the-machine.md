# 36. Transcripts and review artifacts leave the machine

- **Status:** Proposed
- **Date:** 2026-09-30

## Context

ADR-0013 split a run's record in two:

- **A JSONL transcript file** on disk, `<app-data>/runs/<task-id>/<run-id>.jsonl`, appended
  as events arrive and flushed continuously so a crash still leaves a readable file.
- **An indexed `runs` row** with the summary.

The live view tails a bounded buffer over a separate channel (seam-contract D14). ADR-0022
made the row permanent and the transcript a cache that pruning may delete.

On a team (ADR-0027) the run executes on a runner, and the people who review it may be
elsewhere: another member's desktop, a browser, the morning after, with the runner's laptop
closed. The row is on the server already, because runs are board records (ADR-0028). The
transcript and the review information (diff, commits) exist only on the runner.

This is also the point where Rimaia starts moving source code between people. A transcript
contains everything the agent read and wrote:

- file contents;
- command output;
- sometimes environment values.

Until now it never left the machine that produced it.

## Decision

### 1. The runner writes locally first, then uploads

The runner keeps ADR-0013's local file exactly as it is: written as events arrive, flushed
continuously, readable after a crash. Uploading is a second step, never a replacement:

- **During the run**, the runner uploads the transcript in chunks by byte offset, through
  the runner protocol's `append_transcript` (ADR-0034 point 2). Each chunk is idempotent, so
  a retried upload never duplicates lines.
- **At the end of the run**, the final chunk is followed by `finish_run`, which carries the
  offset of the last byte. The server marks the transcript complete only when it holds every
  byte up to there.
- **Pending chunks and reports wait in the runner's outbox** (ADR-0028) if the server is
  unreachable, and are sent in order once it is back. A run that finished while a laptop was
  offline appears on the board when the laptop reconnects, complete.

The server's copy is the one every client reads. The runner's copy is a cache the runner may
prune on its own schedule (ADR-0022 point 2), once the server has acknowledged every byte.

### 2. The live tail flows through the server

The runner forwards `RunTail` messages (seam-contract D14) to the server, which fans them out
over SSE to clients that asked for that run's tail (ADR-0034 point 3). D14's rules hold end
to end:

- a dropped tail message is nothing, and is never replayed;
- the tail is never the source of truth for anything persisted.

The transcript upload is the source of truth.

### 3. Review artifacts travel with the transcript

The review bundle from ADR-0033 point 7 (diff summary, commits, head and base commits, PR
URL, and the patch up to a size cap) is uploaded with `finish_run`. ADR-0013's diff-first run
view and task 017's morning review render from it on every client.

### 4. Redaction happens before anything is written

ADR-0020 point 7 already scrubs a repository's forge token from the transcript and the tail
before they are written. That stays, and applies before upload, because the local file and
the uploaded copy are the same bytes. The runner also redacts the values of its own tokens
(ADR-0030) and of any environment variable whose name marks it as a secret (names ending in
`_TOKEN`, `_SECRET`, `_KEY` or `_PASSWORD`).

This is a floor, not a guarantee. A transcript can still contain a secret the agent read out
of a file, and the consent dialog says so (point 5).

### 5. A runner may keep transcripts at home

Each runner has a setting: **upload full transcripts** (default) or **upload summaries
only**.

- **Summaries only:** the runner uploads the row, the review bundle and the tail, but not the
  JSONL. The board shows "Transcript kept on Alice's laptop", and only that desktop can open
  it.
- **Why it exists:** it is for a member working in a repository whose content must not leave
  their machine, or who simply prefers it. Nothing else about the run changes.

The default is upload, because the reviewer is often not the runner's owner and a transcript
is how a failed night gets explained. Pairing a runner shows this setting and what the
transcript can contain in plain language, in the same register as ADR-0012's dialog.

### 6. Retention is a team setting, and rows are still kept

Each team sets how long the server keeps transcripts: 90 days by default, from 7 days to
"until the task is deleted". When the period passes, the server deletes the transcript and
marks the row, which is ADR-0022's pruning applied on the server's side. `runs` rows, the
review bundle's summary fields and analytics (ADR-0022, task 024) are kept permanently. The
patch is pruned with the transcript. Deleting a task or a team deletes both (ADR-0029
point 6).

### 7. Storage is the server's disk, behind a narrow interface

Transcripts are stored as files under the server's data directory, keyed by team and run,
with `runs.transcript_key` holding the storage key (ADR-0028). The server reaches them through an
interface with four operations: append at offset, read a range, mark complete, delete. That
way, moving to object storage is an implementation of that interface, not a change to the
protocol. Files on a disk stay the answer until a hosting constraint says otherwise
(ADR-0037).

## Consequences

- **Anyone on the team can review any run, whenever,** including when the machine that ran
  it is off. That is what makes a team queue reviewable at all.
- **Source code now lives on the server.** Plans, diffs, patches and transcripts from every
  team's repositories are stored by whoever hosts the instance. For the hosted instance that
  is Abtion, acting as a data processor for every team that is not Abtion itself (ADR-0037).
- **Offline-first runners.** A runner that works through a night without a connection loses
  nothing: its outbox and local transcripts are enough to catch up.
- **Storage grows with use.** A transcript is tens of megabytes (ADR-0022), a busy team runs
  dozens a night, and retention is what bounds it. The instance reports storage per team.
- **Summaries-only runners make some reviews local.** A reviewer who is not the runner's
  owner sees the diff and the outcome but not the transcript. That is the owner's choice, and
  the board says so rather than showing an empty transcript.

## Alternatives considered

- **Stream events instead of uploading bytes.** Send each event as a structured message, and
  have the server write the file. The server then becomes the transcript's author, and a
  runner offline mid-run has to buffer structured events rather than bytes it already
  wrote. Uploading the file the runner already has keeps one writer of the canonical bytes.
- **Never upload transcripts; review summaries only.** Maximum privacy by default. Rejected
  as the default because on a team the reviewer is usually someone else, and "why did this
  fail" is the question transcripts exist to answer. Kept as the per-runner option in point 5.
- **Store transcripts in the database.** ADR-0013 rejected this for the single-writer reason,
  and it holds more strongly on a server where the database carries every team's board.
- **Object storage from the start.** Better at scale, and a second service to provision,
  authenticate and back up, before a single disk has run out. Point 7's interface keeps the
  move cheap when it is needed.
