---
id: "054"
title: Repositories by remote, checkouts by runner
milestone: v0.5
status: ready
depends_on: ["052", "053"]
adrs: ["0033", "0020", "0025", "0028", "0029", "0031", "0032", "0034"]
size: L
---

# Repositories by remote, checkouts by runner

## Goal

Make [ADR-0033](../docs/adr/0033-repositories-belong-to-the-team-checkouts-to-the-runner.md)
points 1, 2, 6 and 8 true. After this task:

- **A board repository is identified by its normalised remote** (`github.com/owner/repo`),
  computed by one function in `rimaia-core`. Two repositories in one team cannot share one.
  A repository in a personal team may still have no remote, which is the local-only case
  solo keeps.
- **A runner maps a board repository to a local clone**, and refuses the mapping unless the
  clone's `origin` normalises to that repository's remote. The mapping is one core function
  over `&dyn BoardPort`, so solo, the connected desktop and 058's headless CLI call the same
  code.
- **A runner reports what it maps**: the mapped set, its consent for each mapping, a push
  check for each, and its last doctor result. The board keeps the report and lists, for
  each repository, the runners that map it, with each one's consent and push state.
- **The board refuses a Run now or a claim aimed at a runner that has not mapped the task's
  repository**, before any relayed request or lease exists.
- **Forge credentials are keyed by (repository, runner)** in the runner's keychain. Every
  existing item is copied to the new key once, at first launch, and the old item stays
  until 065.
- **The on-archive policy is configuration of one mapping.** It never crosses to the board,
  and no report can carry it.
- **Cleanup after an archive or a move to `done` runs on the runner that holds the
  worktree**, wherever that runner is, and an archive's result reaches the board.
- **The doctor checks that this runner can push to each mapped remote**, before the queue
  starts rather than after a night of failed runs.

**Solo behaviour does not change**, apart from two more doctor rows per repository and one
more clause on one refusal (point 7). Adding a repository by choosing a folder works as it
does today, remote or not. The screens that show who maps a repository, the board's warning
line, the browser's register form and each runner's doctor result belong to 061's runner
half, which 061 splits into a task of its own. This file calls it 061. This task ships the
commands, DTOs and stored facts those screens draw from, and the rules and sentences they
must use (Notes, "What the next tasks expect").

## Why now

052 gave the runner an HTTP board adapter and the board a runner route with a
`RunnerCaller`. Until now every runner that reached the board through it was a runner whose
repositories the board already knew by path, because only solo existed. The four tasks that
follow each need a runner that knows its repositories without a board file:

- **055 and 057** push to the remote on the runner's behalf and check a postcondition there.
  Both assume the clone the runner holds is a clone of the repository the board means. Today
  nothing checks that.
- **058's headless runner** has no board file at all. The checkout mapping is the only way
  it can know which repositories it serves.
- **059's connected desktop** maps the clones its user already has to team repositories.
  Without identity by remote there is nothing to map them to.

ADR-0033 point 6 also cannot wait for a second runner. Every credential a solo user has
saved is keyed by repository id alone. The first connected runner that reads that key would
read another machine's intention. Copying the key while there is still one runner per board
is mechanical. Doing it later means guessing which runner each item belonged to.

## Scope

### 1. The two migrations

- `src-tauri/migrations/20261003120500_repositories_by_remote.sql`: seam-contract D28 part
  6's DDL for this file, as D28's 2026-10-04 amendment left it, with
  `runner_repositories.unattended_consent`, `tasks.cleanup_pending` and
  `tasks.archive_outcome`.
- `crates/runner/migrations/20261003130200_checkout_mapping.sql`: D28 part 6's DDL,
  unchanged.

Each file gets a header comment in the voice of the existing migrations. Its first line is
its title and never begins with `-- no-transaction` (D28 part 1). Neither deletes a row.
Regenerate **both** offline caches with D33 point 3's recipe, exactly as written, and commit
them.

### 2. One normalisation function

`crates/core/src/repo/remote.rs` defines `NormalizedRemote`, a newtype over `String`. It is
the only code in the workspace that decides whether two remote URLs name one repository.
`NormalizedRemote::parse(url: &str) -> Option<NormalizedRemote>` applies these rules, in
order:

1. Trim surrounding whitespace.
2. Recognise the shapes git itself accepts: `scheme://[userinfo@]host[:port]/path` for the
   schemes `https`, `http`, `ssh`, `git`, `git+ssh` and `ssh+git`, and the scp-like
   `[user@]host:path`. The scp-like shape is refused when the part before the colon is one
   character (a Windows drive letter) or contains `/` or `\`, as `host_from_remote_url`
   already decides.
3. Drop the scheme, the userinfo (user and password, or a token) and the port.
4. Lowercase the host and the path. ADR-0033 point 1 names case as a difference to remove.
5. Strip trailing `/`, then one trailing `.git`, then trailing `/` again. Collapse empty
   path segments.
6. The result is `host/segment/segment…`, with at least one path segment. Nested groups
   (`gitlab.example.com/group/sub/repo`) keep every segment.

A bare filesystem path, a `file://` URL, and anything with no host return `None`. These are
not identities two machines share. For a solo repository a `None` means the same as no
`origin`: remote-less.

`NormalizedRemote::from_normalized(s: &str) -> Option<NormalizedRemote>` accepts only the
canonical form rule 6 produces: no whitespace, no uppercase, no scheme, a host with no `@`
or `:`, at least one path segment, no empty segment, no trailing `/`. It is how the board
re-validates a remote a runner reports (point 6), and `NormalizedRemote`'s `Deserialize`
goes through it, so `RepositoryLookup::ByRemote` cannot carry an invalid value. `parse`
cannot do that job, because `github.com/owner/repo` has neither a scheme nor an scp colon
and so parses to `None`.

`repo::git::host_from_remote_url` is reimplemented on top of the same parser, so one parser
reads remote URLs. Its existing tests pass unchanged.

`NormalizedRemote` holds no credential by construction, so it may be logged, stored and
sent. **The raw URL may not.** Nothing this task writes to the board, to a report, to a
change event or to a log line is a raw remote URL.

No new dependency. The scp-like shape is not a URL, so a URL crate would not help (D6, D34).

### 3. The board side: register by remote

**`Repository` gains `remote: Option<String>`**, the normalised form: in Rust, in
`src/types.ts` and in the MCP `RepositoryView`. It is the only remote field on any board
DTO.

**`register_repository` splits** (D32's appendix row), and its flip from `local` to `board`
is one commit: registry row, handler moved into `crates/core/src/api/board/`, wrapper
switched to `board<T>` (D32 point 8). The board half is
`repo::register(ctx, NewRepository { name, remote, default_branch })`:

- `remote` is a raw URL from the caller. It is parsed with `NormalizedRemote::parse`, and
  only the result is stored in `repositories.normalized_remote`.
- In a team that is not personal, a missing remote, or one that parses to `None`, is refused
  as `Invalid`, exactly: `a team repository needs a remote URL that another computer can
  reach, because the remote is the only thing two computers share`.
- In a personal team `remote` may be absent. The row is then remote-less.
- A second repository in the same team with the same normalised remote is refused as
  `Invalid`, exactly: `this team already has <remote> registered, as "<existing name>"`. The
  service checks before it inserts. `idx_repositories_team_remote` is the store's backstop,
  and a unique-violation from it is mapped to the same sentence, never surfaced as
  `Internal`.
- The same remote in two teams is two repositories, and neither team learns the other
  exists (ADR-0029 point 5, and 039's "registration is team-local").
- Registering requires the `Owner` role in the team (ADR-0029's roles table), checked the
  way 045's ceiling command checks it. A member is refused as `Invalid`.
- `name` defaults to the last path segment of the normalised remote, or, remote-less, to
  what task 003's `naming::derive_name` gives for the clone. `default_branch` is required
  when there is no clone to read it from.

**One-step registration from a clone.** The folder flow a desktop user has today becomes the
local command `add_repository_from_clone { path, name? }`. It runs task 003's four
validations on the path. It reads `origin` and the default branch from the clone, dispatches
the board `register_repository` with the solo or desktop `Caller` (D32 point 8: a local
handler never reads the board's `ServiceContext`), and then maps the clone through point 4's
function. If the mapping step fails after the board row was written, the board row stays and
the repository shows "Not set up on this computer" (066's state), with the mapping error
returned. Nothing is rolled back across the two stores, because nothing can be.

"Add repository" is `src/components/RepositoryAddForm.tsx`, which both `RepositoriesSection`
and `src/views/WelcomeView.tsx` render. It calls `add_repository_from_clone` instead of
`register_repository`, with the same fields and the same error rendering. 050's browser gate
on it stays, because the clone flow is still a local command. Its comment now names
`add_repository_from_clone`, and 061 replaces the gate with the form that registers by
remote.

### 4. The runner side: map a clone

**One core function maps.** `machine::checkouts::map(machine: &MachineContext, board: &dyn
BoardPort, repository_id, path) -> Result<Checkout>` reads the board only through
`board.find_repositories` (D31's 2026-10-04 amendment). It never reads it through the
dispatcher, because a headless runner holds only an `rmr_` token, which 047 refuses on every
board route. The desktop's local command passes `AppState.board_port`, and 058's CLI passes
its `HttpBoard`.

- **`map_repository_checkout { repositoryId, path }`** calls it. The function asks for
  `RepositoryLookup::ById`, and an empty answer is `NotFound`, the same for another team's
  id as for a missing one. It runs task 003's four validations on `path`, then:
  - if the repository has a remote, requires `origin` to normalise to it. Otherwise it
    refuses as `Invalid`, exactly: `<path> is a clone of <clone's normalised remote>, not of
    <repository's remote>`. With no `origin`, or one that parses to `None`: `<path> has no
    origin remote another computer could reach, so it cannot be a clone of <remote>`.
  - if the repository is remote-less, allows it only while `served_remote_less_by` is
    `None`. Otherwise: `"<name>" has no remote, so only the computer that holds its clone
    can run it`. ADR-0033 point 1: a remote-less repository "is then identified by its
    clone alone".
  - refuses a `path` this runner already maps to another repository, unless both
    repositories have the same non-`NULL` normalised remote. That is the one legitimate
    sharing, one clone serving the same forge repository for two teams, which 039 left to
    this task. Paths are compared canonicalised.
  - writes the checkout with `normalized_remote` and `remote_verified_at`. On a re-map of
    an existing checkout it keeps `max_concurrency`, `unattended_consent`, `on_archive`,
    `on_archive_script` and the credential metadata. A re-map moves the clone; it does not
    reset the machine's choices about it.
- **`unmap_repository_checkout { repositoryId }`.** It removes the checkout. It refuses
  while `worktrees` holds a row for it (041's `RESTRICT`), exactly: `"<name>" still has
  <n> worktrees on this computer; remove them first`. In the same call it deletes the
  keychain item under the checkout's key and under the pre-054 account (point 7), so an
  unmapped repository leaves no secret behind.

Both commands, and every other mapping change (consent, on-archive), send a report (point 6)
after the write.

**Every checkout is verified at startup.** One function,
`machine::checkouts::verify_remote(&Checkout, &RepositoryRef) -> RemoteVerification`, reads
`origin` and parses it. The startup pass and the doctor's `CheckoutRemote` (point 8) both
call it, and nothing else makes that decision. The pass runs at every runner start, after
adoption (`credential_keys` included) and before the first report or queue start. It asks
`find_repositories` for every checkout's id and writes each result to the checkout:

- the board's remote is `NULL`, or equals what `origin` parses to: `normalized_remote`
  becomes `origin`'s parse (`NULL` when that is `None`), and `remote_verified_at` becomes
  now;
- the board has a remote and `origin` parses to something else, or to `None`:
  `remote_verified_at` becomes `NULL`;
- the board does not know the id: the checkout is left as it is, and stays out of this
  report.

If the board cannot be reached, the pass changes nothing and runs again before the next
report is sent.

These terms have one meaning each everywhere in this task. A checkout is **verified** when
its `remote_verified_at IS NOT NULL`. A repository is **remote-less** when the board's
`repositories.normalized_remote IS NULL`. `CheckoutReport.normalized_remote` is the
checkout's column, never a fresh read of `origin`. On an upgraded install, every checkout
041 adopted starts with both columns `NULL`. This pass is what fills them, and the first
report is what teaches the board each remote (point 6). Without it, every repository would
either look remote-less or be unverified and drop out of `Next`, which would silently empty
the solo queue.

**The mapping is re-verified before every spawn.** At the point where the runner resolves
the checkout for a claimed task (066's `worktree::prepare` path), it reads `origin` again and
compares its normalised form with `RunContext::repository.remote`. A mismatch releases the
lease and records the refusal, exactly as 045's pre-spawn consent re-check does, and spawns
nothing. A clone re-pointed after it was mapped would otherwise push one team's plan into
another repository. A remote-less repository skips the check.

**`list_checkouts`** (066) gains `normalizedRemote` and `remoteVerifiedAt`.

### 5. On-archive is configuration; cleanup runs on the holding runner

066 already moved the readers of `on_archive` and `on_archive_script` to `checkouts`. This
task makes the rule structural rather than incidental:

- `set_repository_on_archive` addresses a mapping. On a repository this runner has not
  mapped it is refused with 066's "not set up on this computer" sentence.
- Unmapping deletes the policy with the checkout.
- `RunnerReport` (point 6) has no field that could carry a policy or a script path. A test
  serialises a fully populated report and asserts its exact key set, so a field added later
  is a visible diff.

No team member can author a command for another member's machine (ADR-0032 point 5), and
after this task the type system says so.

**The cleanup a remote runner owes** (D31's 2026-10-04 amendment, D28's columns). 041 gave
`archive_task`, `archive_tasks`, `move_task`, 034's `review::approve` and `review::reject`
one core function each, taking `Option<&MachineContext>`:

- **With a machine (solo)** they react in the same call, as today, and also record the
  archive's `ArchiveOutcomeSummary` in `tasks.archive_outcome`, through the same
  `board::service` function a report uses.
- **Without one (a server)**, for a task that has a `runs` row naming a runner, archiving
  sets `cleanup_pending = 'archived'` and clears any earlier `archive_outcome`. Entering
  `done`, through `move_task` or `approve`, sets `'done'`. The archived entry's
  `OnArchiveOutcome` is a new variant, `Pending`, instead of 041's `Nothing`. Unarchiving
  clears `'archived'`, and moving out of `done` clears `'done'`. `review::reject`'s machine
  half is a guard that runs before its write, not a reaction after it, so on a server it
  stays skipped, as 041 left it.
- **The runner reacts on its heartbeat** (053's loop). For each `CleanupRequest` it calls
  the function solo would have called: `archive::run_on_archive` with the checkout's policy,
  D26's guards and timeout for `Archived`, and D20.3's best-effort removal under its own
  `worktree_auto_cleanup` for `Done`, whose refusal is logged on the runner and never
  reported. It then appends a `CleanupDone` to its next report. While that entry waits in an
  unsent report, a repeated request is not run again. Only a restart that loses the unsent
  report can run a script a second time for one archive, and the PR names that case.
- A task with no `runs` row naming a runner (planned, never run) gets no flag. A worktree
  its planner left stays until someone removes it on that machine. The PR says so.

`TaskDetail` gains `archiveOutcome: ArchiveOutcomeSummary | null`. 030's `describeCleanup`
in `src/lib/archive.ts` renders `Pending` as, exactly: `Cleanup runs on the computer that
holds the worktree.` Showing `archiveOutcome` on the archived task is 061's.

### 6. The board side of the two leaseless methods

D31's 2026-10-04 amendment gives `find_repositories`, `report_runner`, every DTO and the
heartbeat's `cleanup` field. This section says what `board::service` does with them.

**`find_repositories`** reads within the teams the runner's owner belongs to. `ById` returns
zero or one row, and `ByRemote` returns one row per matching team.
`served_remote_less_by` names another runner, never the caller. That runner is not
unpaired, its owner is still a member, and it reports this remote-less repository.

**`report_runner`**, in one transaction:

- **Checkouts are a snapshot.** The runner's `runner_repositories` rows become exactly the
  reported set. Rows it no longer lists are deleted. `reported_at` is the board clock's now
  on every row the report keeps.
- **An id the runner's owner cannot see is unknown.** A repository in a team the owner is
  not a member of, and one that does not exist, get the same answer: listed in
  `unknown_repositories`, with nothing written for it. Never `NotFound` for the whole report,
  which would let one removed repository silence every other. The runner leaves unknown ids
  out of `ClaimTarget::Next.repositories` until a later receipt stops naming them.
- **The remote-less backfill.** D28's comment on `normalized_remote` says a solo board
  learns each remote from the runner's first report. For each reported checkout whose
  repository has `normalized_remote IS NULL` and is in a personal team, the board writes the
  reported remote, unless another repository in that team already holds it. In that case
  the repository stays remote-less and keeps working as today, and nothing is refused. The
  value must pass `NormalizedRemote::from_normalized`. One that does not is never written to
  `repositories`, and the checkout's row is kept. A repository in a team that is not
  personal is never written by a report, and a non-`NULL` remote is never rewritten by one.
  A runner does not get to re-point a team's repository.
- **The doctor summary** replaces `runners.doctor_report` and `doctor_reported_at` when
  present. It carries check ids, statuses and repository ids, and **no prose**: every
  `detail` and `remediation` today can name an absolute path (`repository_path`,
  `data_directory`), and ADR-0028 point 2 keeps paths off the board. The machine that ran
  the doctor still shows the full rows. The board stores a check id it does not know as it
  came, so a newer runner's report never fails to parse (D31 point 6).
- **`branches_deleted`** is 066's leaseless branch clear in `worktree::remove`, which 066
  left with a comment naming this method. The board sets `tasks.branch = NULL` only where it
  still equals the reported `branch` and the task is in a team the owner is a member of. A
  resend is a no-op. `worktree::clear_branch` goes through the port from here on.
- **`cleanups`**: for each entry whose task is in one of the owner's teams, whose latest
  run names this runner, and whose `cleanup_pending` still equals the trigger, the board
  clears `cleanup_pending` and, for `Archived`, writes `archive_outcome`. A resend is a
  no-op.
- **Change events.** The board publishes `ChangeEvent::repositories` for the repositories
  whose rows changed, and `ChangeEvent::tasks` for cleared branches and recorded cleanups,
  each tagged with its team (038). A report that changes nothing publishes nothing, so a
  runner retrying a report does not refresh every board.

**When a runner reports.** At startup, after the verification pass and before the queue can
start or the first command is served. After every doctor run. After every mapping, consent
or on-archive change. After a cleanup or a leaseless branch delete. In process the call is
synchronous. Over HTTP (`POST /api/v1/runner/report_runner`, authenticated by 052's
`RunnerCaller`), a failed send marks the report dirty in memory, and 053's `on_tick`
resends it until it succeeds. The checkout set and the doctor summary are latest-only. The
two event lists accumulate until a send succeeds. It does not go through 056's outbox, which
holds lease-bound reports in order. If the runner restarts with an unsent cleanup, the
board lists that cleanup again on the next heartbeat.

**The push error leaves the machine redacted.** Git's and ssh's stderr from the push check
(point 8) can carry a URL with userinfo, the injected token, and paths under the user's home
or the data directory, such as `known_hosts`, an identity file, an askpass helper or a
`pushInsteadOf` target. Before it enters a `PushCheck` it passes through, in order:

1. the credential's own `Redactor` (D25 point 5);
2. a rule that replaces the userinfo of every `scheme://userinfo@` occurrence with `***`;
3. the checkout path replaced with `<clone>`, the data directory with `<data>`, and the home
   directory with `~`, longest first, so a clone under home becomes `<clone>`;
4. only the first line that begins with `fatal:`, `error:` or `remote:` once trimmed, or the
   first non-empty line when none does;
5. a cap of 1,024 bytes on a character boundary.

**Two board reads.**

- `list_repository_runners { repositoryId? }`: a `board` `Read` row. For each repository in
  the caller's team (or the one named), one row per runner that reports it: `repositoryId`,
  `runnerId`, `runnerLabel`, `ownerLogin`, `connected`, `reportedAt`, `unattendedConsent`,
  `pushCheckedAt` and `pushError`. Rows whose runner is unpaired (`runners.unpaired_at IS
  NOT NULL`), or whose owner is no longer a member of the team, are filtered out at read
  time, so unpairing (057) and leaving a team (051) need no cleanup here. `connected` is
  false exactly for the board's solo runner (`solo_identity.runner_id`), which the board
  knows without trusting the report.
  There is **no `serving` flag**. A repository is mapped when it has at least one row.
  Consent and push are separate states of each runner, and a push error counts against a
  runner only when `connected` is true, which is point 8's fail/warn split. A solo user
  who has not opted into unattended runs, or whose push check fails, still has a mapped
  repository that Run now works in, and nothing may tell them otherwise.
- `list_runner_doctor_reports`: a `board` `Read` row, scoped to the **caller's own**
  runners, not the team's: `{ runnerId, reportedAt, report: DoctorSummary }` for each runner
  that has reported one. ADR-0034 point 5's browser doctor shows "each of the user's
  runners". A teammate's doctor result is not team data. It is the only doctor read on the
  board, and 061 renders it in 050's `RunnersSection`.

Both get a case in 046's `BOARD_CASES`, a cross-team one included, and D32's appendix gains
their rows.

**Run now and claims need a mapping.** One `board::service` function,
`ensure_runner_maps(ctx, runner_id, repository_id)`, refuses as `Invalid` when the runner
has no `runner_repositories` row for the repository, exactly: `"<repository name>" is not
set up on <runner label>`. It has two callers:

- **A fifth check in 052's "What the board checks first"** for `start_task_run` and
  `retry_task_now`, after its four and before `host.relay.run_now`. A browser Run now aimed
  at a runner that cannot serve the repository is refused synchronously, and no relayed
  request is recorded. Without this check, `RelayedRequests` would hold the request, the
  runner's `claim(Run)` would refuse it, and 052 drops a refused relayed claim silently.
- **The claim**, for `ClaimTarget::Run` and `ClaimTarget::Plan`, before any lease is
  written. This is the backstop for a runner's own claim, and for a mapping that
  disappeared between the press and the collection.

`ClaimTarget::Next` keeps using the claim's own `repositories` list, which is fresher than
any report. The runner builds that list from checkouts that are verified, consented (045),
and not named unknown by the last receipt. No `SkipReason` variant is added: an unmapped
repository's tasks are never offered to this runner, so the queue has no skip to explain.
061's board line is where a repository nobody maps becomes visible.

**The test harness records the mapping too.** Every `testing::` builder that registers a
repository (the `TestContext` builders and `testing::teams::TwoTeams`) also writes the
builder's runner's `runner_repositories` row, by calling `board::service::report_runner` in
process, the way the startup report does. Without it, every existing test that claims
through `TestContext` (the scheduler, runner, strategy, relay and contract suites) fails on
the new refusal.

### 7. Credentials keyed by (repository, runner)

ADR-0033 point 6 changes the key and nothing else of ADR-0020 or D25.

- `credentials::CredentialKey` is an enum. This task defines one variant,
  `Repository { repository_id, runner_id }`, whose keychain account is exactly
  `<repository_id>/<runner_id>`. The service stays `KEYCHAIN_SERVICE`. Both ids are D10 UUID
  strings, so the separator cannot occur inside either. Every account is built in
  `CredentialKey::account()` and never parsed. Later variants for accounts that belong to no
  repository (058, 059) must not produce a `/`, so no two variants can collide.
- `CredentialStore`'s four methods take `&CredentialKey`. `CredentialAccess` is built with
  the runner id bound (`CredentialAccess::new(store, runner_id)`), so every caller still
  passes only a repository id and no caller can name another runner. `MemoryStore` follows
  the trait.
- The module header's "the account is the repository id" paragraph is rewritten to say what
  is true now. Amend D25 with one line pointing at ADR-0033 point 6.
- **The adoption step `credential_keys`**, appended to 040's list after `machine_state`.
  For each checkout whose `credential_added_at` is set: read the item under the old account
  (the repository id); if it is present, write it under the new key and read it back, and a
  mismatch is an error. **The old item is left where it is.** ADR-0028 point 5 keeps the
  release before 065 able to roll back, and an older build reads the keychain by repository
  id alone. If the old item were deleted, D25 point 1 would refuse every run in a
  credentialed repository after a rollback. 065 deletes the old accounts. Running the step
  again writes the same value again, so an interrupted step simply runs again. The step
  inserts its `adoptions` row only after every checkout has been handled.
- **A keychain that is `Unavailable`** leaves the step pending. Startup continues, a warning
  is logged, and the step runs again at the next launch. Until then a credentialed
  repository's runs are refused by D25 point 1's rule. Its message gains one clause, exactly:
  `; its keychain item has not moved to this computer's key yet — unlock the keychain and
  restart Rimaia`.
- Items for repositories 041's adoption skipped (a board row with no path) have no checkout
  to move to. They are left where they are, and the PR says so.

### 8. The doctor: the clone's remote, and push access

Two new `Check` variants, after `RepositoryPath` in `Check::ALL`, which becomes ten. Mirror
both in `src/types.ts`'s `DoctorCheck` union. Both are per mapping and carry the repository
id as well as the name, which `ReportedCheck` needs. `CheckStatus` gains `Deserialize`,
because the board now reads it back.

- **`CheckoutRemote`** (`checkout_remote`, label `Clone's remote`). It calls point 4's
  `verify_remote` and writes the result to the checkout, as the startup pass does. It passes
  when the checkout stays verified. It **fails** otherwise, naming both remotes, with the
  remediation to point `origin` back or to map the right clone. A failing checkout is
  unverified, so it leaves the report's checkout set and `Next`, and the board stops listing
  this runner for that repository.
- **`RemotePush`** (`remote_push`, label `Push access`). For a remote-less repository it
  passes with "no remote to push to". Otherwise it runs, in the clone:

  ```
  git push --dry-run --porcelain --no-verify origin HEAD:refs/heads/rimaia/doctor-push-check
  ```

  An argument vector, never `sh -c`. The environment is the one a run in this repository
  gets from `credentials::inject::child_environment`, so the check proves the run's
  credential and not the operator's. `GIT_TERMINAL_PROMPT=0` and `GCM_INTERACTIVE=Never` are
  added whether or not a credential exists, and `GIT_SSH_COMMAND=ssh -o BatchMode=yes` when
  the parent environment sets no `GIT_SSH_COMMAND`. The check must never wait on a prompt
  nobody will answer.

  A dry run negotiates with the remote's receive side, so the forge checks write access, and
  it writes nothing. The ref is under `rimaia/`, the only namespace a runner ever pushes to
  (ADR-0033 point 4), so a branch-protection rule on the default branch does not decide the
  answer.

  The process is bounded by an injected timeout, `PUSH_CHECK_TIMEOUT` (30 seconds in
  production), the second wall-clock bound in the codebase after D26 point 5's. It is scoped
  the same way: injected, never read at the call site. On expiry the process group is
  stopped through `runner::process::signal_group`, as the archive script is.

  **Status.** On a connected runner (`runner_identity.server_url IS NOT NULL`), a failed
  push is a **fail**. ADR-0033 point 4 makes every connected run in that repository fail
  after the work is done, so D22 point 3's line puts it on the blocking side. That costs a
  queue start for one bad repository, which is the cost `repository_path` already names. In
  solo it is a **warn**: the push is what the base instructions ask for, not a condition of
  success. A warn stays dismissable (task 027).

**What a queue start waits for.** D22 point 1 runs the doctor in `QueueHandle::start`. The
local probes stay sequential, for the reason the comment in `doctor::run` gives about local
subprocesses. The push checks wait on remote hosts instead, so they run concurrently on a
`tokio::task::JoinSet`, at most `PUSH_CHECK_CONCURRENCY` (4) at a time, and their results
are put back in repository order so the report stays deterministic. A queue start
therefore waits at most ⌈mappings ÷ 4⌉ × `PUSH_CHECK_TIMEOUT` for them: 30 seconds for up to
four unreachable remotes, 90 seconds for twelve. The comment in `doctor::run` is rewritten
to say which checks it covers.

`doctor::Environment` gains `connected: bool` and the timeout. After every run the runner
sends a report with the summary and each mapping's `PushCheck`.

### 9. Doors

**MCP.** Board router: `register_repository` and `list_repository_runners`. Local router
(041's host-injected `LocalTools`): `add_repository_from_clone`, `map_repository_checkout`
and `unmap_repository_checkout`. Every one is `RunAccess::Refused` in `Tool::run_access`
(D30). `list_runner_doctor_reports` gets no tool: the operator's agent already has
`run_doctor` on the machine it talks to. Record that as an ADR-0021 point 1 gap in its
appendix row, the way D32 point 9 records the others.

**The interface, this task's part.** `src/types.ts` mirrors every new DTO and field
(`Repository.remote`, `TaskDetail.archiveOutcome`, `OnArchiveOutcome`'s `Pending`, the two
`DoctorCheck` ids, both reads' rows). `src/lib/commands.ts` gets wrappers for the five new
commands. `RepositoryAddForm.tsx` calls `add_repository_from_clone` (point 3). 030's archive
report renders `Pending` (point 5). `DoctorSection` renders the two new checks through the
existing `Check::label` projection, with no special case. 028's fixture transport answers
the new commands. Each change is tested with Vitest, mocking `@tauri-apps/api/core` the way
the existing test files do (or 049's transport mock, if it has replaced that).

## Out of scope

- **The screens** (061): Settings → Repositories' remote, mapping, **Map a clone…** and
  **Unmap**, and each runner's consent and push state; the board's line for a repository
  nobody maps; the browser form that registers by remote, replacing 050's gate; each
  runner's doctor result in `RunnersSection`; and `archiveOutcome` on an archived task.
- **Cloning on demand** into the data directory. ADR-0033 point 2 defers it.
- **Changing a registered repository's remote**, including after a forge rename. The forge
  redirects the old URL, and `CheckoutRemote` names the mismatch if `origin` was changed.
  `update_repository` keeps name and default branch (D32).
- **Forge-specific URL aliases.** Azure DevOps' two URL shapes, `ssh.github.com:443`, and
  enterprise hosts with different SSH and HTTPS hostnames normalise to different
  identities. Point 2's rules are the whole definition.
- **The push postcondition and pushing on the agent's behalf** (057). This task checks
  access; it never pushes a real ref.
- **`base_ref` and fetching a dependency's commit** (044, 057).
- **Deleting the pre-054 keychain accounts** (065, after its readiness check).
- **Any migration other than the two in point 1**, and any column beyond D28's DDL for
  them. If another is missing, stop and ask (D28's D4 amendment).

## Acceptance criteria

**Migrations and caches**

- `src-tauri/migrations/20261003120500_repositories_by_remote.sql` and
  `crates/runner/migrations/20261003130200_checkout_mapping.sql` exist under exactly those
  names and contain D28 part 6's DDL as amended on 2026-10-04. No other migration is added
  or edited. `no_migration_opts_out_of_its_transaction` passes for both sets.
- Both `crates/core/.sqlx/` and `crates/runner/.sqlx/` are regenerated with D33 point 3's
  recipe and committed. No `.sqlx/` exists at the workspace root.
  `SQLX_OFFLINE=true cargo check --workspace --all-targets` passes.
- `the_runner_adopts_the_board_once` covers `credential_keys`. A second launch moves nothing
  and writes nothing to the board.

**Normalisation** (`crates/core/src/repo/remote.rs` unit tests, exact strings)

- `scp_and_https_spellings_of_one_repository_normalise_to_one_identity`:
  `git@github.com:Owner/repo.git`, `https://github.com/owner/repo`,
  `https://github.com/owner/repo.git/`, `ssh://git@github.com:22/owner/repo.git` and
  `git+ssh://git@github.com/Owner/Repo` all give `github.com/owner/repo`.
- `ports_and_nested_groups_normalise_as_documented`:
  `https://gitlab.example.com:8443/Group/Sub/repo.git` gives
  `gitlab.example.com/group/sub/repo`.
- `a_remote_url_never_carries_its_credentials_into_the_identity`:
  `https://x-access-token:ghp_secret@github.com/o/r.git` gives `github.com/o/r`, and the
  result's `Debug` and `Display` do not contain `ghp_secret`.
- `a_local_path_or_file_url_has_no_shared_identity`: `/Users/someone/origin.git`,
  `file:///srv/repo.git`, `C:\repo` and `github.com` (no path) give `None`.
- `a_normalised_remote_round_trips_and_nothing_else_does`: for every input above that
  parses, `from_normalized(parse(x).as_str()) == parse(x)`; `from_normalized` refuses
  `GitHub.com/o/r`, `https://github.com/o/r`, `u@github.com/o/r`, `github.com/o//r`,
  `github.com/o/r/` and `github.com`.
- `host_from_remote_url`'s existing tests pass unchanged on the shared parser.

**The board** (`crates/core/tests/`, over `testing::teams::TwoTeams` and `TestClock`)

- `a_team_repository_is_refused_without_a_remote`, with point 3's exact sentence, and
  `a_personal_team_may_register_a_repository_with_no_remote`.
- `two_repositories_in_one_team_cannot_share_a_remote`: the second registration is `Invalid`
  with the exact sentence, whether the two URLs are spelled alike or not. The same test
  inserts through a raw query to show the unique index maps to that sentence, not to
  `Internal`.
- `two_teams_may_register_the_same_remote`, and `only_an_owner_registers_a_repository`.
- `registering_stores_only_the_normalised_remote`: after registering
  `https://u:secret@github.com/o/r.git`, no column of any board table, no change event and
  no DTO contains `secret` or `u:`.
- `a_report_replaces_the_runners_mapped_set`,
  `a_report_that_changes_nothing_publishes_nothing`, and
  `push_columns_survive_a_report_without_a_push_result`.
- `a_report_naming_another_teams_repository_is_answered_as_unknown_and_writes_nothing_for_it`:
  the receipt is identical for team B's id and for an id that does not exist, and the
  report's other rows are written.
- `a_solo_board_learns_each_repositorys_remote_from_its_first_report`,
  `two_solo_clones_of_one_remote_leave_the_second_repository_remote_less`,
  `a_shared_teams_remote_is_never_rewritten_by_a_report`, and
  `a_reported_remote_that_is_not_canonical_is_never_backfilled`.
- `a_doctor_summary_on_the_board_carries_no_path_and_no_prose`: a report built from a real
  `DoctorReport` over a `TempDir` data directory serialises with no substring of that
  directory and no `detail` or `remediation` key. A summary naming a check id this build
  does not know is stored, not refused.
- `repository_runners_are_listed_only_for_paired_runners_of_current_members`: removing a
  member hides their runner's row without deleting it, and so does unpairing the runner.
  `connected` is false for the solo runner and true for any other.
- `a_mapped_solo_repository_without_consent_and_with_a_failing_push_still_lists_its_runner`:
  one row, `unattendedConsent` false, `pushError` set, `connected` false.
- `list_runner_doctor_reports_returns_only_the_callers_runners`.
- `run_now_on_a_runner_that_has_not_mapped_the_repository_is_refused_before_any_lease`:
  exact sentence, no `runner_leases` row, and `run_state` unchanged. The same for
  `ClaimTarget::Plan`.
- `a_reported_branch_deletion_clears_the_branch_only_while_it_still_matches`, including a
  resend that changes nothing and a task in another team that is untouched.
- `archiving_on_a_server_leaves_a_cleanup_for_the_runner_that_holds_the_worktree`: the
  holder's heartbeat lists it, another runner's does not, the archive report says
  `Pending`, and unarchiving clears it. The same for `done` through `move_task` and
  `approve`.
- `a_reported_archive_outcome_is_stored_once_and_clears_the_request`: a resend is a no-op,
  and a report for a task re-archived meanwhile changes nothing.
- `a_solo_archive_records_its_outcome_and_leaves_no_request`.
- `a_freshly_built_test_repository_is_claimable`: through `Run` and through `Next`, with no
  extra arrangement.

**The port**

- `BoardPort::find_repositories` and `report_runner`, `BoardMethod::FindRepositories` and
  `ReportRunner`, and `Heartbeat::cleanup` exist with D31's 2026-10-04 types.
- The contract suite (D31 point 13) runs the report, lookup and cleanup cases above that
  concern the port through both adapters: in process, and over 052's HTTP adapter against
  the real `rimaia-server` router. Among them:
  `find_repositories_never_answers_for_another_team` (by id and by remote, with an id that
  does not exist answered the same) and
  `the_same_remote_in_two_of_the_owners_teams_returns_both`.
  `every_lease_method_refuses_a_stale_generation` excludes both new methods explicitly, with
  a comment saying they act under no lease.
- `a_runner_report_has_no_field_for_a_path_or_a_script`: the serialised key set of a fully
  populated `RunnerReport` equals a literal list in the test.
- Over HTTP the runner id comes only from the `rmr_` token. A body naming another runner is
  impossible by type, and a test sends a hand-written body with a `runnerId` key to show it
  is ignored.
- Over the HTTP harness, `a_browser_run_now_without_the_mapping_is_refused_at_once`:
  `start_task_run { taskId, runnerId }` returns the exact sentence, `RelayedRequests` holds
  nothing, no `runner_leases` row exists, and `run_state` is unchanged. The same for
  `retry_task_now`.

**The runner** (real git in `TempDir` through `testing::TempRepo`; no mocked git)

- `mapping_a_clone_of_the_registered_remote_records_its_normalised_origin`. The test sets
  `origin` to `https://github.com/owner/repo` with `git remote set-url`; nothing contacts
  the network.
- `mapping_a_clone_whose_origin_is_another_repository_is_refused`, and
  `mapping_a_clone_with_no_shared_origin_to_a_repository_with_a_remote_is_refused`, each
  with the exact sentence.
- `one_clone_may_serve_the_same_remote_in_two_teams_and_nothing_else`.
- `a_remote_less_repository_is_mapped_by_one_runner_only`.
- `remapping_keeps_the_machines_choices_about_the_checkout`.
- `unmapping_waits_for_worktrees_and_then_deletes_both_keychain_accounts`.
- `a_checkout_adopted_before_this_version_is_verified_at_its_first_launch`: a checkout
  adopted by 041 with both new columns `NULL` and a forge `origin`, then one launch, ends
  with the checkout verified, the board repository's remote set, and the task claimable
  through `Next`.
- `a_clone_whose_origin_changed_after_mapping_is_refused_before_spawn`: the lease is
  released, the refusal is recorded, and the stand-in agent's spawn log is empty.
- `next_lists_only_verified_consented_known_checkouts`.
- `a_cleanup_request_runs_the_checkouts_policy_once_and_reports_its_outcome`, including a
  heartbeat that repeats the request before the report is acknowledged.
- `a_failed_report_is_resent_on_the_next_heartbeat_tick`, driven by `TestClock` with no
  `sleep`, with its event lists accumulated across two failures.

**Credentials** (`MemoryStore`; no test touches a real keychain)

- `the_keychain_account_is_the_repository_then_the_runner`:
  `CredentialKey::Repository { .. }.account()` equals `"<repository_id>/<runner_id>"`
  exactly.
- `a_credential_saved_before_this_version_is_read_at_spawn_after_it`: an item under the old
  account, adoption run, the spawn environment of `runner_credentials.rs`'s existing
  assertions carries it, and the old account still holds the item.
- `the_credential_keys_step_can_be_interrupted_and_run_again`: a store that fails the second
  checkout's write, then a second launch, ends with both items under their new keys, both
  old accounts unchanged, and the `adoptions` row written once.
- `an_unavailable_keychain_leaves_the_step_pending_and_startup_continues`, and the spawn
  refusal then carries point 7's exact clause.
- `one_runners_credential_is_never_read_for_another_runner`: two `CredentialAccess` values
  over one `MemoryStore`, bound to two runner ids, do not see each other's item.
- Every existing test in `crates/core/tests/runner_credentials.rs` passes, changed only
  where a key is built.

**The doctor** (real git; the remote is a bare repository in the same `TempDir`, reached
through `url.<bare>.pushInsteadOf`, so `git remote get-url origin` still reads the forge URL)

- `a_remote_the_runner_can_push_to_passes`, and `the_push_check_writes_nothing_to_the_remote`:
  `git ls-remote` on the bare repository lists no ref afterwards.
- `a_remote_the_runner_cannot_push_to_fails_when_connected_and_warns_when_solo`.
- `the_push_check_uses_the_runs_credential_environment_and_never_prompts`: the check's
  command builder is pure, and the test asserts its exact argv and its exact environment
  diff, for a repository with a credential and for one without.
- `a_push_error_carries_no_secret_no_userinfo_and_no_path`: the redaction function, with
  exact expected output, over stderr that contains a token, a `scheme://user:pass@` URL, the
  clone path, a home-directory `known_hosts` path, a data-directory askpass path, and
  `hint:` lines before a `fatal:` line; plus the 1,024-byte cap on a multi-byte boundary.
- `a_push_check_that_never_answers_is_stopped_at_its_injected_timeout`, following
  whichever pattern 030's `a script that never exits` test used. It must pass on all three
  CI operating systems. If a hanging remote cannot be built portably, gate it on
  `cfg(unix)` and say so in the PR.
- `push_checks_run_at_most_four_at_a_time_and_report_in_repository_order`.
- `a_clone_whose_origin_no_longer_matches_fails_its_check_and_leaves_the_report`.
- `Check::ALL` has ten entries in the order point 8 gives, and `src/types.ts` matches.

**Doors and wiring**

- `register_repository`'s flip is one commit, and `./scripts/check-command-wiring.sh`
  passes at that commit and at the end.
- D32's appendix has rows for `add_repository_from_clone`, `map_repository_checkout`,
  `unmap_repository_checkout`, `list_repository_runners` and `list_runner_doctor_reports`,
  each with its kind and deciding ADR point. `register_repository`'s row says it is `board`
  from 054.
- `BOARD_CASES` covers both new board reads and the new `register_repository`, with a
  cross-team case for each.
- `every_registered_tool_has_a_run_scope_decision` passes, with the five new tools
  `Refused`.
- `no_board_dto_carries_an_absolute_path` still passes, now also over `RunnerReport`,
  `RepositoryRef`, `list_repository_runners` and `list_runner_doctor_reports`.
- Vitest: `RepositoryAddForm` calls `add_repository_from_clone` with the exact payload, as
  rendered from `RepositoriesSection` and from `WelcomeView`; the archive report renders
  `Pending` with point 5's exact sentence; `DoctorSection` renders both new checks.
- The full CLAUDE.md command list passes. CI and CLAUDE.md's commands need no change,
  because no crate is added; the PR confirms that they are still identical.

## Notes

**Read first.** ADR-0033 in full: this task implements points 1, 2, 6 and 8 and the doctor
half of its Consequences. ADR-0020 (the credential is unchanged, only its key moves),
ADR-0025 points 4–6 (the policy that becomes mapping configuration), ADR-0028 points 2 and 5
(no path on the board; the rollback window), ADR-0029 point 5 (foreign ids look absent),
ADR-0031 point 7 (Run now for one runner), ADR-0032 points 4 and 5, and ADR-0034 point 5
(the browser doctor). Then the seam entries:

- **D28** part 6, both files' DDL and its 2026-10-04 amendment, the `runners` columns, and
  "The runner set"'s `credential_keys` step. Its D4 amendment names both files.
- **D31** points 2, 3, 4, 13 and 14, and its 2026-10-04 amendment: the two methods, the
  heartbeat field and the suite they join.
- **D32** points 2, 8 and 9, the 054 bullet, and the appendix rows for `register_repository`
  and `set_repository_on_archive`.
- **D33** point 3, the recipe for both caches.
- **D20** point 3 and **D26** points 3–5 (the done removal, the archive outcome, the process
  group and the injected timeout this task follows), **D25** points 1, 3 and 5 (the refusal,
  the environment the push check reuses, the redactor), **D22** points 1 and 3 (the queue
  start, fail and warn), **D30** (the surface the new tools are refused on), D8 and D10,
  and D4 and D6 as prohibitions. No new dependency is needed.

Add a row for 054 to the seam contract's "How to use this" table:
D4 · D8 · D10 · D20 · D22 · D25 · D26 · D28 · D30 · D31 · D32 · D33, plus the entry below.
Append that entry under the next free `D` number, as "Task 054's cross-cutting choices", in
the four-part shape. It records the choices this file makes that no ADR or earlier entry
does:

- the normalisation rules, `from_normalized`, and that a host-less URL is remote-less;
- what "verified" and "remote-less" mean, and the startup pass;
- report semantics: a snapshot, unknown ids, the backfill only into a `NULL` in a personal
  team, no prose from the doctor, and no `serving` flag;
- the keychain account string, the step's order, and that the old item stays until 065;
- the push check's command, environment, timeout, concurrency and fail/warn split;
- the one-clone-two-teams rule;
- the pre-spawn re-verification;
- `OnArchiveOutcome::Pending`, as an amendment line to D26 point 3.

**Files to start from.** These exist on `main` today:

- `crates/core/src/repo/mod.rs` (`register`, `ensure_not_already_registered`,
  `remote_info`), `crates/core/src/repo/git.rs` (`remote_url`, `host_from_remote_url`),
  `crates/core/src/repo/naming.rs`.
- `crates/core/src/credentials/mod.rs` (`CredentialStore`, `KeyringStore`,
  `CredentialAccess`), `credentials/inject.rs`, `credentials/redact.rs`,
  `credentials/provision.rs` (`owner_repo_from_remote`), and
  `crates/core/src/testing/credentials.rs`.
- `crates/core/src/doctor/mod.rs` (`Check`, `Check::ALL`, `CheckStatus`, `run`) and
  `crates/core/src/doctor/checks.rs` (`repository_path`, `github_cli`).
- `crates/core/src/runner/process.rs` (`repository_credentials`, `signal_group`).
- `crates/core/src/archive/mod.rs` and `crates/core/tests/archive.rs` (the timeout pattern).
- `crates/core/src/testing/repo.rs` (`TempRepo::with_remote`, which today sets a bare-path
  `origin`; the push tests set a forge URL plus `pushInsteadOf` instead).
- `crates/core/src/mcp/scope.rs` and `crates/core/src/mcp/server.rs`.
- `crates/core/tests/runner_credentials.rs`, `crates/core/tests/repo_service.rs`,
  `crates/core/tests/doctor.rs`, `crates/core/tests/mcp_scope.rs`.
- `src-tauri/src/commands/repositories.rs`, `src-tauri/src/lib.rs`.
- `src/components/RepositoryAddForm.tsx`, `src/views/WelcomeView.tsx`,
  `src/views/settings/RepositoriesSection.tsx` and `DoctorSection.tsx`,
  `src/lib/commands.ts` and `src/types.ts`.

These arrive with the chain: `crates/core/src/board/` (036, 043, 045, 052),
`crates/core/src/machine/` (041, 066), `crates/runner/src/adopt.rs` and the store (040,
041), `crates/runner/src/board/http.rs` (052), the heartbeat loop (053),
`crates/core/src/api/registry.rs` and `crates/core/src/testing/api.rs` (046).

**Migrations.** `src-tauri/migrations/20261003120500_repositories_by_remote.sql` and
`crates/runner/migrations/20261003130200_checkout_mapping.sql`, frozen once this task lands
on the branch.

**What the chain provides.**

- 038/039: `runners`, the personal team, team-scoped services, `TwoTeams`, and
  `ChangeEvent` tagged with its team.
- 040/041/066: `runner.db`, `runner_identity.server_url`, `checkouts` with the per-machine
  columns, `MachineContext`, `list_checkouts` (066), the adoption list, the host-injected
  local MCP router, and the five machine-reaction functions with their synchronous solo
  path.
- 045: `unattended_consent` as the runner's consent, `ClaimTarget::Next.repositories`
  limited to consented checkouts, the pre-spawn re-check this task's remote check sits
  beside, and the first `Owner` check.
- 046: the registry, `board<T>`/`local<T>`, the dispatcher, and `BOARD_CASES`.
- 049: the HTTP test mock. 050: `RunnersSection`, and the browser gate on Add repository.
- 052: `/api/v1/runner/*`, `RunnerCaller`, the HTTP adapter, the contract suite over both
  adapters, `RelayedRequests`, and the four checks before a Run now for one runner.
- 053: the heartbeat loop, its `on_tick` hook, and the presence heartbeat an idle runner
  sends, which is what carries `cleanup` to it.

If any of those is not where this file says, stop and ask rather than build a second copy.

**What the next tasks expect.**

- **055**: a runner whose clone is verified to be the board's repository, and a redactor
  applied to anything git says before it leaves the machine.
- **057**: `RemotePush` as the preflight for its postcondition, `NormalizedRemote` to name
  the remote it verifies with `git ls-remote`, and `list_repository_runners`' unpaired
  filter, so releasing pins at unpairing needs no second cleanup.
- **058**: `machine::checkouts::map` over its `HttpBoard`, and
  `find_repositories(RepositoryLookup::ByRemote(..))` as the lookup `checkout add` uses,
  with more than one row meaning `--repository` is required. Its runner token is keyed by a
  new `CredentialKey::RunnerToken { runner_id }` variant, whose account is
  `runner-token:<runner_id>`.
- **059**: a connected desktop maps its existing clones through the same function and
  method. Its two tokens are keyed by new `CredentialKey` variants with the accounts 059
  names. Credentials saved in solo are keyed by the solo board's repository ids, so 059 must
  say what happens to them, and this task's key makes that a visible question rather than a
  silent reuse.
- **061**: `list_repository_runners`, `list_runner_doctor_reports`,
  `TaskDetail.archiveOutcome`, `Repository.remote` and the mapping commands, for every
  screen listed under Out of scope. These rules and sentences are settled here:
  - a repository is **mapped** when it has at least one `list_repository_runners` row.
    One with none reads `No computer can run tasks in this repository yet.` A mapped solo
    repository never shows that line, whatever its consent or push state;
  - the board's line, above the columns, for each repository with at least one `ready` task
    and no row: `No computer can run tasks in <name> yet — map a clone in Settings →
    Repositories.`;
  - per runner, a push error is a state only when `connected` is true, worded
    `Cannot push: <pushError>`, and it comes before the consent states;
  - a repository's remote, or `No remote — this computer only`, and this computer's clone
    path, or 066's `Not set up on this computer`;
  - a doctor summary shows a check id it has no label for as the id itself, and no detail
    or remediation, because there is none.
- **065**: deletes the pre-054 keychain accounts once its readiness check passes.

**Size.** L, sized for one session with nothing handed on. Roughly: 60 lines of SQL; 300 of
normalisation and its table tests; 900 of board services, the report, the lookup, the two
refusals and their tests; 450 for the port methods, both adapters and the contract cases;
600 for mapping, the startup pass, re-verification and the local commands; 450 for the
cleanup channel on both sides; 350 for the keychain re-key and its step; 600 for the two
doctor checks, concurrency and redaction; 200 of MCP; 150 for the harness change and its
test; 150 of interface and Vitest. That is about 4.2k lines before the caches. If one
session cannot finish it, stop at a green commit and report what is left. Do not move any
of it to another task: every part above is something 055, 057 or 058 reads.
