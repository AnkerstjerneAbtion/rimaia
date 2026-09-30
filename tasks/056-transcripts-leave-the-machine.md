---
id: "056"
title: Transcripts leave the machine
milestone: v0.5
status: ready
depends_on: ["053"]
adrs: ["0036", "0013", "0022", "0020"]
size: L
---

# Transcripts leave the machine

## Goal

Make [ADR-0036](../docs/adr/0036-transcripts-and-review-artifacts-leave-the-machine.md)
true: a run's JSONL transcript reaches the server, and every client reads the server's copy.
The runner keeps writing ADR-0013's local file exactly as it does today. Uploading is a
second step that follows the file:

- the runner sends the file in chunks, keyed by byte offset (`append_transcript`), and
  `finish_run` carries the final length;
- what the server has not acknowledged waits in `runner.db`'s outbox, survives a restart
  and an outage, and is sent in order when the server is back;
- the transcript, the stderr log and the live tail are redacted before anything is written,
  now also for Rimaia's own tokens and for any environment variable whose name marks it as a
  secret;
- a runner can keep its transcripts at home ("summaries only"), and the board says so;
- the server keeps transcripts for a per-team period (90 days by default), then deletes the
  file and the patch and keeps the row.

Solo mode is unchanged for the user. Its board reads the runner's own file through the same
storage interface, nothing is copied, and nothing is pruned by retention.

## Why now

053 made leases real across the network: a remote runner can claim, heartbeat, report and
finish. What it cannot do yet is let anyone see what the run did. The row and the review
bundle reach the server with `finish_run` (033, 052), but the transcript stays on the
runner's disk. A reviewer on another machine sees an outcome and a diff with no way to ask
why a run failed, which is the question transcripts exist to answer (ADR-0036 Alternatives).

Every piece this task builds on is in place, and nothing after it can be built without it:

- 036 put `append_transcript` and `TranscriptEnd` on the port, and its in-process body
  acknowledges without copying. 052 serves the raw-body route. D31 point 7 lists
  `append_transcript` as the one method with no production caller "until 056".
- 040 created `runner.db`, and D28 reserved `outbox` and `transcript_uploads` for this task.
- 043's fence exists, so this task can place D31 point 12's resend rule in front of it.
- 048 relays tails over SSE and 052 publishes remote runners' tails into it, so the live
  half of ADR-0036 point 2 needs only its redaction and its test.
- 057's push postcondition, 058's headless runner and 059's connected desktop all ship
  runs whose reviewer is someone else. Each of them assumes a transcript arrives.

## Scope

### 1. The migrations, exactly as D28 writes them

- `src-tauri/migrations/20261003120600_transcripts_and_retention.sql`: the five `runs`
  columns (`transcript_key`, `transcript_bytes`, `transcript_complete_at`,
  `transcript_pruned_at`, `transcript_kept_on_runner`) and the two backfilling `UPDATE`s.
- `crates/runner/migrations/20261003130300_outbox.sql`: `outbox`, `idx_outbox_run` and
  `transcript_uploads`.

The header comment of each file is this task's, in the voice of the existing migrations. The
DDL is D28 part 6's, byte for byte. No other file. Regenerate both offline caches with D33
point 3's recipe and commit them.

### 2. The storage interface (ADR-0036 point 7)

New module `crates/core/src/transcripts/`:

- **`store.rs`: `TranscriptStore`**, exactly four operations, with boxed futures for the
  reason D31 point 2 gives (object-safe, no `async-trait`):

  ```rust
  pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

  pub trait TranscriptStore: Send + Sync + 'static {
      /// Writes `bytes` at `offset` and truncates anything after them. `offset` greater
      /// than the stored length is refused and writes nothing. Returns the new length.
      fn append<'a>(&'a self, key: &'a TranscriptKey, offset: u64, bytes: &'a [u8])
          -> StoreFuture<'a, u64>;
      /// Exactly the bytes in `range`, clipped to the stored length.
      fn read_range<'a>(&'a self, key: &'a TranscriptKey, range: Range<u64>)
          -> StoreFuture<'a, Vec<u8>>;
      /// Makes the first `length` bytes durable (`sync_all` for files).
      fn mark_complete<'a>(&'a self, key: &'a TranscriptKey, length: u64)
          -> StoreFuture<'a, ()>;
      /// Removes the transcript. A key that names nothing is not an error.
      fn delete<'a>(&'a self, key: &'a TranscriptKey) -> StoreFuture<'a, ()>;
  }
  ```

  `append` truncates after its write because the board's row, not the file's length, is
  the count of bytes held. If the server dies between writing a chunk and committing the
  row, the next chunk arrives at the row's offset and overwrites the orphaned tail. No
  second bookkeeping is needed.

- **`TranscriptKey`**, a newtype that parses rather than trusts. A key is one or two
  segments joined by `/`, where the last segment ends in `.jsonl` and each segment matches
  `[A-Za-z0-9-]+` (plus the extension on the last). Anything else, including `..`, an
  absolute path, a backslash or an empty segment, is `Error::invalid`. The file store
  therefore cannot be made to touch a path outside its root, whatever a row says.

- **`FileTranscriptStore { root }`**, the only implementation. It resolves a key to
  `root.join(key)` and creates parent directories on `append`.

- **Two key schemes, one per host, and never a fifth store method.** The server mints
  `<team_id>/<run_id>.jsonl` under `<RIMAIA_DATA_DIR>/transcripts/` (ADR-0036 point 7's
  "keyed by team and run"). The solo board mints `<task_id>/<run_id>.jsonl` under
  `AppPaths::runs_dir()`, which is ADR-0013's layout and what the migration's backfill
  wrote (D28). Minting a key is a function of where the board lives, not of the storage, so
  it sits next to the store rather than on the trait:

  ```rust
  pub enum TranscriptHold {
      /// The runner's own files are the board's copy (ADR-0028 point 4, D31 point 4).
      Solo(Arc<dyn TranscriptStore>),
      /// The server holds its own copy, uploaded by runners.
      Server(Arc<dyn TranscriptStore>),
  }
  ```

  `BoardHost` gains `transcripts: TranscriptHold` (D32 point 2: a field arrives with the
  task that needs it), and so does 036's `InProcessBoard`. `src-tauri/src/lib.rs` builds
  `Solo`, the server binary builds `Server`, and `testing::context` builds either.

### 3. The board side

In `crates/core/src/board/service.rs`, behind both adapters:

- **`start_run`** writes `transcript_key` from the hold's scheme, in the insert, and stops
  writing `runs.log_path` (041 stopped reading it; 065 drops it). A resend with a `run_id`
  that already exists for the same task is acknowledged and writes nothing. This check runs
  before 043's fence (D31 point 12).

- **`append_transcript`**, with `Server`, inside one transaction that reads
  `transcript_bytes` as `held`:
  - `offset + len <= held`: already stored. Nothing is written, and the answer is
    `stored_through: held`. This runs before the fence, so a resend after the lease has
    moved on is acknowledged, not fenced.
  - `offset > held`: a gap. Nothing is written, and the answer is `held`, so the runner
    rewinds.
  - Otherwise, the fence (043's `current`) runs. The store writes `bytes[held - offset..]`
    at `held`, and the row's `transcript_bytes` becomes `offset + len`.
  - A chunk that would extend a transcript whose `transcript_complete_at` is set is
    `Error::invalid`.

  With `Solo`, 036's body stands: acknowledge through `offset + len` and copy nothing.

- **`finish_run`** reads `FinishRun::transcript`:
  - `Complete { length }` with `Server`: if `transcript_bytes == length`, it calls
    `mark_complete` and sets `transcript_complete_at` in the transaction that closes the
    row. If fewer bytes are held, it is `Error::invalid` naming both numbers
    (`the server holds <held> of this run's <length> transcript bytes`), and nothing is
    applied: no row closed, no task landed.
  - `Complete { length }` with `Solo`: `transcript_bytes = length` and
    `transcript_complete_at = now`. The runner's file is the board's copy.
  - `KeptOnRunner`: `transcript_kept_on_runner = 1`. With `Server`, any bytes already held
    are deleted and `transcript_bytes` becomes `0`: the owner's choice wins over bytes that
    arrived first.

  **A resent `finish_run` gets the first answer (D31 point 12), rebuilt from the board and
  never from a stored reply.** If the row is already closed, the answer is the row as it now
  reads, plus:
  - `Continue { kind }`, if a lease with the report's generation is still held for the task
    and its `run_id` is this run. The next phase has not started yet, and `kind` follows
    021's table from the closed row's kind: implementation → review, review → fix,
    fix → review.
  - `Released { resume_after: row.resume_after }` otherwise.

  This check also runs before the fence. 036's "has already been finalized" refusal is
  deleted.

- **`get_run` and `list_runs*`** gain `transcript: TranscriptState`, which says what a
  reader will find. It is a tagged union with `camelCase` fields:
  `complete`, `incomplete { bytesHeld }`, `keptOnRunner { runnerId, runnerLabel }`,
  `pruned { at }`, and `none` (no key). `runnerLabel` is `runners.label`. With `Solo`,
  `complete` and `incomplete` are also checked with `try_exists` on read, which is the
  ADR-0013 amendment's rule, and a missing file is `none`. `logAvailable` stays, and means
  "a board read will return lines".

### 4. The transcript reads flip to the board (D32 point 8)

In one commit: the registry rows for `read_run_transcript_page`, `search_run_transcript`
and `summarize_run_transcript` become `board` `Read`, their handlers move into
`api/board/`, their wrappers switch to `board<T>`, and `check-command-wiring.sh` passes.

- `runs::transcript::{read_page, search, summarize}` stop taking a `&Path`. They read
  through `read_range` only, in windows of `TRANSCRIPT_READ_WINDOW = 1 MiB`, over
  `(&dyn TranscriptStore, &TranscriptKey)`. Their parsing, their limits and their output
  types do not change. An unterminated last line is not yielded unless the transcript is
  `complete`, because an upload can cut a line in two.
- The handler reads the run under the caller's scope (a run in another team is
  `not_found`, ADR-0029 point 5) and answers from the hold. `keptOnRunner` and `pruned`
  are `Error::invalid` with the sentences in Scope 9. The UI never needs them, because it
  renders from `TranscriptState`.
- **Three local commands for a transcript kept at home** (ADR-0036 point 5, and the D32
  appendix's "a local command that 056 adds"): `read_local_run_transcript_page`,
  `search_local_run_transcript` and `summarize_local_run_transcript`. They take the same
  arguments, find the file through `transcript_uploads.path`, and wrap it in a
  `FileTranscriptStore` over its parent. A run with no `transcript_uploads` row is
  `not_found`. They serve any run this runner holds a file for, so a connected desktop can
  also read an `incomplete` transcript whole.
- **Runner-side readers use `transcript_uploads.path`**: `reveal_run_log`,
  `get_run_log_size` and `prune_run_logs`. They fall back to 041's derived path for a run
  with no row, which is every run from before this task.
- `prune_run_logs` never deletes a file whose row has `upload = 1` and `completed_at IS
  NULL`. Those bytes exist nowhere else yet. The row is left in place, so a local read can
  report the file as gone.

### 5. The runner: uploads and the outbox (`crates/runner/src/outbox/`)

**`OutboxBoard { inner: Arc<dyn BoardPort>, store: RunnerStore, … }` implements
`BoardPort`** and wraps whichever adapter the host built. `rimaia-core` never learns about
`runner.db` (ADR-0027 point 6, 040's structural test). `src-tauri`'s `setup()` wraps its
in-process port with it, and the tests wrap 052's `HttpBoard`. Every path runs through it,
including solo, so the code the contract suite and the outbox tests exercise is the code
that ships. The cost in solo is one read of each transcript that the in-process body then
discards. That is accepted, because a solo-only shortcut would be the one branch no HTTP
test runs.

- **What queues.** Every lease-bound write: `record_branch`, `start_run`, `finish_run`,
  `release` and `record_strategy`. Each is written to `outbox` in the transaction that
  also updates `transcript_uploads` where one applies. `body` is the request as it will be
  sent, `LeaseRef` included. Transcript bytes are never copied in (D28).
- **Order is per lease, and leases do not wait for each other.** Entries for one
  `(task_id, generation)` are delivered in `id` order. A lease with a backlog does not hold
  up another lease's delivery.
- **Writes return when they are durable, except `finish_run`.** `start_run` and the other
  unit-returning writes return `Ok(())` once their row is committed, because their reply
  carries nothing. `finish_run` awaits delivery, because the runner needs `NextStep`. The
  in-flight slot and the worktree stay held while it waits, exactly as they do during a
  long run.
- **Reads wait for the lease's backlog.** `run_context` under a lease with undelivered
  entries awaits delivery first, then goes live. A read must see the writes made before it.
  `preview`, `claim`, `heartbeat`, `publish_tail`, `record_review_findings` and `run_tool`
  pass straight through: the first three hold no lease, the tail is never replayed (D14),
  and the last two are the agent's own synchronous calls (055).
- **Uploading follows the file.** `start_run` inserts the `transcript_uploads` row. `path`
  is the file `EventStream` writes, and `upload` is the runner setting in Scope 7, read
  once, at that moment. While the run is live, the worker checks the file's length every
  `UPLOAD_INTERVAL = 10 s` of the injected clock. From `acked_offset` it sends chunks of
  at most `TRANSCRIPT_CHUNK_BYTES = 1 MiB` until it catches up. The first chunk of a run
  goes out only after that run's `start_run` entry is delivered. Every answer's
  `stored_through` becomes `acked_offset`, including a smaller one: that is the rewind.
- **The end of a run.** `finish_run` records `final_offset`, the file's length after
  `Transcript::sync`, and replaces `FinishRun::transcript` with `Complete { length:
  final_offset }` or `KeptOnRunner`, from the row's `upload`. The runner owns the policy,
  because the setting lives in `runner.db`, which core cannot read. 057's postcondition
  rewrites the outcome on the same terms (D31 point 6). The `finish_run` entry is delivered
  only once `acked_offset == final_offset`, or at once when `upload = 0`. `completed_at` is
  set when the board acknowledges the finish.
- **When the board refuses.** A `finish_run` answered with the Scope 3 shortfall first
  probes with an empty chunk at `final_offset`. It then rewinds `acked_offset` to the
  answer, sends the missing bytes, and sends the finish again. For every entry:
  - `conflict` is D31 point 11's fenced reaction, through 053's function. The lease's
    backlog is deleted, the `transcript_uploads` row stays incomplete, and the local file
    is kept.
  - `not_found` deletes the lease's backlog. The task or its team is gone.
  - `upgrade_required` stops delivery for the whole runner and keeps every entry, because
    the runner is out of date (ADR-0037 point 4).
  - `invalid`, other than the shortfall, is handed to the awaiting caller, or logged at
    `error` if nobody awaits. The entry is then deleted, so one malformed report cannot
    wedge a lease forever.
  - Anything else is retried. Each retry increments `attempts`, records `last_error`, and
    backs off from 1 s, doubling to a cap of 60 s, on the injected clock. The backoff
    resets after any success.
- **After a restart.** The worker starts before 043's `reconcile_held`, and delivers the
  backlog of every held lease first. `reconcile_held` skips a lease while it still has
  queued entries, so a finish the runner already recorded is never replaced by an
  `interrupted` one. A reply that no caller awaits is handled by
  `outbox::settle_unawaited`: on `Released`, the `held_leases` row is deleted, and on a
  `usage_limit` class `pause::note_usage_limit` is raised from `resume_after`, as
  `run_task` does. A `Continue` leaves the lease exactly as a crash right after the reply
  would, and 043's reconcile owns that state (see Notes).

### 6. Redaction before write (ADR-0036 point 4)

- **`credentials::redact::secret_env_values(env) -> Vec<String>`**, a pure function over
  `(OsString, OsString)` pairs. It keeps a value when the variable's name ends in `_TOKEN`,
  `_SECRET`, `_KEY` or `_PASSWORD`. The comparison is ASCII case-insensitive: Windows
  environment names are case-insensitive, and `github_token` is as secret as
  `GITHUB_TOKEN`. Non-UTF-8 values are skipped, because they cannot occur in a UTF-8 line.
  `Redactor::for_values` still drops anything under 8 bytes.
- **The set is what the child actually receives:** the parent environment, minus the
  plan's `env_remove`, plus its `env_set`, after `with_repository_credentials`. `execute`
  builds one `Redactor` from three sources: the repository credential's values (task 022,
  unchanged), `secret_env_values` of that environment, and `RunnerConfig::host_secrets`.
  That redactor covers the transcript, the stderr log and the tail, in the one place task
  022 put it, as the first statement of `EventStream::observe`. Implementation runs and
  planner runs share `execute`, so both are covered.
- **`RunnerConfig::host_secrets: Redactor`** holds the Rimaia tokens the host process
  owns (ADR-0030): the runner's `rmr_` token, and a connected desktop's `rmd_` token. It
  is empty in solo. The runner crate fills it where it builds the config. `Redactor`'s
  hand-written `Debug` keeps `RunnerConfig`'s derived `Debug` from printing either.
- `Redactor` gains `merged(self, other) -> Redactor`. It keeps the longest-first order and
  the de-duplication.

### 7. A runner may keep transcripts at home (ADR-0036 point 5)

- **The runner setting `upload_transcripts`** is a `runner_settings` key read through a
  typed accessor in the runner crate (D3). Its values are `full` (the default, and what an
  absent key means) and `summaries_only`. There is a local command pair,
  `get_transcript_upload` and `set_transcript_upload`.
- **`summaries_only` changes one thing:** `transcript_uploads.upload = 0`, so no chunk is
  sent and the finish says `KeptOnRunner`. The row, the review bundle (033) and the tail
  (048, 052) still go.
- **`transcripts::UPLOAD_DISCLOSURE`**, one constant in `rimaia-core`. It is the
  plain-language paragraph ADR-0036 point 5 requires, written in ADR-0012's register. 058
  prints it at pairing and 059 shows it, so both say the same thing:

  > A run's transcript is everything the agent read and wrote: file contents, command
  > output, and sometimes values from your environment. Rimaia removes its own tokens and
  > any environment variable named like a secret before anything is written, but it cannot
  > recognise a secret the agent read out of a file. With full transcripts, teammates can
  > read them on the server. With summaries only, the transcript stays on this machine,
  > and the server gets the outcome, the diff and the commits.

- There is no Settings control for it in this task. In solo nothing leaves the machine,
  and pairing, where the choice is made, is 058's and 059's.

### 8. Retention on the server (ADR-0036 point 6)

- **The team setting `transcript_retention`**, in `team_settings` through a typed accessor
  (D3; team placement by exclusion, D28 part 4). Its value is a decimal number of days in
  `7..=3650`, or `until_task_deleted`. An absent key means `90`. The ceiling is arbitrary
  and stated: beyond ten years is "until deleted" in practice, and a bound keeps the date
  arithmetic from overflowing. Anything else is `invalid`:
  `transcript retention is a number of days from 7 to 3650, or until_task_deleted`.
- **`get_transcript_retention` (`Read`) and `set_transcript_retention` (`Write`)** are
  board rows. Setting is owner-only, through 051's role refusal (ADR-0029 point 3: owners
  change team settings). Each is an MCP tool with `RunAccess::Refused`, because ADR-0021
  point 1 makes a command without a tool a defect, and a run has no business changing
  retention.
- **`transcripts::retention::sweep(ctx, hold, now)`** runs only with `Server`. It is
  started by the server binary, at startup and then every hour through
  `Clock::sleep_until`. It never runs in the solo shell: in solo the "server's copy" is the
  runner's own file, and ADR-0013 keeps that until the task is deleted or the user prunes.
  For each team, under a context scoped to that team (039), it takes runs with
  `ended_at < now − retention`, `transcript_pruned_at IS NULL` and `transcript_key IS NOT
  NULL`, of every kind (D29 point 6), 500 at a time. For each batch it:
  1. calls `store.delete` for each key;
  2. then, in one transaction, sets `transcript_pruned_at = now` and
     `transcript_bytes = 0` on the runs, and `patch = NULL, patch_pruned_at = now` on their
     `review_bundles` rows. It deletes no row, and changes no other bundle column;
  3. publishes the run ids under the team.

  The file goes first. A failure between the two steps leaves a row that the next sweep
  prunes again, and deleting a missing key is not an error. The other order would leave a
  file that no row can find. `ended_at` rather than `started_at`, so a long run is kept for
  the whole period after it ended.
- **Deleting removes the server's files.** `delete_task`, and 051's shared deletion
  function for a team or an account, collect the affected keys in their transaction. After
  it commits, they call `store.delete` for each one, only with `Server`. A failed delete is
  logged at `error` with the key, which holds ids and no content (ADR-0037 point 6). In
  solo `delete_task` still leaves the runner's files, as it does today (ADR-0029 point 6:
  a runner's files are its owner's).

### 9. The interface

- `src/types.ts` mirrors `TranscriptState`. `TranscriptViewer.tsx` and
  `RunDetailOverlay.tsx` render from it. Exact copy:
  - `keptOnRunner`, on another client: `Transcript kept on <runnerLabel>. Only that machine
    can open it.` On the runner that kept it (049's `localRunnerId` equals `runnerId`), the
    viewer reads through the three local commands and shows no notice.
  - `pruned`: `Transcript removed on <date> by the team's retention setting. The outcome,
    diff and commits are kept.`
  - `incomplete` on a run that has ended: `The server holds part of this transcript. The
    rest is on <runnerLabel>.` The lines that are held are shown below it. On the runner
    that holds the rest, the local read is used instead.
  - `incomplete` on a running run: no notice. The live tail is the view.
- `StorageSection.tsx` gains a retention control, shown only when
  `getClientCapabilities()` reports a server (049). It offers 7, 30, 90, 180 and 365 days
  and "Until the task is deleted", and it is disabled with the reason `Only a team owner
  can change this.` for a member. The solo desktop does not show it.
- The two `invalid` sentences from Scope 4, word for word: `this run's transcript is kept
  on <runnerLabel>` and `this run's transcript was removed on <date> by the team's
  retention setting`.

## Out of scope

- **Storage per team, and the metrics that report it** (ADR-0036 consequences, ADR-0037
  point 7). Those belong to 062, which reads `SUM(transcript_bytes)` per team.
- **Backing up transcripts.** Litestream replicates the database only. How transcript
  files are backed up and restored is 062's runbook.
- **Object storage.** ADR-0036 point 7 keeps files on disk, and D34 approves no client for
  anything else.
- **The pairing screens and the headless flag** that present `UPLOAD_DISCLOSURE` and set
  `upload_transcripts`. 058 and 059 own them.
- **Redacting the review bundle's patch.** The patch is the work under review. A secret
  committed to the branch is on the forge already, and rewriting it would show the reviewer
  a diff that is not the branch.
- **Redacting the run-scoped `rimaia-run` handle's token** (D30). ADR-0036 point 4 names
  ADR-0030's tokens. The handle is checked against the live lease on every call (055), so
  it is worth nothing once the run ends.
- **Sweeping orphaned files**, meaning a server file whose row is gone because the process
  died between commit and unlink. The store has no list operation, and adding one would
  widen ADR-0036 point 7's interface for a rare window. The failure is logged with the key.
- **Changing the fence for transcript bytes.** See Notes.
- **Any change to `EventStream`'s parsing, `Transcript`'s write discipline, or the tail's
  shape.**

## Acceptance criteria

**Schema and caches**

- `20261003120600_transcripts_and_retention.sql` and
  `crates/runner/migrations/20261003130300_outbox.sql` exist with D28 part 6's DDL, and no
  other migration was added in either set. `no_migration_opts_out_of_its_transaction`
  passes over both sets.
- `crates/core/.sqlx/` and `crates/runner/.sqlx/` were regenerated with D33 point 3's
  recipe. No `.sqlx/` exists at the workspace root.
- `existing_runs_are_keyed_by_their_runner_file_layout`: a file board built at the
  pre-056 schema (the D28 part 7 technique) with one ended run and one running run
  migrates. Both get `<task_id>/<run_id>.jsonl`, the ended run gets
  `transcript_complete_at = ended_at`, and the running one gets `NULL`.

**The store**, in `crates/core/src/transcripts/store.rs`, over a `TempDir`:

- `appending_at_the_end_extends_the_transcript`;
- `appending_past_the_end_is_refused_and_writes_nothing`;
- `appending_inside_the_transcript_replaces_everything_after_the_offset`;
- `a_range_read_returns_exactly_those_bytes_clipped_to_the_length`;
- `deleting_a_missing_key_is_not_an_error`;
- `a_key_cannot_name_a_path_outside_the_store`, which covers `..`, an absolute path, a
  backslash, an empty segment and a missing `.jsonl`.

**The board**, through `board::service` with each hold, and through both adapters where a
case is marked *contract*. *Contract* cases go in `testing/board_contract.rs`, so they run
in process (036) and over HTTP (052):

- `the_server_keys_a_transcript_by_team_and_run` and
  `solo_keys_a_transcript_by_its_runner_file`.
- `a_chunk_that_leaves_a_gap_writes_nothing_and_answers_what_is_stored` (Server).
- `an_overlapping_chunk_writes_only_its_new_tail` (Server).
- `new_bytes_under_a_stale_generation_are_conflict` (Server).
- `a_resent_chunk_is_acknowledged_without_writing_twice` (*contract*).
- `a_resent_start_run_is_acknowledged_once` (*contract*).
- `a_resent_finish_gets_the_first_answer` (*contract*), with one case for `Released` and
  one for `Continue { Review }` under a loop that is on (021).
- `a_resend_after_the_lease_moved_on_is_acknowledged_not_fenced` (*contract*).
  `every_lease_method_refuses_a_stale_generation` still passes unchanged.
- `a_finish_marks_the_transcript_complete_only_when_every_byte_is_held` and
  `a_finish_short_of_its_bytes_is_invalid_and_changes_nothing`: after the refusal the run
  is still `running` and the task has not moved.
- `kept_on_runner_is_recorded_and_held_bytes_are_deleted`.
- `solo_marks_a_finished_transcript_complete_without_copying_a_byte`: `runs_dir()` holds
  exactly the one file the run wrote.
- `get_run` reports each of the five `TranscriptState` variants.

**The reads**

- The three transcript reads are `board` `Read` rows served through `dispatch`, flipped in
  one commit, and `./scripts/check-command-wiring.sh` passes.
- Every test in `crates/core/src/runs/transcript.rs` keeps its assertions. Only the way
  its input is built changes, to a `FileTranscriptStore` over a `TempDir`.
- `an_incomplete_transcript_is_read_up_to_its_last_whole_line`.
- `another_teams_transcript_is_not_found`, for each of the three reads (039's two-team
  fixture).
- `a_kept_transcript_is_refused_by_the_board_and_read_by_its_own_runner`, with the exact
  sentence.
- `the_runner_never_prunes_a_transcript_the_server_has_not_acknowledged`.

**The runner**, in `crates/runner/tests/outbox.rs`: real `runner.db` files in a `TempDir`,
real transcript files, a `TestClock`, and no `sleep`. Server outages are made by a
test-only `BoardPort` wrapper that fails with an `internal` error while a switch is off. It
wraps the real in-process or HTTP adapter and is not a mock of either.

- `a_run_that_finishes_offline_reaches_the_board_complete_when_the_server_returns`: the
  switch goes off mid-run and back on before the lease expires. The server's copy is then
  byte-identical to the local file, `transcript_complete_at` is set, and the task has
  landed.
- `chunks_resume_from_the_acknowledged_offset_after_a_restart`: the process is dropped
  with half the file acknowledged and reopened over the same `runner.db`. No byte is sent
  twice.
- `a_gap_answer_rewinds_the_upload`.
- `finish_is_sent_only_after_every_byte_is_acknowledged`.
- `one_leases_backlog_does_not_hold_up_another`.
- `a_read_under_a_lease_waits_for_that_leases_backlog`.
- `a_conflict_drops_that_leases_backlog_and_keeps_the_local_file`.
- `upgrade_required_stops_delivery_and_keeps_the_backlog`.
- `retries_back_off_on_the_injected_clock`: the delivery attempts land at 1, 2, 4 … 60,
  60 s.
- `queued_reports_are_delivered_before_reconcile_touches_their_lease`: a queued
  `finish_run` wins over 043's interrupted report.
- `a_released_reply_nobody_awaits_clears_the_held_lease`.
- `solo_leaves_an_empty_outbox_after_a_run`.
- `a_summaries_only_runner_sends_the_row_the_bundle_and_the_tail_and_no_bytes`.

**Redaction**

- `secret_env_values_match_names_by_suffix_case_insensitively`, which also covers a
  short value, a non-UTF-8 value, and names that merely contain `KEY` (`KEYBOARD`,
  `MONKEY_BUSINESS`), which are kept out.
- `a_secret_in_the_childs_environment_is_redacted_everywhere_a_run_writes`: a real child
  prints the value of a `*_API_KEY` variable its plan sets, and the transcript, the stderr
  log and the published tail all carry `[redacted]` instead. The test never calls
  `std::env::set_var`, which races every other test in the binary.
- `the_runner_token_is_redacted_from_everything_a_run_writes`: the script prints a fixed
  string, and the test sets that string as `host_secrets`.
- `a_remote_runs_transcript_and_tail_arrive_redacted`, in `crates/runner/tests/`: a real
  run through `OutboxBoard` over `HttpBoard` against the real server router (052's
  harness). The server's stored transcript and a 048 subscriber's `runs:tail` both contain
  `[redacted]` and never the sentinel.
- `crates/core/tests/runner_credentials.rs` passes with no assertion changed.

**Retention and deletion**

- The accessor's cases: absent is 90, and `7`, `3650` and `until_task_deleted` are
  accepted. `6`, `3651`, `-1`, `90d` and `""` are refused with the exact sentence.
- `a_member_cannot_change_retention`: an `invalid` from 051's refusal, through the command
  and through the MCP tool.
- `every_registered_tool_has_a_run_scope_decision` passes with the two new tools
  `Refused`.
- `retention_deletes_the_transcript_and_the_patch_and_keeps_the_row_and_the_bundle_summary`:
  every capture column (D18) and `files_changed`, `insertions`, `deletions`, `files` and
  `commits` are unchanged.
- `retention_takes_every_run_kind`.
- `retention_never_touches_a_run_that_has_not_ended`.
- `until_task_deleted_never_prunes`.
- `retention_is_counted_from_when_a_run_ended`.
- `solo_never_prunes_by_retention`: the solo shell starts no sweep, and a solo board's
  90-day-old run keeps its file.
- `the_sweep_runs_hourly_on_the_injected_clock`.
- `a_sweep_interrupted_after_the_delete_prunes_the_row_next_time`.
- `deleting_a_task_deletes_its_transcripts_from_the_server` and
  `deleting_a_team_deletes_its_transcripts`. Solo `delete_task` leaves the runner's files,
  and an existing test pins that or a new one does.

**Interface**

- Vitest cases for each `TranscriptState` notice, with the exact strings, and for the
  switch to the local commands when `localRunnerId` matches.
- The retention control renders for a server client, is absent for the solo desktop, and
  is disabled with its reason for a member.
- The 31 test files that mock `@tauri-apps/api/core` still pass. The ones that assert
  `invoke` for the three transcript reads were updated only as far as the flip requires.

**Every command in CLAUDE.md passes**, with `SQLX_OFFLINE=true` exported, exactly as
`ci.yml` runs them, `cargo test -p rimaia-runner` and the runner's clippy line included.

## Notes

**Read first.** ADR-0036 in full. ADR-0013 and its amendment (the file stays, and a missing
file is noticed on read). ADR-0022 point 2 (rows are kept, and transcripts are a cache).
ADR-0020 point 7 (redaction before write). Also ADR-0028 point 2 (placement: retention is a
team setting, the upload choice is a runner setting), ADR-0029 point 6 (deletion),
ADR-0031 points 3 to 5 (fencing and reconcile), ADR-0034 point 2 (the raw-body route) and
ADR-0037 points 4 and 6. Seam entries:

- **D28**: part 6's two files, the backfill, and part 4's placement by exclusion. Its D4
  amendment for the names and the freeze rule.
- **D31**: point 4 (`append_transcript`, `publish_tail`, `finish_run`), point 7's
  transcript row, point 10 (the raw-body route, and "the adapter does not retry"), point 11
  (one reaction to `Conflict`), point 12 (resends), and point 13 (the suite, and its "056:
  resends").
- **D32**: point 2 (`BoardHost` grows here), point 8 (the flip), and the appendix rows for
  the three reads, `reveal_run_log`, `get_run_log_size` and `prune_run_logs`.
- **D33**: point 3's recipe, for both caches.
- **D29** point 6: retention and pruning take every kind.
- **D25** point 5 and **D18**: what is redacted, and that capture columns survive
  pruning.
- **D14**: the tail is never replayed and never the source of truth.
- **D17** point 5: a planner transcript has no row and stays on the runner.
- **D34**: no object-storage client, no `regex` for redaction, and the body limit is raised
  on `append_transcript` alone. A 1 MiB chunk fits under axum's default, so this task sets
  no limit.
- **D3**, **D8** and **D10**: the typed accessors, no new error code, and string ids.

**Files to start from** (on `main` today):

- `crates/core/src/runner/events.rs`: `Transcript`, `transcript_path`, `EventStream::observe`
  and `redacting`.
- `crates/core/src/runner/process.rs`: `execute`, `repository_credentials`,
  `with_repository_credentials` and `spawn`. 041 and 042 may have moved some of these. Follow
  the names.
- `crates/core/src/credentials/{redact,inject}.rs`.
- `crates/core/src/runs/{mod,transcript}.rs`: `prune_logs`, `get_run`, `log_available`,
  `read_page`, `search` and `summarize`.
- `crates/core/src/paths.rs`: `runs_dir`.
- `crates/core/tests/runner_credentials.rs`: the script that prints its own token. Copy
  the technique.
- `crates/core/src/testing/cli.rs`: `write_script` and `replays`.
- `src-tauri/src/commands/runs.rs`: the three reads, the reveal, the size and the prune.
- `src/components/runs/{TranscriptViewer,RunDetailOverlay}.tsx` and
  `src/views/settings/StorageSection.tsx`, with their tests.

Created by earlier tasks on this branch:

- `crates/core/src/board/{port,types,service,in_process}.rs` and
  `crates/core/src/testing/board_contract.rs` (036, 043);
- `crates/runner/` and its `RunnerStore` (040, 041), and `held_leases` with
  `reconcile_held` (043);
- `crates/core/src/api/{mod,registry}.rs` and the server binary (046);
- `crates/core/src/api/tail.rs` and the SSE subscription (048);
- `getClientCapabilities` and `localRunnerId` (049);
- the role refusal and the shared deletion function in `crates/core/src/teams/` (051);
- `crates/runner/src/board/http.rs` and `crates/server/src/runner_api.rs` (052);
- the fenced-reaction function and the HTTP test harness (053).

**Migrations.** `src-tauri/migrations/20261003120600_transcripts_and_retention.sql` and
`crates/runner/migrations/20261003130300_outbox.sql`, both reserved by D28's D4
amendment. They are the last file in each set. A column this task finds missing is a
stop-and-ask, never an edit to an earlier file.

**What the chain provides.**

- **033:** `review_bundles`, including `patch_pruned_at` and the reader's pruned state.
  This task is the first writer of `patch_pruned_at`.
- **036:** the port, `TranscriptEnd`, the in-process body, and `run_task` reading `length`
  off the file.
- **038/039:** `team_id` on every row, and contexts scoped per team.
- **040/041:** `runner.db`, `runner_settings`, and the derived log path this task
  replaces.
- **043:** `current()`, generations, `held_leases` and `reconcile_held`.
- **048/052:** the tail relay end to end. 048's Notes say this task adds nothing to the
  stream, and it does not. It adds only the redacted end-to-end test.
- **051:** `delete_team`'s shared function. 051's Notes hand this task its extension to
  transcript files.
- **053:** expiry, and the one fenced-reaction function.

**What the next tasks expect.**

- **057** rewrites the outcome before `OutboxBoard::finish_run`, and relies on a fenced
  runner's backlog already being dropped.
- **058** prints `UPLOAD_DISCLOSURE` and sets `upload_transcripts` at pairing, fills
  `host_secrets` with its `rmr_` token, and wraps `HttpBoard` in `OutboxBoard`.
- **059** does the same for a connected desktop, with both its `rmr_` and `rmd_` tokens.
- **061** may show `transcript` states elsewhere on the card, from the same DTO.
- **062** reports storage per team from `transcript_bytes`, backs up `transcripts/`, and
  starts the sweep in the container exactly as the binary does here.
- **065** drops `runs.log_path`, which nothing reads or writes after this task.

**A known limit, recorded and not solved: bytes that arrive after their lease expired.**
ADR-0031 point 3 fences every report by generation, and it names transcript chunks. D31
point 13's `every_lease_method_refuses_a_stale_generation` covers `append_transcript`. So a
laptop offline for longer than a lease's lifetime comes back to a run the server has
already closed as `interrupted` (053). Its new bytes and its finish are `conflict`, and the
rest of that attempt's transcript stays on the laptop. The board says so (`incomplete`:
"The rest is on <runnerLabel>"), and the owner can still read it locally. The work is not
lost: the worktree and the session survive, and the pinned re-claim resumes them. ADR-0036's
"a run that finished while a laptop was offline appears … complete" therefore holds for
outages shorter than the lease. **Do not accept stale-generation bytes to close this
gap.** That would contradict an accepted ADR and a seam entry. If it matters, it needs an
ADR-0036 amendment and a D31 amendment, and the PR body names it as a follow-up. Fencing
bytes by the run rather than by the lease is the likely shape. The run's `runner_id`
already identifies the only writer.

**Another state this task inherits and does not change.** A `finish_run` answered with
`Continue`, followed by a crash before the next phase's `start_run`, leaves a held lease
whose run is closed. That already happens today, with no outbox, if the process dies right
after the reply, and 043's `reconcile_held` owns it. The outbox makes the state reachable
by one more route, a restart before the queued finish is delivered, and it hands the lease
to the same reconcile. It adds no rule of its own.

**Size, and where to cut.** Honestly estimated, this is about 4,000 changed lines before
the regenerated caches:

- the store and the key: about 350;
- the board side and resends: about 500;
- the reads flip and the local commands: about 400;
- the outbox and the uploader: about 900;
- redaction: about 150;
- retention, deletion and the two tools: about 450;
- the interface: about 300;
- the rest is tests.

That is at the ceiling of one session. If it runs long, cut along Scope 8. Move the
retention setting, its commands and tools, the sweep, the retention control in
`StorageSection.tsx` and their tests to a new task, **066 Transcript retention on the
server**, placed directly after 056 in `tasks/README.md` and depending on it. Keep
everything else here, including the migration's `transcript_pruned_at` column and the
`pruned` state in the reader: the file is frozen once this task lands. Keep deletion here
too (Scope 8's last point), because ADR-0029 point 6 applies from the first byte a server
holds. **Never cut** the resend rules, the gap rewind, or the redaction. Without the first
two, a flaky network duplicates or loses bytes. Without the third, the first upload sends
secrets to a server.
