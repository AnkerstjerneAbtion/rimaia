---
id: "062"
title: Hosting, backups and observability
milestone: v0.5
status: ready
depends_on: ["056", "058", "060", "068"]
adrs: ["0037", "0036", "0028", "0030", "0031", "0034"]
size: L
---

# Hosting, backups and observability

## Goal

Make `rimaia-server` something a person can deploy, lose and get back. After this task the
repository holds one `Dockerfile` that builds the server binary and the web bundle into one
image (ADR-0037 point 1). The server configures itself from environment variables only, and
refuses a variable it does not know. It takes an exclusive lock on its data directory before
it touches anything in it. It replicates its SQLite file continuously with Litestream, backs
up transcripts to the same destination on a schedule, and restores both onto an empty volume
by itself (point 2). It answers a health check, and a separate listener serves metrics an
operator can diagnose from without reading team data (point 7). No log line carries a plan, a
transcript or a secret (point 6). A runbook, `docs/hosting.md`, says how to deploy, restore
and watch the instance, and `scripts/rehearse-restore.sh` proves the restore half of it
against a local replica, with no cloud account.

**This task does not deploy the hosted instance.** The container host, the bucket and the
GitHub OAuth app belong to a person (the plan's "Needs a person" list). What this task
delivers is everything that person needs, plus a rehearsal that fails if any of it is wrong.

## Why now

056 is the last task that adds something the server stores. After it, the board, the review
bundles and the uploaded transcripts all exist on the server, and ADR-0036's Consequences say
what that means: source code from other teams' repositories now lives on whoever hosts the
instance. ADR-0037 point 2 puts the restore **before** the instance holds anyone's data: "A
restore is tested before the instance holds anyone's data … An untested backup is a hope."
This task is where that test first becomes possible, because before 056 there was no
transcript to back up and no complete picture of what a restore has to bring back.

Several earlier tasks left a piece here on purpose, and each is a hole in a deployed server
until this task fills it:

- 046 ships a binary that reads two variables and installs no tracing subscriber. The lock
  file, the public URL and logging setup are 062's.
- 047 keys its rate limiter by the peer address. Behind a platform's proxy that address is the
  proxy, so the per-address limit becomes one limit for the whole instance. 047 left
  "which proxy to trust" to 062.
- 050 sets `frame-ancestors` and nothing else of a content security policy, and says 062 owns
  the production headers.
- 048 suggests that counters for lagged streams may come from its log lines. That only works
  if the log lines are safe to keep, which is point 6.

The plan's end-of-M6 check is "the Docker image boots, and a Litestream restore is
rehearsed". This task makes both of those a script.

## Scope

**1. Configuration, in one place (ADR-0037 point 1).** 046's parser, which 047 and 050
extended, lives in `crates/server/src/config.rs` after this task; move it there if it landed
elsewhere. Its input changes from a lookup closure to an iterator of `(OsString, OsString)`
pairs, which `main` fills from `std::env::vars_os()`. A closure answers for a name it is
asked about and cannot list the names nobody asked about, and the unknown-variable refusal
below needs that list. This task makes the parser the single table of every variable the
server reads, adds its own, and adds two refusals. When it is done, the table is:

| Variable | Required | From | Meaning |
| --- | --- | --- | --- |
| `RIMAIA_DATA_DIR` | yes | 046 | Absolute path, validated as `AppPaths::resolve` validates an override |
| `RIMAIA_LISTEN` | yes | 046 | Socket address of the public listener |
| `RIMAIA_PUBLIC_URL` | yes | 047 | The origin people reach the server at |
| `RIMAIA_GITHUB_CLIENT_ID` | yes | 047 | |
| `RIMAIA_GITHUB_CLIENT_SECRET` | yes | 047 | Never printed; `Debug` redacts it |
| `RIMAIA_WEB_ROOT` | no | 050 | The built bundle. The image sets it |
| `RIMAIA_LOG` | no | 062 | Log filter. The desktop already reads this name (`src-tauri/src/logging.rs`) |
| `RIMAIA_METRICS_LISTEN` | no | 062 | Socket address of the metrics listener. Unset: no metrics are served |
| `RIMAIA_TRUSTED_PROXY_HOPS` | no | 062 | Integer, default `0`. See point 7 |
| `RIMAIA_BACKUP_URL` | no | 062 | `s3://<bucket>/<prefix>` or `file:///<absolute path>`. Unset: backups are off |
| `RIMAIA_BACKUP_S3_ENDPOINT` | no | 062 | For an S3-compatible store that is not AWS |
| `RIMAIA_TRANSCRIPT_BACKUP_MINUTES` | no | 062 | Integer, default `60`, at least `5` |
| `RIMAIA_LITESTREAM_BIN`, `RIMAIA_RCLONE_BIN` | no | 062 | Program paths, default `litestream` and `rclone` on `PATH`. Tests point them at stand-ins |

- **An unknown `RIMAIA_*` variable is refused**, and the refusal names it. A typo in
  `RIMAIA_BACKUP_URL` would otherwise start a server with backups silently off. The rule is
  checked against the table, so a variable a later task adds without a row fails its own
  first test run.
- **A backup URL that carries credentials is refused** (`s3://key:secret@bucket/...`), as
  is a `file://` URL whose path is not absolute, and any other scheme. Credentials for the
  store come from the standard `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and `AWS_REGION`
  variables. The server never reads them; it passes them to its two children (point 4).
- **Unset `RIMAIA_BACKUP_URL` means off, loudly.** The server starts, logs one `warn` line
  saying backups are off, and reports `rimaia_backup_configured 0`. It does not refuse to
  start: 046's documented local run (`RIMAIA_DATA_DIR=… RIMAIA_LISTEN=… cargo run -p
  rimaia-server`) has no bucket, and a self-hoster may back up the volume another way. The
  runbook makes the hosted instance's alert on that metric a precondition of going live.
- **One `info` line at startup** states the version, the protocol version, the data
  directory, whether the web root, metrics, trusted hops and backups are configured, and the
  backup URL's scheme and bucket. It never states a secret or the S3 endpoint's credentials.
- Refusals follow D11: the process exits non-zero with a message on stderr that names what
  failed.

**2. The lock file (ADR-0037 point 1).** `crates/server/src/lock.rs`, `DataDirLock`:

- `DataDirLock::acquire(data_dir)` opens `<data dir>/rimaia-server.lock`, creating it, and
  calls `std::fs::File::try_lock()`. That call is stable since Rust 1.89, and the pinned
  toolchain is 1.98.0, so no crate is needed. `fs4` is already in the tree, but D19 confines
  its import to `doctor::checks::disk_space`, and std makes widening that unnecessary.
- **A held lock is a refusal:** `another rimaia-server holds <path>; a second instance must
  not open the same data directory`. After acquiring, the holder writes its process id,
  host name and start time into the file. That text is for a person reading the file. No
  code ever reads it back to decide anything.
- The lock is held for the life of the process, and released when its `File` drops.
- **The order in `main` is fixed:** parse the configuration, take the lock, restore if
  needed (point 4), connect and migrate, start Litestream, bind, serve. A second instance
  therefore stops before it runs a restore, before it starts a second Litestream, and before
  it opens `rimaia.db`. Two Litestream processes replicating one database corrupt the
  replica. Litestream also writes into the database it replicates, so the lock has to come
  first, and that is why the server, not an entrypoint script, starts Litestream (see
  Notes).
- The lock protects one volume. It cannot see a second instance on a second volume that
  points at the same backup URL. That case is closed by the deployment pinning the instance
  count to one (ADR-0037 point 1), and the runbook says so.

**3. Health (ADR-0037 point 7).** `GET /healthz` on the public listener:

- It answers `200` with `{"status":"ok","version":"<CARGO_PKG_VERSION>","protocol":"<PROTOCOL_VERSION>"}`
  once `ops::ping(pool)` succeeds. That is `SELECT 1`, in `crates/core/src/ops.rs` beside
  Scope 5's snapshot, because `rimaia-server` names no `sqlx` (046, D33 point 2) and reaches
  the store only through `rimaia-core`. Otherwise it answers `503` with
  `{"status":"unavailable"}`. The cause goes into a `warn` line, never into the body, because
  a database error can name a path.
- It takes no `Caller`, sets no cookie and reads no row. It is not a board route (D32 counts
  board routes only, and `/auth/*` and the bundle are already routes without one). It is
  mounted outside `/api/v1`, so 046's JSON fallback does not apply to it.
- The platform's health check starts only once `main` has bound, which is after any
  restore. The runbook sets the platform's grace period accordingly.

**4. Backups: Litestream for the database, rclone for transcripts (ADR-0037 point 2).**
`crates/server/src/backup/`, with one module per child program.

- **Both programs are children of the server**, built as argument vectors (no `sh -c`), and
  their environment is cleared and rebuilt. They receive `PATH`, `HOME`, every `AWS_*`
  variable, every `LITESTREAM_*` variable and every `RCLONE_*` variable, and nothing else.
  In particular they never receive `RIMAIA_GITHUB_CLIENT_SECRET`, and never a `CLAUDE_*`
  variable (CLAUDE.md's rule for spawned processes).
- **Litestream's configuration is rendered, not checked in.** `backup::litestream::config(&BackupTarget) -> String`
  is a pure function, and its output is written to `<data dir>/litestream.yml` on each
  start. It names `<data dir>/rimaia.db` and its one `replica`, derived from
  `RIMAIA_BACKUP_URL`. When `RIMAIA_BACKUP_S3_ENDPOINT` is set, the replica also carries
  `endpoint` and `force-path-style: true`, which MinIO and most non-AWS stores need. The
  top-level `snapshot` block sets **`retention: 720h`**. That is ADR-0037 point 6's 30
  days: a deleted team's rows age out of the backups when it passes, and it is also how far
  back a point-in-time restore reaches. The file holds no credential, because Litestream
  reads those from its environment. **The field names are Litestream 0.5's.** 0.3's
  `replicas:` list and its per-replica `retention` are a different format, and the
  rehearsal is what proves the rendered file against the pinned binary.
- **Restore on an empty volume.** When `rimaia.db` does not exist and a backup URL is
  configured, `main` runs `litestream restore -config <file> -if-db-not-exists
  -if-replica-exists <db path>` and waits for it to finish before `db::connect`. If the
  database was restored, the server also restores the transcripts (below). A database that
  exists is never restored over. The check is made in Rust, and `-if-db-not-exists` is
  passed as well, so both have to be wrong at once before anything is overwritten. A
  restore that exits non-zero is a startup refusal (D11). The server does not start on an
  empty board while a backup exists that it failed to read.
- **Replication.** After migrating, the server starts `litestream replicate -config <file>`
  and keeps it running. If the child exits, the server logs an `error` line and starts it
  again after a backoff of 1, 2, 4 … seconds, capped at 60. It waits through
  `Clock::sleep_until` so a test drives it. It counts restarts in
  `rimaia_litestream_restarts_total` and reports `rimaia_litestream_up`. It does not exit
  the server: the board keeps working and the metric pages someone.
- **Transcripts, on a schedule.** Every `RIMAIA_TRANSCRIPT_BACKUP_MINUTES`, on the injected
  clock, the server runs two commands in order:
  1. `rclone sync <transcript root> <remote>/transcripts --backup-dir
     <remote>/transcripts-deleted/<UTC stamp>`. A transcript that retention or a deletion
     removed from the server leaves the live copy and lands under a dated prefix.
  2. `rclone delete <remote>/transcripts-deleted --min-age 30d --rmdirs`. That is the same
     30 days as Litestream's retention, enforced by the product rather than left to a
     bucket lifecycle rule someone may forget to set.

  `<remote>` is derived from `RIMAIA_BACKUP_URL`: a local path for `file://`, rclone's
  on-the-fly `:s3:<bucket>/<prefix>` for `s3://`. The endpoint and `env_auth` go through
  `RCLONE_S3_*` variables set on the child, never through the argument vector. The
  transcript root is `<data dir>/transcripts/` (056). `main` computes it once and passes the
  same path to `FileTranscriptStore::new` and to the backup module. `TranscriptStore` keeps
  its four operations and gains no root accessor. Success sets
  `rimaia_transcript_backup_last_success_timestamp_seconds` from the injected clock. A
  failure increments `rimaia_transcript_backup_failures_total`, logs rclone's exit
  status (never its output, which names transcript keys), and retries at the next interval.
- **Transcript restore** is `rclone copy <remote>/transcripts <transcript root>`, run by
  `main` after a database restore and before serving.
- **Shutdown.** On SIGTERM or Ctrl-C the server:
  1. stops accepting connections;
  2. calls `StreamControl::shutdown()`, which this task adds to 048's `StreamControl`: a
     `tokio::sync::watch` arm that every stream loop selects beside its revalidation arm,
     and that ends the stream;
  3. waits at most 10 seconds for in-flight requests;
  4. closes the pool with `close()` on the pool `ServiceContext` holds (no `sqlx` import);
  5. sends SIGTERM to Litestream and waits at most 10 seconds for it to exit.

  The order is the point. Litestream holds its own connection, so the server's close is not
  the last one and does not delete the WAL under it, and Litestream ships the final frames
  after the last write. SSE streams never end by themselves, so without step 2 a graceful
  shutdown would wait on them forever. On a platform that sends SIGTERM to process 1, the
  server is process 1 and handles the signal itself.

  **Step 5 runs `kill -s TERM -- <pid>` as an argument vector**, as `rimaia-core`'s
  `signal_group` does (with the child's pid, not a group). std and tokio can only SIGKILL a
  `Child`, no `libc` or `nix` is approved (D34), and that function is `pub(crate)`. On
  bookworm `kill` comes from `procps`, which the slim image lacks and CI's hosts have, so
  the runtime image installs it and an image test asserts that. Steps 3 and 5 take up to 20
  seconds, so the runbook requires a stop grace of at least 30 (Scope 10). A platform's
  usual 10 would kill Litestream before its final sync.

**5. Metrics (ADR-0037 point 7).** When `RIMAIA_METRICS_LISTEN` is set, a second listener
serves `GET /metrics` in the Prometheus text format, and nothing else. It is never mounted on
the public listener. It has no authentication, because ADR-0030 defines no operator
credential, and a network boundary is the one access control that needs no new credential
kind. The runbook binds it to the platform's private network. The storage figures below name
which teams exist and how active they are, and that is team data in ADR-0037 point 6's
sense.

D34 declines a metrics crate. The counters are atomics in `crates/server/src/metrics.rs`,
rendered by one handler. These are the metric names, and a golden test asserts the rendered
text exactly:

| Metric | Type | Labels | Source |
| --- | --- | --- | --- |
| `rimaia_build_info` | gauge, always 1 | `version`, `protocol` | constants |
| `rimaia_http_responses_total` | counter | `route`, `status` | middleware; `route` is axum's `MatchedPath`, or `unmatched`; never the URI |
| `rimaia_runners` | gauge | `state` = `paired` \| `connected` | `runners`: `unpaired_at IS NULL`; connected = `last_seen_at` within 053's lease lifetime of now |
| `rimaia_leases_active` | gauge | `purpose` | `runner_leases` with `expires_at` NULL or after now |
| `rimaia_claim_duration_seconds` | histogram | — | the claim handler, see below |
| `rimaia_team_storage_bytes` | gauge | `team` (the id), `kind` = `transcripts` \| `patches` | see below |
| `rimaia_database_bytes` | gauge | — | size of `rimaia.db` plus its `-wal` |
| `rimaia_backup_configured` | gauge | — | config |
| `rimaia_litestream_up`, `rimaia_litestream_restarts_total` | gauge, counter | — | point 4 |
| `rimaia_transcript_backup_last_success_timestamp_seconds`, `rimaia_transcript_backup_failures_total` | gauge, counter | — | point 4 |

- **Claim latency excludes the time a request spends parked in 053's long poll.** A latency
  that included the park would measure how idle the queue is, not how fast a claim is. The
  park is inside `board::service`'s claim body (042 steps 1 to 3, with 053's wait between
  attempts), so only `rimaia-core` can see where it ends, and the server never times the
  call from outside. `crates/core/src/board/service.rs` splits the body into
  `claim_timed(ctx, target)`, which returns what `claim` returns plus a `Duration`. It runs from
  the start of the last attempt's selection to the end of its claim transaction, read from
  `ctx.clock`. `claim` keeps its signature for both adapters and drops the duration, so
  D31's port does not change. 052's handler in `crates/server/src/runner_api.rs` calls
  `claim_timed` and records the figure. The buckets are 0.005, 0.01, 0.025, 0.05, 0.1,
  0.25, 0.5, 1 and 2.5 seconds.
- **Storage per team** comes from `rimaia-core`, `crates/core/src/ops.rs`,
  `ops::snapshot(pool, clock) -> OpsSnapshot`. That function reads the runner, lease and
  storage figures above. For each team: the sum of `runs.transcript_bytes` where
  `transcript_pruned_at IS NULL`, and the sum of `length(review_bundles.patch)` where the
  patch is not pruned, both joined to `tasks.team_id`. **It counts runs of every kind**,
  because review and fix transcripts are stored too (D29 asks every reader of `runs` to say
  which kinds it means, and this is the answer). It reads no content column. Teams are
  labelled by id, never by name. It never reads `runs.log_path`, which 065 drops.
- **`ops::snapshot` is the one deliberate cross-team read**, and `ops::ping` reads nothing.
  Both take the pool, not a `ServiceContext`, because they have no caller and belong to no
  team. The snapshot returns counts and opaque ids only. Each gets one entry, with its
  reason in a comment, in the allowlist of 039's `no_service_takes_a_pool_without_a_scope`.
  No registry row and no MCP tool reaches either, and the only route that does is
  `/healthz`, to `ping`.
- The snapshot is computed per scrape. At a scrape interval of 15 seconds or more, two
  grouped sums over indexed joins are cheaper than a cache and its staleness rules.

**6. Logs (ADR-0037 point 6).** `crates/server/src/logging.rs` installs the subscriber that
046 deliberately did not:

- Plain-text lines to stdout, no ANSI, with the target. The platform collects stdout, and
  the runbook sets how long it keeps them and who can read them. There is no file appender:
  a container's disk is not where logs survive.
- The filter is `RIMAIA_LOG` if set, otherwise `rimaia_server=info,rimaia_core=info,warn`.
- **The subscriber is built by a function that takes the filter and a writer**, so the
  hygiene test (acceptance) installs the production format over a buffer.
- **No line carries content.** 046's trace layer already records the matched route and not
  the URI, and 047 already keeps secrets out of spans. This task makes that a test rather
  than a convention, and fixes whatever the test finds. The test covers task titles, plans,
  base instructions, review instructions, review findings, transcript chunks, the
  review-bundle patch in a runner's `finish_run` body (052, 056), tool arguments sent to the
  hosted `/mcp` door (060), request bodies (including malformed ones), bearer tokens,
  cookies, OAuth `code` and `state` values, and `X-Forwarded-For` values.
- `rimaia-server` takes `tracing-subscriber` with `{ workspace = true }`, the line 058
  promoted under D34's row for it. Default features plus `env-filter` include the `fmt`
  layer this needs. It takes no `tracing-appender`.

**7. The client address behind a proxy.** 047 left this open.
`RIMAIA_TRUSTED_PROXY_HOPS = n` says how many proxies in front of the server append to
`X-Forwarded-For`. With `n = 0`, the default, the client is the peer address, which is 047's
behaviour. With `n > 0`, the client is the `n`th entry counted from the right of the
`X-Forwarded-For` values, several headers joined in order. The entries a client can forge are
to the left of the ones its proxies append, so counting from the right is what a forger
cannot move. Fewer than `n` entries, or an entry that is not an IP address, falls back to the
peer address. That fallback is 047's one-bucket limit, which still fails closed. The header's
value is never logged. This is a pure function, `client_address(peer, headers, hops)`, and
047's rate limiter calls it in place of reading `ConnectInfo` directly. The runbook states
the assumption this depends on: the container is reachable only through the platform's
proxy.

**8. Production headers.** 050 left these to this task.

- Bundle responses carry one policy, which replaces 050's `frame-ancestors`-only header,
  set by 050's `map_response` middleware:
  `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' https://avatars.githubusercontent.com; connect-src 'self'; font-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'`.
  React's `style={…}` writes through the CSSOM, which `style-src` does not govern. The
  manual check below is what confirms the built bundle needs nothing more.
- Every response carries `Strict-Transport-Security: max-age=31536000` when
  `RIMAIA_PUBLIC_URL` is `https`, and never otherwise, so a local `http` run is not pinned
  to TLS by a browser. Every response carries `Referrer-Policy: no-referrer` and 050's
  `X-Content-Type-Options: nosniff`.

**9. The image.** At the workspace root: `Dockerfile`, `.dockerignore`, and nothing else.

- **Stage `web`:** `node:20-bookworm-slim`, the version CI uses. It runs `npm ci` and
  `npm run build`, which yields `dist/`. `npm ci` downloads no browser for
  `@playwright/test` (D34).
- **Stage `build`:** `rust:1-bookworm`. **The `Dockerfile` names no Rust version.** rustup
  in the image reads `rust-toolchain.toml` from the build context, which keeps CLAUDE.md's
  "bump the version only in `rust-toolchain.toml`" true. The stage installs
  `libdbus-1-dev` and `pkg-config`, because `rimaia-core` links `keyring`'s secret-service
  backend and the server links `rimaia-core`, as CI's `core` job already does for Linux.
  `ENV SQLX_OFFLINE=true`, then `cargo build --release --locked -p rimaia-server`.
  - The migrations are embedded at compile time from `../../src-tauri/migrations`
    (`crates/core/src/db/mod.rs`, `crates/core/build.rs`). The runner's are embedded from
    `crates/runner/migrations`. The offline caches are `crates/core/.sqlx/` and
    `crates/runner/.sqlx/` (D33, whose Binds name this task).
  - The build context therefore has to include `src-tauri/`, for the migrations and for the
    workspace member manifest Cargo needs, and both `.sqlx/` directories.
  - `.dockerignore` excludes `target/`, `node_modules/`, `dist/`, `.git/`, `.context/`,
    `spike/` and `src-tauri/target/`, and **no pattern that matches a dot-directory in
    general**. A `.*` line drops both caches, and the build then fails offline, or worse,
    goes online.
- **Stage `tools`:** `debian:bookworm-slim` with `curl` and `unzip`. It downloads
  **Litestream 0.5.17** (`litestream-<v>-linux-<arch>.tar.gz`) and **rclone 1.75.1**
  (`rclone-v<v>-linux-<arch>.zip`), and checks each archive with `sha256sum -c`. A later
  patch on the same line is acceptable, but a different line is not, because the config
  format above is 0.5's. `ARG TARGETARCH` selects `amd64` or `arm64`, and any other value
  fails the build. The rehearsal on an Apple Silicon Mac builds `linux/arm64`. **The four
  checksums are copied from upstream's published files**: the Litestream release's
  `checksums.txt` and `https://downloads.rclone.org/v<v>/SHA256SUMS`. Each file's URL goes
  in a comment above the values copied from it. The implementing agent never computes a
  checksum from an archive it downloaded itself, and never writes one from memory. If it
  cannot fetch those files, it stops and asks.
- **Stage `runtime-base`:** `debian:bookworm-slim` with `ca-certificates`, `libdbus-1-3`
  and `procps` (Scope 4's `kill`); the two binaries from `tools`; the server binary;
  `dist/` at `/app/web`; a non-root user owning `/data`. `ENV RIMAIA_DATA_DIR=/data
  RIMAIA_LISTEN=0.0.0.0:8080 RIMAIA_WEB_ROOT=/app/web`, `EXPOSE 8080`, and
  `CMD ["/usr/local/bin/rimaia-server"]`. No entrypoint script and no shell wrapper.
- **Stage `rehearsal FROM runtime-base`**, built only with `--target rehearsal`: adds the
  seed example (point 10), so the rehearsal can write to a live database from inside the
  container.
- **Stage `runtime FROM runtime-base`**, last. Docker builds the last stage by default, so
  this order is what makes `runtime`, without the seed binary, the default target.
- If Docker is available, the implementing agent runs `docker build .` once and says in the
  PR body whether it did. Nothing else builds the image automatically (Out of scope).

**10. The runbook and the rehearsal.**

- **`docs/hosting.md`** is the operator's document, in the register of the rest of `docs/`:
  - the configuration table from point 1;
  - the deployment's fixed conditions: one instance, a persistent volume at `/data`, an EU
    region, the health check at `/healthz` with a grace period longer than a restore, a
    stop grace of at least 30 seconds (`docker run --stop-timeout 30`, or the platform's
    kill timeout; Scope 4), and metrics on the private network only;
  - **deploying and upgrading** (ADR-0037 point 3): stop the old container, then start the
    new one. A rolling or overlapping deploy on one volume makes the new instance refuse on
    the lock, or crash-loop until the old one exits. A deploy should finish within 053's
    three-minute lease lifetime, or running work is interrupted and resumes pinned. Before
    a deploy, confirm that the latest restore point is recent, because the new binary
    migrates at start;
  - backups: what Litestream and rclone each hold, where, for how long, and why the 30 days
    are ADR-0037 point 6's;
  - **the restore runbook**, both paths: the automatic one (start on an empty volume), and a
    point-in-time one (`litestream restore -timestamp` into a scratch path, inspected, then
    swapped in with the server stopped). The point-in-time path brings back what happened
    after the timestamp was undone, so two steps follow the swap, before the server serves
    anyone. First, revoke every session and token in the restored database. One revoked
    after the timestamp is live again (ADR-0030), so every person signs in again and every
    runner pairs again. Second, re-apply every team and account deletion made after the
    timestamp, found in the logs (051's purge span records the team id) and the
    administrators' access record, because ADR-0037 point 6 deletes from the live database
    immediately. The runbook also says what neither path can bring back: runs that
    finished after the last transcript backup have a row whose transcript is missing or
    short. A runner re-sends what 056's offset protocol says the server lacks only while it
    still holds the local file;
  - log retention (30 days, matching backups) and who may read the logs;
  - production database access: named administrators only, and every access recorded in a
    log kept outside the repository (ADR-0037 point 7);
  - the preconditions before the first team outside Abtion: a data processing agreement,
    the stated region, and the open sign-up question 047 raised (see Notes);
  - the rehearsal: when to run it (before go-live, monthly, and after every Litestream or
    rclone version bump) and where its output is recorded.
- **`crates/server/examples/rehearsal_seed.rs`** writes a known board through
  `rimaia-core`'s services, never raw SQL, so it follows the schema as it changes: two teams,
  their repositories and tasks, finished runs of every kind with review bundles, and
  transcripts through 056's store. It has a `--more` mode that adds a second, disjoint set
  to a live database. `cargo check --workspace --all-targets` compiles it, so it cannot rot
  unnoticed.
- **`scripts/rehearse-restore.sh [--replica file|s3]`** needs Docker and `sqlite3`, and
  nothing from a cloud. Bash 3.2-safe, like the other scripts. `/data` is a **named Docker
  volume**, never a bind mount: a bind-mounted directory is owned by the host's uid, which
  the image's non-root user cannot write, and `flock` between two containers on a Docker
  Desktop bind mount is unreliable, which would void step 10. `file` is the default, with
  a second named volume as the replica, mounted at the same path in every container. `s3`
  starts a MinIO container pinned by image digest, with throwaway credentials. The script:
  1. builds `--target rehearsal`;
  2. starts the container on a fresh volume and runs `rehearsal_seed` inside it;
  3. waits for `/healthz` by polling, with a deadline, never a fixed sleep;
  4. polls Litestream's own listing command for the pinned version (`litestream ltx` on
     0.5) inside the container until the replica holds the first seed's last transaction,
     then records a timestamp, then runs `rehearsal_seed --more`. A timestamp taken before
     the replica caught up would make step 9 racy;
  5. waits for a transcript backup by polling the metrics listener's last-success gauge;
  6. stops the container with `docker stop -t 30`;
  7. copies `/data` out of the stopped container with `docker cp`, starts a fresh container
     on an **empty** volume, and waits for `/healthz`;
  8. stops the restored container the same way, copies its `/data` out, and compares the
     two databases with
     `sqlite3 .dump`, Litestream's own tables excluded, and the transcript trees with
     `diff -r`. Both must be identical;
  9. runs a point-in-time restore to the recorded timestamp into a scratch file, and checks
     that the first seed's rows are present and the second's are absent;
  10. starts the restored volume's container again, and while it runs starts a second
      container on the same volume, and checks that the second exits non-zero naming the
      lock;
  11. removes everything it created, and prints one `PASS` or `FAIL` line per step.

**11. CLAUDE.md, dependencies, and the query caches.**

- **CLAUDE.md.** The layout table gains `Dockerfile` (the server image, ADR-0037) and
  `docs/hosting.md` (deploy, restore and observe). Gotchas gains one bullet: the image
  builds offline from both `.sqlx/` caches and embeds `src-tauri/migrations`, so a
  `.dockerignore` line that drops either one breaks the image, not CI. Another bullet says
  that `scripts/rehearse-restore.sh` needs Docker, is not run by CI, and is run before
  go-live and after every backup-tool bump. `## Commands` does not change, because CI gains
  no step.
- **Dependencies.** The one Cargo change is `rimaia-server`'s `tracing-subscriber =
  { workspace = true }`, on the line 058 promoted under D34's row. No `json` feature, which
  would add `tracing-serde` to the tree. Litestream and rclone are binaries in the image,
  pinned in the `Dockerfile` and governed by neither D6 nor D34, as D34's closing list
  says (see Notes for why rclone is not the object-storage client D34 declines).
- **`.sqlx/`.** `ops.rs` adds query macros to `rimaia-core`, so both caches are regenerated
  with D33's recipe and committed in this task.

## Out of scope

- **Deploying the hosted instance, creating the bucket, and the OAuth app.** A person does
  these, with this task's runbook.
- **Building the image in CI.** A release build of the server and the bundle on every pull
  request costs more than it catches. `crates/server/tests/image.rs` checks the
  `Dockerfile`'s structural rules on every run, and the full build is the rehearsal's first
  step. Whether CI should build the image on pushes to `main` is 064's decision, and 064's
  Scope carries the question.
- **Object storage as the transcript store.** ADR-0036 point 7 keeps files on disk; backup
  copies them, and nothing reads from the bucket at run time.
- **Re-uploading transcripts a restore lost.** That is 056's offset protocol working as
  designed, when a runner still has the file. This task documents the gap and does not
  close it.
- **Litestream's own metrics endpoint.** It exists, and the runbook mentions it. The server
  reports `rimaia_litestream_up`, which is enough to alert on.
- **SSE counters** (lagged or revalidated streams). 048's log lines carry them, and they are
  safe to keep after point 6.
- **A lock for the desktop app's solo database.** ADR-0037 point 1 is about the server.
- **Alerting rules and dashboards.** These are the platform's configuration. The runbook
  names the three alerts that matter: `rimaia_backup_configured == 0`,
  `rimaia_litestream_up == 0`, and a transcript backup older than two intervals.
- **Any migration** (D4's list reserves none for 062), **any npm dependency**, and **any
  Cargo dependency** beyond Scope 11's one line (D6, D34).
- **The desktop updater and out-of-date runners.** 063.

## Acceptance criteria

Rust tests use the real SQLite harness, `crates/core`'s `TestClock`, and real files in a
`TempDir`, and contain no `sleep`. Tests that spawn a stand-in `litestream` or `rclone` are
shell scripts written into a `TempDir` in the manner of `crates/core/src/testing/cli.rs`. They
record their argv, their environment and, on SIGTERM, a line saying so. Like that module's
scripts, they are `#[cfg(unix)]`. Every pure function they rely on is tested on all three
operating systems. Exact strings are asserted exactly. Every test that runs
`env!("CARGO_BIN_EXE_rimaia-server")` calls `env_clear()` and then sets each variable it
needs, because a developer's shell may export `RIMAIA_DATA_DIR` (CLAUDE.md) or any other
`RIMAIA_*` name.

**Configuration** (`crates/server/src/config.rs` unit tests, over literal pairs):

- `an_unknown_rimaia_variable_is_refused_by_name`, using `RIMAIA_BACKUP_UR` as the typo.
- `a_backup_url_with_credentials_is_refused`, `a_relative_file_backup_url_is_refused` and
  `an_unsupported_backup_scheme_is_refused`, each with its exact message.
- `backups_are_off_when_the_url_is_unset`, and the `warn` line is emitted.
- `a_transcript_backup_interval_under_five_minutes_is_refused`.
- `the_startup_line_names_no_secret`: with every variable set to a distinct sentinel, the
  rendered startup line contains no sentinel of a secret variable, and the `Debug` of the
  config does not either.

**The lock** (`crates/server/src/lock.rs` and `crates/server/tests/lock.rs`):

- `a_second_lock_on_one_data_directory_is_refused_naming_the_path`.
- `the_lock_is_released_when_its_holder_drops`.
- `the_binary_refuses_a_data_directory_another_process_holds`: the test holds the lock and
  runs `env!("CARGO_BIN_EXE_rimaia-server")` with a complete, valid environment. The binary
  exits non-zero, stderr names the lock path, and **`rimaia.db` was never created** in the
  fresh `TempDir`. That last check proves the lock precedes everything else.

**Backups** (`crates/server/src/backup/` unit tests, and `crates/server/tests/backup.rs`):

- `litestream_config_for_an_s3_url_is_exactly`,
  `litestream_config_for_an_s3_url_with_an_endpoint_is_exactly` (with `force-path-style:
  true`) and `litestream_config_for_a_file_url_is_exactly`, golden strings in Litestream
  0.5's format, each with `retention: 720h` under `snapshot`.
- `the_rendered_litestream_config_holds_no_credential`.
- `the_transcript_sync_argv_is_exactly`, with the `--backup-dir` stamp taken from a
  `TestClock` set to `2026-10-01T02:00:00Z`. `the_deleted_transcript_expiry_argv_is_exactly`
  and `the_transcript_restore_argv_is_exactly`, for both schemes.
- `a_fresh_volume_restores_the_database_before_it_is_opened`: the stand-in `restore` copies
  a prepared board into place. The server then serves a row that only that board holds, and
  the stand-in's argv carries `-if-db-not-exists -if-replica-exists`.
- `an_existing_database_is_never_restored_over`: no `restore` invocation at all.
- `transcripts_are_restored_only_when_the_database_was`.
- `a_failed_restore_refuses_to_start`.
- `a_litestream_child_that_exits_is_restarted_with_backoff`: the stand-in exits at once. After
  advancing the `TestClock` one second there are two spawns, after two more seconds three,
  and `rimaia_litestream_restarts_total` is `2`.
- `transcript_backup_runs_on_the_injected_clock`: no rclone call before the interval, one
  sync and one expiry after advancing it once, and two of each after twice.
- `a_failed_transcript_backup_is_counted_and_retried_next_interval`.
- `children_receive_only_the_backup_environment`: with `AWS_ACCESS_KEY_ID`,
  `RIMAIA_GITHUB_CLIENT_SECRET` and `CLAUDE_CODE_SESSION_ID` set in the parent, the recorded
  child environment has the first and neither of the others.
- `shutdown_closes_the_pool_then_stops_litestream`: the stand-in, on SIGTERM, writes a
  marker and exits. The test's shutdown hook records the pool's `is_closed()` at the moment
  the server runs `kill`, and the test asserts the pool was already closed then, and that
  the marker exists afterwards. Order is the assertion, not just both effects.
- `an_open_event_stream_ends_on_shutdown`: an SSE stream opened on `/api/v1/events` reaches
  end-of-body once shutdown starts, and the shutdown finishes without waiting out its
  10 seconds.

**Health and metrics** (`crates/server/tests/ops.rs`, through a real listener and `reqwest`,
as 046's suite does):

- `healthz_is_ok_with_the_version_and_protocol`, with the exact body.
- `healthz_is_503_when_the_database_is_closed`, with the exact body and no path in it.
- `healthz_needs_no_credential_and_sets_no_cookie`.
- `metrics_are_not_served_on_the_public_listener`: `GET /metrics` on the public router is
  `404`.
- `metrics_render_exactly`. It uses 039's two-team fixture on a `TestClock`, with:
  - one runner seen a minute ago, one ten minutes ago, and one unpaired;
  - one live lease and one expired lease;
  - transcripts on both teams, one of them pruned;
  - a review run's transcript and a patch;
  - three requests, one of them to an unknown path;
  - two observed claim durations.

  The whole rendered body equals a golden string.
- `storage_is_labelled_by_team_id_and_never_by_name`: the team is named with a sentinel, and
  the sentinel does not appear.
- `every_kind_of_run_counts_toward_storage` (D29).
- `a_deleted_team_drops_out_of_storage_metrics`: a team deleted with 051's deletion service
  has no `rimaia_team_storage_bytes` line on the next scrape (051's hand-off to this task).
- `an_unmatched_path_is_counted_as_unmatched_not_by_its_uri`.
- `claim_timed_measures_the_last_attempt_not_the_wait`, a `rimaia-core` test in
  `crates/core/src/board/service.rs`: a `Next` claim with a `wait` parks, the `TestClock`
  advances 20 seconds, work arrives, and the returned duration is zero.
- `claim_latency_excludes_the_long_poll_wait`, the same sequence through 052's HTTP
  handler: the one observation falls in the lowest bucket.
- `ops::snapshot` and `ops::ping` are the only new entries in
  `no_service_takes_a_pool_without_a_scope`'s allowlist, each with its reason in a comment.
  No file under `crates/core/src/api/` or `crates/core/src/mcp/` names `ops::`.

**Logs** (`crates/server/tests/log_hygiene.rs`, its own test binary, because it installs a
global subscriber):

- `log_lines_never_carry_board_content`. It installs the production subscriber over a
  buffer with `RIMAIA_LOG=rimaia_server=debug,rimaia_core=debug,info` and drives the server
  over HTTP. Each category Scope 6 lists carries its own sentinel, including one in a
  `/mcp` tool call's arguments and one in a `finish_run` patch, and some requests succeed
  while others fail validation. The test then asserts that the buffer is non-empty,
  names a matched route, and contains no sentinel.
- `the_default_log_filter_is_exactly` `rimaia_server=info,rimaia_core=info,warn`.

**The client address** (unit tests of `client_address`, and one in 047's rate-limit suite):

- `with_no_trusted_hops_the_client_is_the_peer`.
- `one_trusted_hop_takes_the_rightmost_forwarded_address`, with a forged left-hand entry
  present.
- `too_few_forwarded_entries_fall_back_to_the_peer` and
  `an_unparseable_forwarded_entry_falls_back_to_the_peer`.
- `clients_behind_one_proxy_get_separate_rate_limit_buckets`, with hops set to 1.

**Headers** (in 050's bundle suite):

- `the_bundle_is_served_with_the_production_policy`, asserting the exact
  `Content-Security-Policy` string from Scope 8.
- `hsts_is_sent_only_for_an_https_public_url`.

**The image** (`crates/server/tests/image.rs`, reading the files as text):

- `the_dockerfile_builds_offline`: it sets `SQLX_OFFLINE=true` in the build stage and builds
  with `--locked -p rimaia-server`.
- `the_dockerfile_names_no_rust_version`: no `FROM rust:` tag other than `1-…`.
- `litestream_and_rclone_are_pinned_by_version_and_checksum`: one exact version each, a
  `sha256sum -c` for each, an archive and a checksum for both `amd64` and `arm64` selected
  by `TARGETARCH`, and a comment naming the upstream checksum file's URL.
- `the_runtime_image_runs_as_a_non_root_user`.
- `the_default_stage_is_runtime_without_the_seed`: the last `FROM` line is
  `FROM runtime-base AS runtime`, and that stage copies no `rehearsal_seed`.
- `the_runtime_image_has_kill`: `runtime-base` installs `procps`.
- `dockerignore_keeps_the_migrations_and_both_query_caches`: no line of `.dockerignore`
  matches `src-tauri/migrations`, `crates/core/.sqlx` or `crates/runner/.sqlx`, including
  `.*`, `**/.*` and `*.sqlx` shapes.

**Documents and repository**:

- `docs/hosting.md` exists with every section Scope 10 lists, including deploying and
  upgrading, the stop grace, and the two steps after a point-in-time swap. Its
  configuration table names exactly the variables `config.rs` accepts. A unit test
  compares the two lists, so the document cannot drift from the code.
- `rimaia-server`'s manifest takes `tracing-subscriber` with `{ workspace = true }` and
  adds no other dependency. `cargo tree -d` shows one `tracing-subscriber`, and
  `Cargo.lock` gains no package.
- 062 has a row in seam-contract's "How to use this" table: D4 · D5 · D6 · D8 · D11 · D19 ·
  D28 · D29 · D31 · D32 · D33 · D34.
- CLAUDE.md carries Scope 11's edits, and `## Commands` still equals `ci.yml` line for line.
- Both `.sqlx/` caches are regenerated with D33's recipe and committed.
- **Every CI check passes**, run with `SQLX_OFFLINE=true` exported: `npm run typecheck`,
  `npm run test`, `npm run build`, `cargo test -p rimaia-core`, `cargo test -p
  rimaia-runner`, `cargo test -p rimaia-server`, `cargo fmt --all --check`, the three
  `cargo clippy … --all-targets -- -D warnings` lines, `cargo check --workspace
  --all-targets`, `./scripts/check-command-wiring.sh` and
  `./scripts/check-crate-boundaries.sh`.

**Needs a human, and listed as a checklist in the PR body:**

- `scripts/rehearse-restore.sh --replica file` and `--replica s3` both print `PASS` on every
  step. Their output is pasted into the PR.
- The web shell, served by the built image, loads with the production policy and shows no
  CSP violation in the browser console: sign-in screen, board, and a task with a review
  bundle.
- `docker run` of the image with only the required variables starts, and `/healthz` answers.

## Notes

**Seam entries to read.** D4 and D6 as prohibitions: no migration, and no dependency beyond
Scope 11's line. D5: the cache rules D33 leaves standing. D8: no new error code; a refused
start is a process exit, not an `Error`. D11: what a refusal at startup looks like. D19:
why `fs4` stays confined. D28: `runs.transcript_bytes`, `transcript_pruned_at`,
`review_bundles.patch`, `runner_leases`, `runners.last_seen_at`, and its note that sign-in
state stays in memory because Litestream would put it in backups. D29: the storage reader's
answer about kinds. D31: the claim body `claim_timed` splits, and `runner_api`. D32:
`/healthz` and `/metrics` are not board routes, and every board route still takes a
`Caller`. D33: the Binds line for 062, and the recipe. D34: the `tracing-subscriber` row,
and the binaries in the image that it does not govern.

**Files to start from.**

- Present on `main`:
  - `crates/core/src/db/mod.rs`: `MIGRATOR`'s `../../src-tauri/migrations`, and WAL mode,
    which Litestream requires and `db::connect` already sets.
  - `crates/core/build.rs`.
  - `crates/core/src/clock.rs` and `crates/core/src/testing/clock.rs`: `sleep_until` for the
    two schedules.
  - `crates/core/src/testing/cli.rs`: how a stand-in program is written and its argv
    recorded.
  - `crates/core/tests/runner_credentials.rs`: the `#![cfg(unix)]` precedent.
  - `crates/core/src/paths.rs`: `AppPaths::resolve`'s override rules.
  - `crates/core/src/runner/process.rs` (or wherever 041 moved it): `KILL` and
    `signal_group`, the argv shape Scope 4's shutdown copies.
  - `src-tauri/src/logging.rs`: `RIMAIA_LOG` and the filter idiom.
  - `.github/workflows/ci.yml`: the `libdbus-1-dev` step the build stage mirrors.
  - `rust-toolchain.toml`, the root `Cargo.toml`, `package.json` (`"build": "tsc && vite
    build"`) and `.gitignore`.
- Created on this branch by earlier tasks:
  - `crates/server/src/main.rs`, `crates/server/src/lib.rs` (`ServerState`, `router`) and
    046's config parser, extended by 047 and 050.
  - 047's rate limiter.
  - 050's bundle middleware.
  - 042's claim body in `crates/core/src/board/service.rs`, with 053's long poll, and 052's
    `crates/server/src/runner_api.rs`.
  - 053's lease-lifetime constant.
  - 048's `StreamControl`.
  - 056's `FileTranscriptStore` and where `main` builds it.
  - 058's `[workspace.dependencies]` lines for `tracing-subscriber`.
  - 060's `/mcp` door.
  - 051's deletion service.
  - 039's `no_service_takes_a_pool_without_a_scope`.

  Read them as they landed. If one is not where this file says, follow the code and say so
  in the PR body.

**Migration.** None. Every column this task reads exists by 056.

**What the chain provides, and what comes next.**

- 046 provides the binary, the config parser and the trace layer that records routes, not
  URIs.
- 047 provides three variables and a limiter keyed by the peer.
- 050 provides the bundle, `RIMAIA_WEB_ROOT` and the header middleware.
- 052 and 053 provide the claim handler and the lease lifetime.
- 056 provides stored transcripts with byte counts, and 068 prunes them.
- 058 provides `tracing-subscriber` on the workspace line. 060 provides `/mcp`, whose tool
  arguments the hygiene test covers. 051 provides team deletion.
- 063 builds the desktop updater and shows out-of-date runners. It needs nothing from this
  task except a server that stays up.
- 064's final pass reads `docs/hosting.md`, confirms CLAUDE.md still matches CI, and decides
  whether CI should build the image on pushes to `main`. Its Scope carries that question.
- 065 drops `runs.log_path`, which nothing in this task reads.

**Why the server starts Litestream rather than `litestream replicate -exec`.** `-exec` is
Litestream's usual pattern, and it makes Litestream the parent. Here that would put a second
Litestream on a shared volume before the child server could find the lock held. An
entrypoint that took the lock with `flock` would hold it on a descriptor the server cannot
see, so the server's own `try_lock` would then refuse its own start. With the server as the
parent, the lock, the restore, the migration and replication happen in one order, in one
process, under one clock, and every step is testable with a stand-in. The cost is about 200
lines of supervision, and ADR-0037 point 1's "fails loudly instead of writing to the same
file" is only true this way round.

**Why rclone, and why the server schedules it.** Litestream replicates one SQLite file, not
a directory. D34 declines an object-storage client inside the server, and it was right to:
ADR-0036 point 7's store stays on disk. A backup tool run as a child is a different thing,
and D34 names it beside Litestream, so its list stays literally true. Scheduling it in the
server, rather than with cron or a shell loop, puts the interval on the injected clock and the
result in a metric, and those are what make "backed up on a schedule" something a test can
check.

**Open sign-up, stated rather than discovered.** 047 notes that nothing in ADRs 0027 to 0037
limits which GitHub accounts may sign up to the hosted instance, and that if it must admit
only some accounts "before 062 deploys it", that is a new ADR decision. This task does not
deploy, and adds no allowlist. The runbook lists the question among the preconditions for
going live, next to the data processing agreement.

**Size.** The plan sized this M. The estimate puts it at L, near 047's size:

| Part | Lines |
| --- | --- |
| Configuration and the lock, with their tests | about 600 |
| Backup supervision and its tests | about 800 |
| Metrics, `ops.rs` and their tests | about 800 |
| Logging, the client address and the headers, with tests | about 500 |
| `Dockerfile`, `.dockerignore` and the image tests | about 200 |
| The runbook, the rehearsal script and the seed example | about 650 |

That is roughly 3,500 lines, before the regenerated caches. If it runs over, cut in this
order, and amend the receiving task's file in the same commit:

1. **The rehearsal's `s3` mode, the point-in-time step and the `rehearsal` stage.** The file
   mode's graceful restore stays and still proves the path. The runbook keeps the dropped
   steps as manual ones, and 064's file gains them as a list in the same commit.
2. **Per-route HTTP counters** collapse to one counter by status class. ADR-0037 point 7's
   "request and error metrics" still holds.
3. **Scope 8's headers** move to a new task appended after 063. The hosted instance does not
   go live until that task lands, and the runbook says so.

The lock, the backups, the restore, health, runner, lease and storage metrics, and log
hygiene are not cuttable. They are ADR-0037's decisions, and they are the reason this task
exists.
