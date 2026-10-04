---
id: "056"
title: Transcripts leave the machine
milestone: v0.5
status: ready
depends_on: ["053", "054", "055", "066"]
adrs: ["0036", "0013", "0022", "0020", "0030", "0031"]
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
- deleting a task, a team or an account deletes the server's copies.

Solo mode is unchanged for the user. Its board reads the runner's own file through the same
storage interface, and nothing is copied. Retention on the server is task 068.

## Why now

053 made leases real across the network: a remote runner can claim, heartbeat, report and
finish. What it cannot do yet is let anyone see what the run did. The row and the review
bundle reach the server with `finish_run` (033, 052), but the transcript stays on the
runner's disk. A reviewer on another machine sees an outcome and a diff with no way to ask
why a run failed, which is the question transcripts exist to answer (ADR-0036 Alternatives).
D31 point 7 lists `append_transcript` as the one method with no production caller "until
056", and 057, 058 and 059 all ship runs whose reviewer is someone else.

## Scope

### 1. The migrations, exactly as D28 writes them

- `src-tauri/migrations/20261003120600_transcripts_and_retention.sql`: the five `runs`
  columns (`transcript_key`, `transcript_bytes`, `transcript_complete_at`,
  `transcript_pruned_at`, `transcript_kept_on_runner`) and the two backfilling `UPDATE`s.
- `crates/runner/migrations/20261003130300_outbox.sql`: `outbox`, `idx_outbox_run` and
  `transcript_uploads`.

The header comment of each file is this task's, in the voice of the existing migrations. The
DDL is D28 part 6's, byte for byte. No other file. 068 adds no migration: the
`transcript_pruned_at` column it writes is in this file. Regenerate both offline caches with
D33 point 3's recipe and commit them.

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
      /// Exactly the bytes in `range`, clipped to the stored length. A key that names
      /// nothing is `Error::not_found`.
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
  second bookkeeping is needed, provided appends to one run are serialised (Scope 3).

- **`TranscriptKey`**, a newtype that parses rather than trusts. A key is one or two
  segments joined by `/`, where the last segment ends in `.jsonl` and each segment matches
  `[A-Za-z0-9-]+` (plus the extension on the last). Anything else, including `..`, an
  absolute path, a backslash or an empty segment, is `Error::invalid`. The file store
  therefore cannot be made to touch a path outside its root, whatever a row says.

- **`FileTranscriptStore { root }`**, the only implementation. It resolves a key to
  `root.join(key)` and creates parent directories on `append`.

- **Two key schemes, one per host, and never a fifth store method.** The server mints
  `<team_id>/<run_id>.jsonl` under `<RIMAIA_DATA_DIR>/transcripts/` (ADR-0036 point 7). The
  solo board mints `<task_id>/<run_id>.jsonl` under `AppPaths::runs_dir()`, which is
  ADR-0013's layout and what the migration's backfill wrote (D28). Minting a key is a
  function of where the board lives, not of the storage, so it sits beside the store:

  ```rust
  pub enum TranscriptHold {
      /// The runner's own files are the board's copy (ADR-0028 point 4, D31 point 4).
      Solo(Arc<dyn TranscriptStore>),
      /// The server holds its own copy, uploaded by runners.
      Server(Arc<dyn TranscriptStore>),
  }
  ```

  `BoardHost` gains `transcripts: TranscriptHold`, and so does 036's `InProcessBoard`.
  `src-tauri/src/lib.rs` builds `Solo`, the server binary builds `Server`, and
  `testing::context` builds either.

- **`TRANSCRIPT_CHUNK_BYTES = 1 MiB`** lives here, because the runner sends chunks of that
  size and the server sizes its body limit from it (Scope 3).

### 3. The board side

In `crates/core/src/board/service.rs`, behind both adapters, with either hold.

**Ownership comes first.** For `append_transcript` and `finish_run`, before any other step
(the resend checks included), the service resolves `run_id` to a run whose `task_id` is
`lease.task_id`, under the caller's team scope. Anything else is `Error::not_found`. This is
the first method that writes server bytes keyed by a client-supplied id, and 043's `current`
fences on the lease's task, generation and runner only. Without this check, a runner holding
any lease in a team could append to another member's live transcript, read its
`stored_through`, or have a finish acknowledged for a run it never started. `start_run`
applies the same rule to an id that already exists under another task.

**A resend skips the fence only when the board holds that exact fact** (D31 point 12, as
amended in Scope 9). A report that carries anything the board has not stored always reaches
043's fence. That is what keeps `every_lease_method_refuses_a_stale_generation` passing
unchanged: each of its reports carries content the board does not hold.

- **`start_run`** writes `transcript_key` from the hold's scheme in the insert, and
  `transcript_bytes = 0` with `Server` (`NULL` with `Solo`, until the finish). It stops
  writing `runs.log_path` (066 stopped reading it; 065 drops it), so the insert loses 066's
  `-- runs.log_path written until 056` marker and `no_board_query_reads_a_retired_column`
  loses that exemption. A resend is acknowledged, and writes nothing, when a row with that
  `run_id` exists for the lease's task with the same `runner_id`, `kind` and `session_id`.

- **`append_transcript`**, with `Server`, inside one **`BEGIN IMMEDIATE`** transaction, as
  043's lease transactions are, so two appends for one run are serialised around reading
  `transcript_bytes` as `held`, writing the file and committing. Under a deferred
  transaction both read the same `held`, the later writer truncates the file to its shorter
  length and then fails to commit with `BUSY`, and the row claims bytes the file lacks:
  every later chunk is refused. 053's request timeouts make this ordinary, because the
  outbox resends while the first request is still in flight.
  1. A non-empty chunk with `offset + len <= held` is already stored. Nothing is written,
     and the answer is `stored_through: held`, before the fence.
  2. The fence (043's `current`) runs. The lease's `run_id` must equal the chunk's;
     otherwise `Error::invalid` (`this lease is not running that run`).
  3. `offset > held` is a gap. Nothing is written, and the answer is `held`, so the runner
     rewinds. An empty chunk lands here or in step 5, so it is a probe that answers `held`
     and is never a resend.
  4. A chunk that would extend a transcript whose `transcript_complete_at` is set is
     `Error::invalid`.
  5. The store writes `bytes[held - offset..]` at `held`, and `transcript_bytes` becomes
     `offset + len`.

  With `Solo`, after the ownership check, 036's body stands: acknowledge through
  `offset + len` and copy nothing.

  The server's `append_transcript` route sets axum's `DefaultBodyLimit` explicitly, to
  `TRANSCRIPT_CHUNK_BYTES` plus 64 KiB, on that route alone (D34, and 052's handoff).
  Today's default would admit a 1 MiB chunk, but a default is not a contract, and the two
  numbers would drift apart silently.

- **`finish_run`** reads `FinishRun::transcript`:
  - `Complete { length }` with `Server`: if `transcript_bytes == length`, it calls
    `mark_complete` and sets `transcript_complete_at` in the transaction that closes the
    row. Any other count is `Error::invalid` naming both numbers (`the server holds <held>
    of this run's <length> transcript bytes`), and nothing is applied: no row closed, no
    task landed.
  - `Complete { length }` with `Solo`: `transcript_bytes = length` and
    `transcript_complete_at = now`. The runner's file is the board's copy.
  - `KeptOnRunner`: `transcript_kept_on_runner = 1`. With `Server`, any bytes already held
    are deleted and `transcript_bytes` becomes `0`: the owner's choice wins over bytes that
    arrived first.

  **A resent finish gets the first answer, rebuilt from the board and never from a stored
  reply.** A closed row is a resend only when its closing facts match the report: the same
  `runner_id`, the same `status` and `exit_class` as the report's outcome, and the same
  `head_sha` where the report sets one. The answer is then the row as it now reads, plus:
  - `Continue { kind }`, if a lease with the report's generation is still held for the task
    and its `run_id` is this run. `kind` follows 021's table from the closed row's kind:
    implementation → review, review → fix, fix → review.
  - `Released { resume_after: row.resume_after }` otherwise.

  Any other closed row is `Conflict`. In particular, a row that 053's expiry closed as
  `interrupted` does not acknowledge a laptop's late `succeeded` finish. That report is
  fenced, so `on_fenced` runs and the local file is kept (Notes). On the write path, the
  lease's `run_id` must equal the finish's, as for chunks. 036's "has already been
  finalized" refusal is deleted.

- **`get_run` and `list_runs*`** gain `transcript: TranscriptState`, which says what a
  reader will find. It is a tagged union with `camelCase` fields, decided in this order:
  1. `keptOnRunner { runnerId, runnerLabel }`, when `transcript_kept_on_runner` is set;
  2. `pruned { at }`, when `transcript_pruned_at` is set (068 writes it);
  3. `complete` or `incomplete { bytesHeld, runnerId, runnerLabel }`, when a key is set;
  4. `none`, when no key is.

  `keptOnRunner` comes before `pruned` because retention removes the server's copy, and a
  kept transcript never had one: the runner still holds it, and saying "removed" would be
  false. `runnerLabel` is `runners.label`. `runnerId` and `runnerLabel` are nullable,
  because `runs.runner_id` is `ON DELETE SET NULL` and an account deletion removes the
  runner. With `Solo`, `bytesHeld` is the file's length, and `complete` and `incomplete`
  are checked with `try_exists` on read, which is the ADR-0013 amendment's rule. A missing
  file is `none`. `logAvailable` stays, and means "a board read will return lines".

### 4. The transcript reads flip to the board (D32 point 8)

In one commit: the registry rows for `read_run_transcript_page`, `search_run_transcript`
and `summarize_run_transcript` become `board` `Read`, their handlers move into
`api/board/`, their wrappers switch to `board<T>`, and `check-command-wiring.sh` passes.

- `runs::transcript::{read_page, search, summarize}` stop taking a `&Path`. They read
  through `read_range` only, in windows of `TRANSCRIPT_READ_WINDOW = 1 MiB`, over
  `(&dyn TranscriptStore, &TranscriptKey)`, and take an explicit `complete: bool`. Their
  parsing, limits and output types do not change. With `complete: false`, an unterminated
  last line is not yielded, because an upload can cut a line in two. With `complete: true`,
  a truncated last line is still yielded as `TranscriptEntry::Malformed`, which is how a
  reader shows a cut stream and how `summarize` detects one.
- The handler reads the run under the caller's scope (a run in another team is
  `not_found`, ADR-0029 point 5) and answers from the hold. `keptOnRunner` is
  `Error::invalid` (`this run's transcript is kept on <runnerLabel>`), and `pruned` is
  `Error::invalid` (`this run's transcript was removed on <date> by the team's retention
  setting`). The UI never needs either sentence, because it renders from
  `TranscriptState`.
- **Three local commands for a transcript kept at home** (ADR-0036 point 5, and the D32
  appendix's "a local command that 056 adds"): `read_local_run_transcript_page`,
  `search_local_run_transcript` and `summarize_local_run_transcript`. They take the same
  arguments, find the file through `transcript_uploads.path`, and wrap it in a
  `FileTranscriptStore` over its parent. A run with no `transcript_uploads` row is
  `not_found`. They serve any run this runner holds a file for, so a connected desktop can
  also read an `incomplete` transcript whole.
- **Runner-side readers use `transcript_uploads.path`**: `reveal_run_log`,
  `get_run_log_size`, `get_run_log_path`, `prune_run_logs` and `startup::survey`'s
  missing-transcript check. Each falls back to 066's derived path for a run with no row,
  which is every run from before this task.
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

- **Only the runner's own reports queue.** `record_branch`, `start_run`, `finish_run`,
  `release`, and `record_strategy` from `record_failure` and `stamp_run_metadata`. Each is
  written to `outbox` in the transaction that also updates `transcript_uploads` where one
  applies. `body` is the request as it will be sent, `LeaseRef` included. Transcript bytes
  are never copied in (D28).
- **Calls made on the agent's behalf never queue.** `OutboxBoard::for_agent()` returns a
  second `Arc<dyn BoardPort>` over the same inner port and backlog. Each lease-bound method
  on it awaits the lease's backlog, then calls the inner port live and returns the board's
  own answer. `src-tauri` hands it to `mcp::build`, and 058 to `run_proxy::bind`. This
  keeps 055's contract (the proxy's `record_strategy` returns the board's `invalid`, and
  its follow-up `get_task` sees the write) and the order: after an outage, a review run's
  findings cannot overtake the queued `start_run` their `review_run_id` points at.
- **Order is per lease, and leases do not wait for each other.** Entries for one
  `(task_id, generation)` are delivered in `id` order. A lease with a backlog does not hold
  up another lease's delivery.
- **Writes return when they are durable, except `finish_run`.** `start_run` and the other
  unit-returning writes return `Ok(())` once their row is committed, because their reply
  carries nothing. `finish_run` awaits delivery, because the runner needs `NextStep`. The
  in-flight slot and the worktree stay held while it waits, exactly as they do during a
  long run.
- **Reads wait for the lease's backlog.** `run_context` under a lease with undelivered
  entries awaits delivery, then goes live. `preview`, `claim`, `heartbeat` and
  `publish_tail` pass straight through: the first three hold no lease, and the tail is
  never replayed (D14).
- **Uploading follows the file.** `start_run` inserts the `transcript_uploads` row. `path`
  is the file `EventStream` writes, and `upload` is the runner setting in Scope 7, read
  once, at that moment. While the run is live, the worker checks the file's length every
  `UPLOAD_INTERVAL = 10 s` of the injected clock. From `acked_offset` it sends chunks of at
  most `TRANSCRIPT_CHUNK_BYTES` until it catches up. The first chunk of a run goes out only
  after that run's `start_run` entry is delivered, and only while the lease is in
  `held_leases`. Every answer's `stored_through` becomes `acked_offset`, including a
  smaller one: that is the rewind.
- **The end of a run.** `finish_run` records `final_offset`, the file's length after
  `Transcript::sync`, and replaces `FinishRun::transcript` with `Complete { length:
  final_offset }` or `KeptOnRunner`, from the row's `upload`. The runner owns the policy,
  because the setting lives in `runner.db`, which core cannot read. 057's postcondition
  rewrites the outcome on the same terms (D31 point 6). The `finish_run` entry is delivered
  only once `acked_offset == final_offset`, or at once when `upload = 0`.
- **`completed_at` means the runner is done with the file.** It is set when the board
  acknowledges the finish, or when the board can no longer take the bytes (`not_found`, or
  a dropped `start_run`). Either way the file becomes prunable. D28's DDL is frozen, so this
  task adds no separate marker.
- **When the board refuses.**
  - **A short finish.** On any `invalid` from `finish_run` with `upload = 1`, the runner
    probes once with an empty chunk at `final_offset`. If `stored_through < final_offset`,
    it rewinds, sends the missing bytes and sends the finish again. Otherwise the `invalid`
    is handed to the caller. No error message is matched: D8 adds no code, and the probe
    answers the only question that matters.
  - `conflict` is D31 point 11's fenced reaction, through 053's `fence::on_fenced`. The
    `transcript_uploads` row keeps `completed_at` `NULL`, so the local file is kept.
  - `not_found`: the task or its team is gone. The `held_leases` row is deleted, and
    `completed_at` is set.
  - `upgrade_required` stops delivery for the whole runner and keeps every entry, because
    the runner is out of date (ADR-0037 point 4).
  - `invalid` drops that entry, so one malformed report cannot wedge a lease forever. A
    dropped `start_run` takes the lease's backlog with it, because every later entry for
    that run depends on its row, and sets `completed_at`.
  - Anything else is retried. Each retry increments `attempts`, records `last_error`, and
    backs off from 1 s, doubling to a cap of 60 s, on the injected clock. The backoff
    resets after any success.

  **Every drop resolves its waiters.** `conflict` and `not_found` drop the lease's whole
  backlog. Whatever is dropped, for whatever reason, resolves each caller awaiting it (a
  `finish_run`, or a read waiting on the backlog) with that error. It is logged at `error`
  only if nobody awaits. A caller never waits on an entry that no longer exists.
- **After a restart.** The worker starts before 043's `reconcile_held` and delivers the
  backlog of every held lease first. `reconcile_held` skips a lease while it still has
  queued entries, so a finish the runner already recorded is never replaced by an
  `interrupted` one. Its own interrupted finishes are queued and not awaited, so an
  offline start does not block the host. A reply that no caller awaits is handled by
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
- **A transcript line is JSON, so a value is also hidden in its escaped form.**
  `Redactor::for_values` also holds each value's JSON-escaped form
  (`serde_json::to_string(v)` without the surrounding quotes) when it differs. A password
  containing `"`, `\` or a control character otherwise appears escaped in the line and is
  never matched.
- **The set is what the child actually receives:** the parent environment, minus the
  plan's `env_remove`, plus its `env_set`, after `with_repository_credentials`. `execute`
  builds one `Redactor` from three sources: the repository credential's values (task 022,
  unchanged), `secret_env_values` of that environment, and `RunnerConfig::host_secrets`.
  That redactor covers the transcript, the stderr log and the tail, in the one place task
  022 put it, as the first statement of `EventStream::observe`. Implementation runs and
  planner runs share `execute`, so both are covered.
- **`RunnerConfig::host_secrets: Redactor`** holds the Rimaia tokens the host process owns
  (ADR-0030): the runner's `rmr_` token, and a connected desktop's `rmd_` token. It is
  empty in solo. `Redactor`'s hand-written `Debug` keeps `RunnerConfig`'s derived `Debug`
  from printing either.
- **The two token keys are 058's and 059's, added here because this task reads them
  first.** Keychain accounts are named once and every host reuses them (ADR-0030 point 4),
  so this task invents no spelling. It adds two variants to 054's
  `credentials::CredentialKey` in `rimaia-core`, with their arms in
  `CredentialKey::account()`, exactly as the later tasks define them:
  - `CredentialKey::RunnerToken { runner_id }`, account `runner-token:<runner_id>` (058
    Scope 4);
  - `CredentialKey::DesktopToken { runner_id }`, account `desktop-token:<runner_id>` (059
    Scope 4).

  Neither account contains a `/`, so neither can equal 054's `<repository_id>/<runner_id>`.
  Nothing else lands with them: 058 still adds `credentials::runner_token` and
  `save_runner_token` over the first, and 059 adds `LoopbackMcpToken` and its own writes.
  Both find these variants in place, and their account-string tests pass unchanged. No
  helper in the runner crate builds an account string.
- **This task fills it.** `rimaia_runner::secrets::host_secrets(&RunnerStore, &dyn
  CredentialStore) -> Result<Redactor>` reads `runner_identity`. With no `server_url` it
  returns an empty redactor. With one, it reads
  `CredentialStore::get(&CredentialKey::RunnerToken { runner_id })` and, if present,
  `CredentialStore::get(&CredentialKey::DesktopToken { runner_id })`, where `runner_id` is
  `runner_identity.runner_id`. So it reads the same items 058 and 059 write. An unavailable
  keychain is an error, never an empty redactor: a connected runner that ran without its own
  tokens redacted is the failure this exists to prevent.
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
  plain-language paragraph ADR-0036 point 5 requires, written in ADR-0012's register, so
  058's pairing and 059's sign-in say the same thing:

  > A run's transcript is everything the agent read and wrote: file contents, command
  > output, and sometimes values from your environment. Rimaia removes its own tokens and
  > any environment variable named like a secret before anything is written, but it cannot
  > recognise a secret the agent read out of a file. With full transcripts, teammates can
  > read them on the server. With summaries only, the transcript stays on this machine,
  > and the server gets the outcome, the diff and the commits.

- There is no Settings control for it in this task. In solo nothing leaves the machine,
  and pairing, where the choice is made, is 058's and 059's.

**MCP tools for the five new local commands** (ADR-0021 point 1):

- `get_transcript_upload` gets a local-router tool, `RunAccess::Refused`. It carries no
  secret, and an operator's agent asked "where do my transcripts go" should be able to
  answer.
- `set_transcript_upload` gets no tool. Widening what leaves the machine is a choice a
  person makes after reading `UPLOAD_DISCLOSURE`, and an agent cannot read it for them. It
  is a recorded gap, on D25 point 6's ground: the argument is consent, not data.
- The three local transcript reads get no tool. They mirror the three board reads, which
  have none today (among D32 point 9's 19 gaps). A tool for the local half alone would give
  an operator's agent a door to kept transcripts and none to uploaded ones. They are
  recorded as inheriting that gap, which 060's parity work closes together.

### 8. Deleting removes the server's files (ADR-0029 point 6)

`delete_task`, and 051's `teams::delete::purge` (which account deletion also calls), collect
the affected transcript keys in their transaction. After it commits, they call
`store.delete` for each one, only with `Server`. A failed delete is logged at `error` with
the key, which holds ids and no content (ADR-0037 point 6). In solo `delete_task` still
leaves the runner's files, as it does today (a runner's files are its owner's). This stays
here, not in 068, because ADR-0029 point 6 applies from the first byte a server holds, and
051 hands `purge`'s extension to this task.

### 9. Records

Each of these is appended to `docs/seam-contract.md` in the same commit as the code it
describes, dated the day it lands:

- **D31 point 4 amendment:** `start_run` writes `transcript_key` instead of `log_path`, and
  `transcript_bytes = 0` on a server.
- **D31 point 12 amendment:** Scope 3's ownership check, which runs before everything;
  "already applied" means the stored fact matches (the `start_run` row's runner, kind and
  session; a non-empty chunk inside `held`; a closed row's runner, status, exit class and
  head); an empty chunk is a probe, never a resend; any other closed row is `Conflict`;
  `BEGIN IMMEDIATE` on appends.
- **D25 point 5 amendment:** the redacted set widens from "exactly the values that were
  injected" to those plus secret-named environment values and the host's own tokens, each
  also in its JSON-escaped form. It is still not a scanner: the set is known, not guessed.
- **D32 point 2 note:** `BoardHost.transcripts`, as an instance of "a field arrives with
  the task that needs it". **D32 point 9:** the four MCP gaps from Scope 7.
- **"How to use this":** a row for 056: D3 · D8 · D10 · D14 · D17 · D25 · D28 · D31 · D32 ·
  D33 · D34.

And one ADR amendment, in the same commit as the outbox:

- **ADR-0036: `## Amendment, <date> — an offline finish is complete only within a lease`**,
  appended to `docs/adr/0036-transcripts-and-review-artifacts-leave-the-machine.md` in the
  voice of ADR-0013's amendment, and dated the day it lands. It records what this task
  builds and does not leave the ADR to be corrected in a diff. Point 1's "a run that
  finished while a laptop was offline appears on the board when the laptop reconnects,
  complete" holds for an outage shorter than one `LEASE_LIFETIME` (three minutes, 043 and
  ADR-0031 point 3). Heartbeats cannot reach the server during an outage either, so after
  that 053 closes the run as `interrupted`. ADR-0031 point 3 fences transcript chunks by
  generation, so the bytes and the finish that follow are `conflict`. The rest of that
  attempt's transcript stays on the runner, the board shows `incomplete` naming the runner,
  and its owner reads it through the local commands. The work is not lost, because the
  pinned re-claim resumes the worktree and the session. The amendment says that fencing
  bytes by the run, whose `runner_id` already names the only writer, is the likely way to
  widen the guarantee, and that doing so needs its own ADR-0036 and ADR-0031 change and a
  D31 amendment. It changes no decision: ADR-0031 still governs, and this task still accepts
  no stale-generation byte. The "Consequences" bullet "a runner that works through a night
  without a connection loses nothing" is qualified the same way: it loses no work, and the
  server's copy of the interrupted attempt's transcript stops where the lease did.

### 10. The interface

- `src/types.ts` mirrors `TranscriptState`. `TranscriptViewer.tsx` and
  `RunDetailOverlay.tsx` render from it. Exact copy:
  - `keptOnRunner`, on another client: `Transcript kept on <runnerLabel>. Only that machine
    can open it.` With no runner: `Transcript kept on a machine that is no longer paired.`
    On the runner that kept it (049's `localRunnerId` equals `runnerId`), the viewer reads
    through the three local commands and shows no notice.
  - `pruned`: `Transcript removed on <date> by the team's retention setting. The outcome,
    diff and commits are kept.`
  - `incomplete` on a run that has ended: `The server holds part of this transcript. The
    rest is on <runnerLabel>.`, or `… The rest is on a machine that is no longer paired.`
    with no runner. The lines that are held are shown below it. On the runner that holds
    the rest, the local read is used instead.
  - `incomplete` on a running run: no notice. The live tail is the view.
- Each of the five new local commands gets its `local<T>` wrapper in
  `src/lib/commands.ts` and a row in 028's fixture transport in `src/dev/fixtures/`. The
  fixture seed gains one run per `TranscriptState` notice, and `screenshots/views.shot.ts`
  gains a row for each.

## Out of scope

- **Retention on the server:** the team setting, its commands and tools, the hourly sweep
  and the retention control. Task 068. This task's reader already renders `pruned`.
- **Storage per team, and the metrics that report it** (ADR-0036 consequences, ADR-0037
  point 7). 062 reads `SUM(transcript_bytes)` per team.
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
- `reading_a_missing_key_is_not_found` and `deleting_a_missing_key_is_not_an_error`;
- `a_key_cannot_name_a_path_outside_the_store`, which covers `..`, an absolute path, a
  backslash, an empty segment and a missing `.jsonl`.

**The board**, through `board::service` with each hold, and through both adapters where a
case is marked *contract*. *Contract* cases go in `testing/board_contract.rs`, so they run
in process (036) and over HTTP (052):

- `the_server_keys_a_transcript_by_team_and_run` and
  `solo_keys_a_transcript_by_its_runner_file`.
- `a_chunk_for_another_tasks_run_is_not_found` and
  `a_finish_for_another_tasks_run_is_not_found` (*contract*): runner B, holding a lease on
  another task of the same team, gets `not_found`, and the other run's bytes and row are
  unchanged.
- `a_chunk_that_leaves_a_gap_writes_nothing_and_answers_what_is_stored` (Server).
- `an_overlapping_chunk_writes_only_its_new_tail` (Server).
- `two_appends_racing_at_one_offset_leave_the_file_as_long_as_the_row` (Server, file-backed
  board): two appends of different lengths at the same offset, joined concurrently. The
  file's length equals `transcript_bytes` afterwards, and a third chunk at that offset is
  accepted.
- `new_bytes_under_a_stale_generation_are_conflict` (Server).
- `a_resent_chunk_is_acknowledged_without_writing_twice` (*contract*).
- `a_resent_start_run_is_acknowledged_once` (*contract*).
- `a_resent_finish_gets_the_first_answer` (*contract*), with one case for `Released` and
  one for `Continue { Review }` under a loop that is on (021).
- `a_resend_after_the_lease_moved_on_is_acknowledged_not_fenced` (*contract*).
- `a_finish_after_expiry_closed_the_run_is_conflict_not_a_resend` (*contract*): A's lease
  expires and 053 closes the run as `interrupted`. A's `succeeded` finish, carrying the
  stale generation, is `Conflict`, and the row is unchanged.
- `every_lease_method_refuses_a_stale_generation` passes unchanged.
- `a_finish_marks_the_transcript_complete_only_when_every_byte_is_held` and
  `a_finish_short_of_its_bytes_is_invalid_and_changes_nothing`: after the refusal the run
  is still `running` and the task has not moved.
- `kept_on_runner_is_recorded_and_held_bytes_are_deleted`.
- `solo_marks_a_finished_transcript_complete_without_copying_a_byte`: `runs_dir()` holds
  exactly the one file the run wrote.
- `get_run` reports each of the five `TranscriptState` variants, a kept row that is also
  pruned as `keptOnRunner`, and a kept row whose runner was deleted with `null` id and
  label.
- `a_chunk_of_exactly_the_chunk_size_is_accepted_over_http`, in
  `crates/runner/tests/board_port_http.rs`.

**The reads**

- The three transcript reads are `board` `Read` rows served through `dispatch`, flipped in
  one commit, and `./scripts/check-command-wiring.sh` passes.
- `crates/server/tests/commands.rs` has a cross-team case for each of the three flipped
  rows, and `every_board_command_has_a_case` and `a_team_cannot_see_another_teams_ids`
  pass.
- Every test in `crates/core/src/runs/transcript.rs` keeps its assertions, including
  `reading_a_transcript_that_does_not_exist_is_a_not_found_error` and
  `summarizing_a_transcript_that_does_not_exist_is_a_not_found_error`. Only the way the
  input is built changes, to a `FileTranscriptStore` over a `TempDir` with `complete:
  true`.
- `an_incomplete_transcript_is_read_up_to_its_last_whole_line`, and
  `a_complete_transcript_still_yields_its_cut_last_line_as_malformed`.
- `a_kept_transcript_is_refused_by_the_board_and_read_by_its_own_runner`, with the exact
  sentence.
- `the_runner_never_prunes_a_transcript_the_server_has_not_acknowledged`.
- `runner_side_readers_use_the_upload_row_and_fall_back_to_the_derived_path`, covering
  `reveal_run_log`, `get_run_log_size`, `get_run_log_path` and the survey's check.

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
- `a_short_finish_is_resent_after_the_probe_finds_the_missing_bytes`, and
  `an_invalid_finish_with_every_byte_held_reaches_the_caller`.
- `finish_is_sent_only_after_every_byte_is_acknowledged`.
- `one_leases_backlog_does_not_hold_up_another`.
- `a_read_under_a_lease_waits_for_that_leases_backlog`.
- `findings_sent_right_after_an_outage_land_after_the_review_runs_start`: through
  `for_agent()`, the findings call returns only after the queued `start_run` is delivered,
  and the findings are stored.
- `the_agents_record_strategy_returns_the_boards_refusal`: through `for_agent()`, an
  `invalid` strategy reaches the caller, and nothing is queued.
- `a_conflict_drops_that_leases_backlog_and_keeps_the_local_file`.
- `a_not_found_drops_the_backlog_and_makes_the_file_prunable`.
- `a_dropped_start_run_fails_its_waiting_finish`.
- `upgrade_required_stops_delivery_and_keeps_the_backlog`.
- `retries_back_off_on_the_injected_clock`: the delivery attempts land at 1, 2, 4 … 60,
  60 s.
- `queued_reports_are_delivered_before_reconcile_touches_their_lease`: a queued
  `finish_run` wins over 043's interrupted report.
- `an_offline_start_does_not_wait_for_reconciles_finishes`.
- `a_released_reply_nobody_awaits_clears_the_held_lease`.
- `solo_leaves_an_empty_outbox_after_a_run`.
- `a_summaries_only_runner_sends_the_row_the_bundle_and_the_tail_and_no_bytes`.

**Redaction**

- `secret_env_values_match_names_by_suffix_case_insensitively`, which also covers a
  short value, a non-UTF-8 value, and names that merely contain `KEY` (`KEYBOARD`,
  `MONKEY_BUSINESS`), which are kept out.
- `a_secret_with_a_quote_is_redacted_in_its_escaped_form`: a `*_PASSWORD` value containing
  `"` and `\`, printed by a real child inside a JSON event, is `[redacted]` in the
  transcript.
- `a_secret_in_the_childs_environment_is_redacted_everywhere_a_run_writes`: a real child
  prints the value of a `*_API_KEY` variable its plan sets, and the transcript, the stderr
  log and the published tail all carry `[redacted]` instead. The test never calls
  `std::env::set_var`, which races every other test in the binary.
- `the_runner_token_is_redacted_from_everything_a_run_writes`: the script prints a fixed
  string, and the test sets that string as `host_secrets`.
- `the_token_accounts_are_spelled_as_058_and_059_define_them`, in `crates/core`:
  `CredentialKey::RunnerToken { runner_id }.account()` equals `runner-token:<runner_id>`,
  `CredentialKey::DesktopToken { runner_id }.account()` equals `desktop-token:<runner_id>`,
  and neither contains a `/`.
- `host_secrets_holds_the_paired_tokens_and_nothing_in_solo`: over `MemoryStore`. With no
  `server_url` it is empty. With one, and both tokens stored with `set` under
  `CredentialKey::RunnerToken` and `CredentialKey::DesktopToken` for
  `runner_identity.runner_id`, both are redacted. A token stored under another runner's id
  is not. An unavailable keychain is an error.
- `a_remote_runs_transcript_and_tail_arrive_redacted`, in `crates/runner/tests/`: a real
  run through `OutboxBoard` over `HttpBoard` against the real server router (052's
  harness). The server's stored transcript and a 048 subscriber's `runs:tail` both contain
  `[redacted]` and never the sentinel.
- `crates/core/tests/runner_credentials.rs` passes with no assertion changed.

**Deletion**

- `deleting_a_task_deletes_its_transcripts_from_the_server` and
  `deleting_a_team_deletes_its_transcripts`. Solo `delete_task` leaves the runner's files,
  and an existing test pins that or a new one does.

**Records**

- ADR-0036 ends with the dated amendment from Scope 9, which names `LEASE_LIFETIME`, the
  `incomplete` state and the local commands, and qualifies both point 1's "complete" and
  the offline-runner consequence. ADR-0031 and D31 are not edited.
- `an_outage_longer_than_a_lease_leaves_the_servers_copy_incomplete_and_the_file_whole`, in
  `crates/runner/tests/outbox.rs`, pins what the amendment says: the switch goes off
  mid-run, the clock passes `LEASE_LIFETIME`, 053's sweep closes the run, and the switch
  comes back on. `get_run` reports `incomplete` with the held byte count and the runner's
  id and label, the server's bytes are a prefix of the local file, the local file is whole
  and not prunable, and the outbox holds nothing for that lease.

**MCP**

- `every_registered_tool_has_a_run_scope_decision` passes with `get_transcript_upload`
  `Refused`, and the four gaps are recorded in D32 point 9.

**Interface**

- Vitest cases for each `TranscriptState` notice, with the exact strings (both no-runner
  variants included), and for the switch to the local commands when `localRunnerId`
  matches.
- 028's fixture transport has a row for each of the five new commands, and its
  every-command test passes.
- `npm run screenshot` covers each notice, and the implementing run has looked at the
  images.
- The 31 test files that mock `@tauri-apps/api/core` still pass. The ones that assert
  `invoke` for the three transcript reads were updated only as far as the flip requires.

**Every command in CLAUDE.md passes**, with `SQLX_OFFLINE=true` exported, exactly as
`ci.yml` runs them, `cargo test -p rimaia-runner` and the runner's clippy line included.

## Notes

**Read first.** ADR-0036 in full. ADR-0013 and its amendment (the file stays, and a missing
file is noticed on read). ADR-0022 point 2 (rows are kept, and transcripts are a cache).
ADR-0020 point 7 (redaction before write). Also ADR-0028 point 2 (the upload choice is a
runner setting), ADR-0029 point 6 (deletion), ADR-0031 points 3 to 5 (fencing and
reconcile), ADR-0034 point 2 (the raw-body route) and ADR-0037 points 4 and 6. Seam
entries:

- **D28**: part 6's two files and the backfill. Its D4 amendment for the names and the
  freeze rule.
- **D31**: point 4 (`append_transcript`, `publish_tail`, `finish_run`), point 7's
  transcript row, point 10 (the raw-body route, and "the adapter does not retry"), point 11
  (one reaction to `Conflict`), point 12 (resends), and point 13 (the suite, and its "056:
  resends").
- **D32**: point 2 (`BoardHost` grows here), point 8 (the flip), point 9 (gaps), and the
  appendix rows for the three reads, `reveal_run_log`, `get_run_log_size` and
  `prune_run_logs`.
- **D33**: point 3's recipe, for both caches.
- **D25** point 5: what is redacted, which Scope 9 amends.
- **D14**: the tail is never replayed and never the source of truth.
- **D17** point 5: a planner transcript has no row and stays on the runner.
- **D34**: no object-storage client, no `regex` for redaction, and the body limit raised
  on `append_transcript` alone.
- **D3**, **D8** and **D10**: the typed accessors, no new error code, and string ids.

**Files to start from** (on `main` today):

- `crates/core/src/runner/events.rs`: `Transcript`, `transcript_path`, `EventStream::observe`
  and `redacting`.
- `crates/core/src/runner/process.rs`: `execute`, `repository_credentials`,
  `with_repository_credentials` and `spawn`. 041 and 042 may have moved some of these.
  Follow the names.
- `crates/core/src/credentials/{redact,inject}.rs`.
- `crates/core/src/runs/{mod,transcript}.rs`: `prune_logs`, `get_run`, `log_available`,
  `read_page`, `search` and `summarize`.
- `crates/core/tests/runner_credentials.rs`: the script that prints its own token. Copy
  the technique.
- `crates/core/src/testing/cli.rs`: `write_script` and `replays`.
- `src/components/runs/{TranscriptViewer,RunDetailOverlay}.tsx`, with their tests.

**What the chain provides.**

- **033:** `review_bundles`, and the row and bundle that reach the server with the finish.
- **036:** the port, `TranscriptEnd`, the in-process body, and `run_task` reading `length`
  off the file.
- **038/039:** `team_id` on every row, and contexts scoped per team.
- **040, 041 and 066:** `runner.db`, `runner_settings`, the local MCP router, and 066's
  derived log path (with `get_run_log_path`), which this task replaces.
- **043:** `current()`, generations, `held_leases`, `reconcile_held` and `BEGIN IMMEDIATE`
  on lease transactions.
- **048/052:** the tail relay end to end. This task adds nothing to the stream except its
  redacted end-to-end test. 052 serves the raw-body route and leaves its body limit here.
- **051:** `teams::delete::purge`, whose extension to transcript files 051 hands to this
  task.
- **053:** expiry, request timeouts, and `fence::on_fenced`.
- **054:** the migrations that sort before this task's in both sets (D4: task order is
  version order).
- **055:** `run_tool` on the port, and the proxy that `for_agent()` serves.

**What the next tasks expect.**

- **057** rewrites the outcome before `OutboxBoard::finish_run`, and relies on a fenced
  runner's backlog already being dropped.
- **058** owns the headless half: `rimaia-runner pair` prints `UPLOAD_DISCLOSURE` and offers
  `summaries_only` (setting `upload_transcripts`); the host stores its token under the
  `CredentialKey::RunnerToken` this task added, through 058's own `save_runner_token`,
  fills `RunnerConfig::host_secrets` from `secrets::host_secrets`, wraps `HttpBoard` in
  `OutboxBoard`, and hands `for_agent()` to `run_proxy::bind`.
- **059** owns the same four for a connected desktop: the disclosure and the choice at
  sign-in through `set_transcript_upload`, both tokens under `CredentialKey::RunnerToken`
  and `CredentialKey::DesktopToken` as this task added them, `host_secrets`, and the wrap.
- **061** may show `transcript` states elsewhere on the card, from the same DTO.
- **062** reports storage per team from `transcript_bytes` and backs up `transcripts/`.
- **065** drops `runs.log_path`, which nothing reads or writes after this task.
- **068** writes `transcript_pruned_at` and the bundle's `patch_pruned_at`, and must not
  change `TranscriptState`'s order.

**A known limit, recorded and not solved: bytes that arrive after their lease expired.**
ADR-0031 point 3 fences every report by generation, and it names transcript chunks. So a
laptop offline for longer than a lease's lifetime comes back to a run the server has
already closed as `interrupted` (053). Its new bytes and its finish are `conflict`, and the
rest of that attempt's transcript stays on the laptop. The board says so (`incomplete`:
"The rest is on <runnerLabel>"), and the owner can still read it locally. The work is not
lost: the worktree and the session survive, and the pinned re-claim resumes them. ADR-0036's
"a run that finished while a laptop was offline appears … complete" therefore holds for
outages shorter than the lease, and Scope 9's ADR-0036 amendment records that narrowing in
the ADR itself, so the next agent reads it there rather than in a PR body. **Do not accept
stale-generation bytes to close this gap.** That would contradict ADR-0031 and a seam
entry. Widening the guarantee, most likely by fencing bytes by the run rather than by the
lease since the run's `runner_id` already identifies the only writer, is a decision for a
new ADR change and a D31 amendment, not for this task. No numbered task schedules it: the
amendment names it as the open question.

**Another state this task inherits and does not change.** A `finish_run` answered with
`Continue`, followed by a crash before the next phase's `start_run`, leaves a held lease
whose run is closed. That already happens today, with no outbox, if the process dies right
after the reply, and 043's `reconcile_held` owns it. The outbox makes the state reachable
by one more route, a restart before the queued finish is delivered, and it hands the lease
to the same reconcile. It adds no rule of its own.

**Size.** Retention moved to 068 so that this fits one session: about 2,700 changed lines
before tests and caches, most of it the outbox (about 900) and the board side (about 600).
The two `CredentialKey` variants, the ADR-0036 amendment and their two tests add well under
a hundred lines, and do not change that estimate. If it still runs past one session, stop
and return `blocked` with a question. Do not cut scope or create a task mid-run: the
workflow's task list is fixed. In particular, never drop the ownership check, the resend
rules, the gap rewind or the redaction. Without the first, one runner writes into another's
transcript. Without the next two, a flaky network duplicates or loses bytes. Without the
last, the first upload sends secrets to a server.
