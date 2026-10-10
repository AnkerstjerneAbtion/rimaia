# 37. Hosting, backups, and version skew between the server and its runners

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

Until now Rimaia shipped one artefact, a desktop bundle, and every installation upgraded as a
unit (task 018). ADR-0027 adds a server that Rimaia hosts for many teams (ADR-0029), and
runners on every member's machine that upgrade when their owners get round to it. This record
covers what the server runs on, how its data survives, and what happens when the server and
a runner are on different versions.

The shape of the server is fixed by earlier records:

- one process owning one SQLite file (ADR-0028);
- transcripts on its disk (ADR-0036);
- an HTTP API, SSE and MCP on one origin (ADR-0034, ADR-0035).

## Decision

### 1. One container image, run as a single instance

`rimaia-server` ships as a container image holding the server binary and the built web app
(ADR-0034 point 6). It is configured by environment variables:

- data directory;
- public URL;
- GitHub OAuth client;
- backup destination.

It runs as **exactly one instance** with a persistent volume. That is SQLite's condition
(ADR-0028). The deployment pins the instance count to one. The server also takes an
exclusive lock file in its data directory at startup, and refuses to start without it. A
misconfigured second instance then fails loudly instead of writing to the same file.

The hosted instance is run by Abtion on a container platform with persistent disks, in an EU
region. The platform itself is a hosting choice, not an architectural one, and is left to
whoever sets it up. The same image is supported for self-hosting by any organisation that
needs its own boundary (ADR-0029's first alternative).

### 2. The database is continuously backed up

The server's SQLite file is replicated continuously to S3-compatible object storage with
Litestream, running in the same container, with point-in-time restore. Transcripts (ADR-0036)
are backed up to the same bucket on a schedule. A restore is tested before the instance holds
anyone's data, and after that on a regular schedule. An untested backup is a hope.

### 3. Downtime is acceptable, and runners must not notice it

Deploys stop the old container and start the new one, and migrations run at start (ADR-0003).
Zero-downtime deploys are not a goal for a single-instance server. Runners and clients are
built so a restart costs nothing:

- long polls and SSE reconnect with backoff (ADR-0031, ADR-0034);
- reports wait in the runner's outbox (ADR-0036);
- the lease lifetime of three minutes (ADR-0031) is longer than a normal deploy, so a deploy
  does not interrupt running work.

A deploy that takes longer than a lease lifetime interrupts running work, which then resumes
on the same runners, pinned (ADR-0031). Nothing is lost.

### 4. The protocol is versioned, and the server decides compatibility

Every runner and desktop request carries `Rimaia-Protocol: <major>.<minor>`, alongside the
`/v1` path (ADR-0034 point 7). The server supports its current minor version and the one
before it:

- **Supported:** served normally.
- **Older than supported:** the server answers board reads, so a person can still see their
  board, but refuses claims and reports with an error naming the minimum version. **A runner
  that is too old stops taking work rather than taking it and misreporting it.** This is
  fail-closed, as elsewhere in the runner (ADR-0020, ADR-0032).
- **Newer than the server:** refused the same way. A runner cannot be ahead of the board it
  reports to.

Solo mode is one process on one version and has no skew.

### 5. The desktop app updates itself

The desktop bundle gains Tauri's updater, with signed updates, and checks at launch. A
connected desktop whose runner has been refused for its version offers the update
immediately. Headless runners report their version to the server, and the web UI shows each
member which of their runners are out of date. This extends task 018's packaging, which
built bundles but not updates.

### 6. Hosting other teams' data makes Abtion a data processor

The hosted instance stores plans, diffs, patches and transcripts from repositories that
belong to other organisations (ADR-0036). Before a team outside Abtion uses it:

- a data processing agreement is in place;
- the hosting region is stated;
- retention is configurable per team (ADR-0036 point 6);
- deleting a team or an account deletes its data from the live database immediately. The data
  ages out of backups when the backup retention period (30 days) passes, because
  point-in-time backups cannot be edited selectively (ADR-0029 point 6);
- server logs are treated as containing team data: they are kept for a bounded period,
  access is restricted, and no plan or transcript content goes into log lines.

Solo mode sends nothing to any Rimaia server, and remains the answer for anyone who cannot
accept a processor at all.

### 7. The instance is observable without reading team data

The server exposes:

- a health endpoint;
- request and error metrics;
- runner counts, active leases and claim latency;
- storage per team.

Operators diagnose from these, not from task or transcript content. Access to the production
database is limited to named administrators and is recorded.

## Consequences

- **Rimaia has an operations surface now:** deploys, backups, restores, updates and incident
  response. None of it existed while the product was a desktop bundle.
- **Version skew is loud, not silent.** A stale runner stops claiming and says why, instead
  of corrupting run records with an older understanding of the protocol.
- **Single-instance hosting caps availability.** A host failure is downtime until the volume
  or backup is restored. Runners ride it out, and the board is unavailable meanwhile. Removing
  that cap is ADR-0028's Postgres decision, taken when downtime costs more than the port does.
- **Legal groundwork comes before the first external team.** The data processing agreement
  and the retention and deletion behaviour are preconditions, not follow-ups.

## Alternatives considered

- **One instance per organisation, operated by Abtion.** Stronger isolation, and N
  deployments to upgrade, back up and monitor. Rejected as the hosted default by ADR-0029, but
  supported through the same image for anyone who needs it.
- **Serverless or multi-instance hosting.** Scales to zero or out, and neither works with
  one process owning one SQLite file. Revisit with ADR-0028's Postgres trigger.
- **Lockstep versions (every client must match the server exactly).** Simplest rule, and it
  makes every server deploy break every runner until its owner updates, overnight included.
  A one-minor-version window turns that into a routine update prompt.
- **Nightly backups instead of continuous replication.** Simpler. It loses up to a day of a
  team's board, which is exactly the day that team planned.
