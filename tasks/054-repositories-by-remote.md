---
id: "054"
title: Repositories by remote, checkouts by runner
milestone: v0.5
status: ready
depends_on: ["052"]
adrs: ["0033", "0020", "0025", "0028"]
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
  clone's `origin` normalises to that repository's remote.
- **A runner reports what it can serve**: its mapped set, its consent for each mapping, the
  result of a push check for each, and its last doctor result. The board keeps the report
  and shows, for each repository, which runners can run tasks in it and which cannot, and
  why.
- **The board refuses a claim aimed at a runner that has not mapped the task's
  repository**, before any lease exists.
- **Forge credentials are keyed by (repository, runner)** in the runner's keychain, and
  every existing item is moved to the new key once, at first launch.
- **The on-archive policy is configuration of one mapping.** It never crosses to the board,
  and no report can carry it.
- **The doctor checks that this runner can push to each mapped remote**, before the queue
  starts rather than after a night of failed runs.

**Solo behaviour does not change**, with two additions a solo user can see: the doctor has
two more rows per repository, and Settings → Repositories says which computer runs each
repository. Adding a repository by choosing a folder works as it does today, remote or not.

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
read another machine's intention. Moving the key while there is still one runner per board
is a copy. Moving it later is a guess about which runner an item belonged to.

## Scope

### 1. The two migrations

- `src-tauri/migrations/20261003120500_repositories_by_remote.sql`: seam-contract D28 part
  6's DDL for this file, with **one addition**. `runner_repositories` gains
  `unattended_consent BOOLEAN NOT NULL DEFAULT 0` between `reported_at` and
  `push_checked_at`. D31 point 14 assigns "its consent per repository" to this task's
  report, and D28's table has no column to keep it in. Without it the board would list a
  runner as able to serve a repository whose queue never picks anything there, which is
  exactly the invisible state ADR-0033's Consequences want visible. `runner_repositories` is
  a leaf table, so the column costs no rebuild (D28 part 5). Amend D28 part 6 in the same
  commit, with this paragraph as the Why.
- `crates/runner/migrations/20261003130200_checkout_mapping.sql`: D28 part 6's DDL,
  unchanged.

Each file gets a header comment in the voice of the existing migrations. Its first line is
its title and never begins with `-- no-transaction` (D28 part 1). Neither deletes a row.
Regenerate **both** offline caches with D33 point 3's recipe, exactly as written, and commit
them.

### 2. One normalisation function

`crates/core/src/repo/remote.rs` defines `NormalizedRemote`, a newtype over `String` that can
only be built by `NormalizedRemote::parse(url: &str) -> Option<NormalizedRemote>`. It is the
only code in the workspace that decides whether two remote URLs name one repository. The
rules, in order:

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
handler never reads the board's `ServiceContext`), and then maps the clone exactly as point
4 does. If the mapping step fails after the board row was written, the board row stays and
the repository shows "Not set up on this computer" (041's state), with the mapping error
returned. Nothing is rolled back across the two stores, because nothing can be.

`RepositoriesSection`'s "Add repository" calls `add_repository_from_clone`. A form to register
by remote URL alone, from the browser, is 050's shell plus this board command. This task
adds the command and its MCP tool, not the browser form.

### 4. The runner side: map a clone

Two new local commands, both served from `rimaia-runner`'s side through 041's
`MachineContext`:

- **`map_repository_checkout { repositoryId, path }`.** It reads the board repository
  through the dispatcher (D32 point 8), runs task 003's four validations on `path`, then:
  - if the repository has a remote, requires `origin` to normalise to it. Otherwise it
    refuses as `Invalid`, exactly: `<path> is a clone of <clone's normalised remote>, not of
    <repository's remote>`. With no `origin`, or one that parses to `None`: `<path> has no
    origin remote another computer could reach, so it cannot be a clone of <remote>`.
  - if the repository is remote-less, allows it only while no other runner reports it
    (read from the board, point 6). Otherwise: `"<name>" has no remote, so only the
    computer that holds its clone can run it`. ADR-0033 point 1: a remote-less repository
    "is then identified by its clone alone".
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
  <n> worktrees on this computer; remove them first`. It deletes the checkout's keychain
  item (point 7) in the same call, so an unmapped repository leaves no secret behind.

Both commands, and every other mapping change (consent, on-archive), send a report (point 6)
after the write.

**The mapping is re-verified before every spawn.** At the point where the runner resolves
the checkout for a claimed task (041's `worktree::prepare` path), it reads `origin` again and
compares its normalised form with `RunContext::repository.remote`. A mismatch releases the
lease and records the refusal, exactly as 045's pre-spawn consent re-check does, and spawns
nothing. A clone re-pointed after it was mapped would otherwise push one team's plan into
another repository. A remote-less repository skips the check.

**`list_checkouts`** (041) gains `normalizedRemote` and `remoteVerifiedAt`.

### 5. On-archive is configuration of one mapping

041 already moved `on_archive` and `on_archive_script` onto `checkouts`. This task makes the
rule structural rather than incidental:

- `set_repository_on_archive` addresses a mapping. On a repository this runner has not
  mapped it is refused with 041's "not set up on this computer" sentence.
- Unmapping deletes the policy with the checkout.
- `RunnerReport` (point 6) has no field that could carry a policy or a script path. A test
  serialises a fully populated report and asserts its exact key set, so a field added later
  is a visible diff.

No team member can author a command for another member's machine (ADR-0032 point 5), and
after this task the type system says so.

### 6. `report_runner`: the one leaseless report

D31 point 14 gives this task one new `BoardPort` method. Amend D31 in the same commit: point
2's trait and `BoardMethod`, point 4's table, and point 14's sentence.

```rust
// Scoped by the runner the adapter was built for, never by a request field.
fn report_runner<'a>(&'a self, report: RunnerReport) -> BoardFuture<'a, RunnerReportReceipt>;

pub struct RunnerReport {
    pub checkouts: Vec<CheckoutReport>,      // the whole verified set; replaces the last one
    pub doctor: Option<DoctorSummary>,       // None leaves the last one standing
    pub branches_deleted: Vec<BranchDeleted>, // events; each is idempotent
}
pub struct CheckoutReport {
    pub repository_id: String,
    pub normalized_remote: Option<String>,
    pub unattended_consent: bool,
    pub push: Option<PushCheck>,             // None leaves the stored push columns as they are
}
pub struct PushCheck { pub checked_at: DateTime<Utc>, pub error: Option<String> }
pub struct DoctorSummary { pub ran_at: DateTime<Utc>, pub checks: Vec<ReportedCheck> }
pub struct ReportedCheck {
    pub check: Check,
    pub status: CheckStatus,
    pub repository_id: Option<String>,
}
pub struct BranchDeleted { pub task_id: String, pub branch: String }
pub struct RunnerReportReceipt { pub unknown_repositories: Vec<String> }
```

**What the board does with it**, in `board::service::report_runner`, one transaction:

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
  reported remote, unless another repository in that team already holds it. Then the
  repository stays remote-less and keeps working as today; nothing is refused. A repository
  in a team that is not personal is never written by a report, and a non-`NULL` remote is
  never rewritten by one. A runner does not get to re-point a team's repository.
- **The doctor summary** replaces `runners.doctor_report` and `doctor_reported_at` when
  present. It carries check ids, statuses and repository ids, and **no prose**: every
  `detail` and `remediation` today can name an absolute path (`repository_path`,
  `data_directory`), and ADR-0028 point 2 keeps paths off the board. The machine that ran
  the doctor still shows the full rows.
- **`branches_deleted`** is 041's leaseless branch clear in `worktree::remove`, which 041
  left with a comment naming this method. The board sets `tasks.branch = NULL` only where it
  still equals the reported `branch` and the task is in a team the owner is a member of. A
  resend is a no-op. `worktree::clear_branch` goes through the port from here on.
- **Change events.** The board publishes `ChangeEvent::repositories` for the repositories
  whose serving rows changed, and `ChangeEvent::tasks` for cleared branches, each tagged
  with its team (038). A report that changes nothing publishes nothing, so a runner retrying
  a report does not refresh every board.

**When a runner reports.** At startup, after adoption and before the queue can start. After
every doctor run. After every mapping, consent or on-archive change. In process the call is
synchronous. Over HTTP (`POST /api/v1/runner/report_runner`, authenticated by 052's
`RunnerCaller`), a failed send marks the report dirty in memory, and 053's heartbeat tick
resends the latest report until it succeeds. It does not go through 056's outbox, which holds
lease-bound reports in order; a snapshot needs only its latest version.

**The push error leaves the machine redacted.** Git's stderr from the push check (point 8)
can carry a URL with userinfo, and in principle the injected token. Before it enters a
`PushCheck` it passes through: the credential's own `Redactor` (D25 point 5); a rule that
replaces the userinfo of every `scheme://userinfo@` occurrence with `***`; the checkout
path replaced with `<clone>`; and a cap of 1,024 bytes on a character boundary.

**Two board reads.**

- `list_repository_runners { repositoryId? }`: a `board` `Read` row. For each repository in
  the caller's team (or the one named), one row per reporting runner: `repositoryId`,
  `runnerId`, `runnerLabel`, `ownerLogin`, `reportedAt`, `unattendedConsent`,
  `pushCheckedAt`, `pushError` and `serving`. `serving` is true exactly when the consent is
  given and `pushError` is `NULL`. Rows whose runner's owner is no longer a member of the
  team are filtered out at read time, so leaving a team (051) needs no cleanup to stop
  showing a machine.
- `list_runner_doctor_reports`: a `board` `Read` row, scoped to the **caller's own**
  runners, not the team's. ADR-0034 point 5's browser doctor shows "each of the user's
  runners". A teammate's doctor result is not team data.

Both get a cross-team case in 046's `crates/server/tests/commands.rs`, and D32's appendix
gains their rows.

**Claims are filtered by the mapped set.** `ClaimTarget::Run` and `ClaimTarget::Plan` are
refused as `Invalid` when the claiming runner has no `runner_repositories` row for the task's
repository, before any lease is written, exactly: `"<repository name>" is not set up on
<runner label>`. This is the refusal 052's "Run now on this runner" needs when a person
picks a runner from the browser. `ClaimTarget::Next` keeps using the claim's own
`repositories` list, which is fresher than any report. The runner now builds that list from
checkouts that are verified (a `remote_verified_at`, or remote-less), consented (045), and
not named unknown by the last receipt. In solo the startup report runs before the queue can
start, so Run now never meets an empty table.

### 7. Credentials keyed by (repository, runner)

ADR-0033 point 6 changes the key and nothing else of ADR-0020 or D25.

- `credentials::CredentialKey { repository_id, runner_id }`. Its keychain account is
  exactly `<repository_id>/<runner_id>`. The service stays `KEYCHAIN_SERVICE`. Both ids are
  D10 UUID strings, so the separator cannot occur inside either. The account is built in
  one function and never parsed.
- `CredentialStore`'s four methods take `&CredentialKey`. `CredentialAccess` is built with
  the runner id bound (`CredentialAccess::new(store, runner_id)`), so every caller still
  passes only a repository id and no caller can name another runner. `MemoryStore` follows
  the trait.
- The module header's "the account is the repository id" paragraph is rewritten to say what
  is true now. Amend D25 with one line pointing at ADR-0033 point 6.
- **The adoption step `credential_keys`**, appended to 040's list after `machine_state`.
  For each checkout whose `credential_added_at` is set:
  1. read the item under the old account (the repository id);
  2. if present, write it under the new key and read it back; a mismatch is an error;
  3. delete the old item.

  An item that is absent under the old key and present under the new one is already moved,
  so an interrupted step can run again. Only after every checkout has been handled does the
  step insert its `adoptions` row. The keychain is not transactional, so the order, not a
  transaction, is what makes the step safe to repeat.
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
id as well as the name, which `ReportedCheck` needs.

- **`CheckoutRemote`** (`checkout_remote`, label `Clone's remote`). It passes when
  `origin` still normalises to the repository's remote, or the repository is remote-less.
  It **fails** otherwise, naming both remotes, with the remediation to point `origin` back
  or to map the right clone. A failing mapping is also left out of the report's checkout
  set, so the board stops listing this runner for that repository.
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
  stopped through `runner::process::signal_group`, as the archive script is. A host that
  never answers would otherwise hold the Re-check button for as long as TCP takes to give
  up.

  **Status.** On a connected runner (`runner_identity.server_url IS NOT NULL`), a failed
  push is a **fail**. ADR-0033 point 4 makes every connected run in that repository fail
  after the work is done, so D22 point 3's line puts it on the blocking side. That costs a
  queue start for one bad repository, which is the cost `repository_path` already names. In
  solo it is a **warn**: the push is what the base instructions ask for, not a condition of
  success. A warn stays dismissable (task 027).

`doctor::Environment` gains `connected: bool` and the timeout. The checks run sequentially,
per the existing comment in `doctor::run`. After every run the runner sends a report with
the summary and each mapping's `PushCheck`.

### 9. Doors

**MCP.** Board router: `register_repository` and `list_repository_runners`. Local router
(041's host-injected `LocalTools`): `add_repository_from_clone`, `map_repository_checkout`
and `unmap_repository_checkout`. Every one is `RunAccess::Refused` in `Tool::run_access`
(D30). `list_runner_doctor_reports` gets no tool: the operator's agent already has
`run_doctor` on the machine it talks to. Record it as an ADR-0021 point 1 gap in its
appendix row, the way D32 point 9 records the others.

**The interface.**

- `src/hooks/useRepositoryRunners.ts` reads `list_repository_runners` and re-reads on
  `repositories:changed`, subscribing through `src/lib/events.ts` (D7).
- **`RepositoriesSection`**. Each row shows the repository's remote (or "No remote — this
  computer only"). It shows this computer's mapping: the clone path, or "Not set up on this
  computer" with a **Map a clone…** action that uses the folder picker 049 put behind a
  local command. It shows **Unmap**, and a line naming the runners that serve it. A
  repository with none reads, exactly: `No computer can run tasks in this repository yet.`
  A runner that reports it without serving shows why: `consent not given` or `cannot push:
  <pushError>`.
- **The board.** For any repository with at least one `ready` task and no serving runner,
  `Board.tsx` shows one line above the columns: `No computer can run tasks in <name> yet —
  map a clone in Settings → Repositories.` This is ADR-0033's "a task that nobody can run
  is visible before the night", and it is shown only where it matters.
- `DoctorSection` renders the two new checks without special cases, through the existing
  `Check::label` projection.

Each component change has a Vitest test for the mapped, unmapped, not-serving and
no-serving-runner states, mocking `@tauri-apps/api/core` the way the existing 31 test files
do (or 049's transport mock, if it has replaced that). 028's fixture transport answers the
new commands, so `npm run screenshot` renders Settings → Repositories and the board line.

## Out of scope

- **Asynchronous archive cleanup on a remote runner.** 041's "What the next tasks expect"
  and D31 point 14 name "on-archive results" as part of this task, but two decisions are
  missing, and neither is implementation. First: how a remote runner learns that a task
  whose worktree it holds was archived. D31 makes the heartbeat the board's only channel to
  a runner, and it carries only held leases and cancels. Second: where the board keeps an
  outcome that arrives after the archive returned. No D28 column holds one. Solo keeps
  041's synchronous path, which already reports `OnArchiveOutcome` in the same call. **Do
  not invent either decision here.** Amend D31 point 14 and 041's line to say so, and
  append a task under the next free number, placed after 053 and before 058, whose first
  step is the seam entry. The PR description names the gap.
- **Cloning on demand** into the data directory. ADR-0033 point 2 defers it.
- **Changing a registered repository's remote**, including after a forge rename. The forge
  redirects the old URL, and `CheckoutRemote` names the mismatch if `origin` was changed.
  `update_repository` keeps name and default branch (D32).
- **Forge-specific URL aliases.** Azure DevOps' two URL shapes, `ssh.github.com:443`, and
  enterprise hosts with different SSH and HTTPS hostnames normalise to different
  identities. Point 2's rules are the whole definition.
- **The browser forms**: register by remote URL (050), the runners page and each runner's
  doctor report (061). The commands they call are in scope; their pages are not.
- **The push postcondition and pushing on the agent's behalf** (057). This task checks
  access; it never pushes a real ref.
- **`base_ref` and fetching a dependency's commit** (044, 057).
- **Any migration other than the two in point 1**, and any column beyond the one addition
  point 1 names. If another is missing from D28, stop and ask (D28's D4 amendment).

## Acceptance criteria

**Migrations and caches**

- `src-tauri/migrations/20261003120500_repositories_by_remote.sql` and
  `crates/runner/migrations/20261003130200_checkout_mapping.sql` exist under exactly those
  names. They contain D28 part 6's DDL, plus `runner_repositories.unattended_consent` in the
  board file, and D28 part 6 is amended to match in the same commit. No other migration is
  added or edited. `no_migration_opts_out_of_its_transaction` passes for both sets.
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
- `a_report_replaces_the_runners_serving_set`,
  `a_report_that_changes_nothing_publishes_nothing`, and
  `push_columns_survive_a_report_without_a_push_result`.
- `a_report_naming_another_teams_repository_is_answered_as_unknown_and_writes_nothing_for_it`:
  the receipt is identical for team B's id and for an id that does not exist, and the
  report's other rows are written.
- `a_solo_board_learns_each_repositorys_remote_from_its_first_report`,
  `two_solo_clones_of_one_remote_leave_the_second_repository_remote_less`, and
  `a_shared_teams_remote_is_never_rewritten_by_a_report`.
- `a_doctor_summary_on_the_board_carries_no_path_and_no_prose`: a report built from a real
  `DoctorReport` over a `TempDir` data directory serialises with no substring of that
  directory and no `detail` or `remediation` key.
- `serving_runners_are_listed_only_for_members_of_the_repositorys_team`: removing a member
  hides their runner's row without deleting it. `serving` is false when consent is off,
  and when `pushError` is set.
- `list_runner_doctor_reports_returns_only_the_callers_runners`.
- `run_now_on_a_runner_that_has_not_mapped_the_repository_is_refused_before_any_lease`:
  exact sentence, no `runner_leases` row, and `run_state` unchanged. The same for
  `ClaimTarget::Plan`.
- `a_reported_branch_deletion_clears_the_branch_only_while_it_still_matches`, including a
  resend that changes nothing and a task in another team that is untouched.

**The port**

- `BoardPort::report_runner` and `BoardMethod::ReportRunner` exist with point 6's types.
  D31 is amended in the same commit.
- The contract suite (D31 point 13) runs the report cases above that concern the port
  through both adapters: in process, and over 052's HTTP adapter against the real
  `rimaia-server` router. `every_lease_method_refuses_a_stale_generation` excludes
  `ReportRunner` explicitly, with a comment saying it acts under no lease, the way it
  excludes `Claim` and `Heartbeat`.
- `a_runner_report_has_no_field_for_a_path_or_a_script`: the serialised key set of a fully
  populated `RunnerReport` equals a literal list in the test.
- Over HTTP the runner id comes only from the `rmr_` token. A body naming another runner is
  impossible by type, and a test sends a hand-written body with a `runnerId` key to show it
  is ignored.

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
- `unmapping_refuses_while_worktrees_remain_and_deletes_the_keychain_item_when_it_succeeds`.
- `a_clone_whose_origin_changed_after_mapping_is_refused_before_spawn`: the lease is
  released, the refusal is recorded, and the stand-in agent's spawn log is empty.
- `next_lists_only_verified_consented_known_checkouts`.
- `a_failed_report_is_resent_on_the_next_heartbeat_tick`, driven by `TestClock` with no
  `sleep`.

**Credentials** (`MemoryStore`; no test touches a real keychain)

- `the_keychain_account_is_the_repository_then_the_runner`: `CredentialKey::account()` equals
  `"<repository_id>/<runner_id>"` exactly.
- `a_credential_saved_before_this_version_is_read_at_spawn_after_it`: an item under the old
  account, adoption run, the spawn environment of `runner_credentials.rs`'s existing
  assertions carries it, and the old account is empty.
- `the_credential_keys_step_can_be_interrupted_and_run_again`: a store that fails the
  delete after the first write, then a second launch, ends with one item under the new key
  and the `adoptions` row written once.
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
- `a_push_error_carries_no_secret_no_userinfo_and_no_clone_path`: the redaction function,
  over stderr strings that contain each, with exact expected output, plus the 1,024-byte
  cap on a multi-byte boundary.
- `a_push_check_that_never_answers_is_stopped_at_its_injected_timeout`, following
  whichever pattern 030's `a script that never exits` test used. It must pass on all three
  CI operating systems. If a hanging remote cannot be built portably, gate it on
  `cfg(unix)` and say so in the PR.
- `a_clone_whose_origin_no_longer_matches_fails_its_check_and_leaves_the_report`.
- `Check::ALL` has ten entries in the order point 8 gives, and `src/types.ts` matches.

**Doors and wiring**

- `register_repository`'s flip is one commit, and `./scripts/check-command-wiring.sh`
  passes at that commit and at the end.
- D32's appendix has rows for `add_repository_from_clone`, `map_repository_checkout`,
  `unmap_repository_checkout`, `list_repository_runners` and `list_runner_doctor_reports`,
  each with its kind and deciding ADR point. `register_repository`'s row says it is `board`
  from 054.
- `every_board_command_has_a_case` and `a_team_cannot_see_another_teams_ids` cover both new
  board reads and the new `register_repository`.
- `every_registered_tool_has_a_run_scope_decision` passes, with the five new tools
  `Refused`.
- `no_board_dto_carries_an_absolute_path` still passes, now also over `RunnerReport`,
  `list_repository_runners` and `list_runner_doctor_reports`.
- Vitest covers `RepositoriesSection`'s mapped, unmapped, not-serving and no-runner states
  with the exact strings in point 9, the board's line, and "Add repository" calling
  `add_repository_from_clone`.
- The full CLAUDE.md command list passes. CI and CLAUDE.md's commands need no change,
  because no crate is added; the PR confirms that they are still identical.

## Notes

**Read first.** ADR-0033 in full: this task implements points 1, 2, 6 and 8 and the doctor
half of its Consequences. ADR-0020 (the credential is unchanged, only its key moves),
ADR-0025 points 4–6 (the policy that becomes mapping configuration), ADR-0028 point 2 (no
path on the board), ADR-0029 point 5 (foreign ids look absent), ADR-0032 points 4 and 5, and
ADR-0034 point 5 (the browser doctor). Then the seam entries:

- **D28** part 6: both files' DDL, the `runners` columns, and "The runner set"'s
  `credential_keys` step. Its D4 amendment names both files.
- **D31** points 2, 3, 4, 13 and 14: the method this task adds and the suite it joins.
- **D32** points 2, 8 and 9, the 054 bullet, and the appendix rows for `register_repository`
  and `set_repository_on_archive`.
- **D33** point 3, the recipe for both caches.
- **D25** points 1, 3 and 5 (the refusal, the environment the push check reuses, the
  redactor), **D22** point 3 (fail and warn), **D26** points 4 and 5 (the process group and
  the injected timeout this task follows), **D30** (the surface the new tools are refused
  on), D8 and D10, and D4 and D6 as prohibitions. No new dependency is needed.

Add a row for 054 to the seam contract's "How to use this" table:
D4 · D8 · D10 · D22 · D25 · D26 · D28 · D30 · D31 · D32 · D33, plus the entry below. Append
that entry under the next free `D` number, as "Task 054's cross-cutting choices", in the
four-part shape. It records the choices this file makes that no ADR does:

- the normalisation rules, and that a host-less URL is remote-less;
- the one extra column;
- report semantics: a snapshot, unknown ids, the backfill only into a `NULL` in a personal
  team, and no prose from the doctor;
- the keychain account string and the step's order;
- the push check's command, environment, timeout and fail/warn split;
- the one-clone-two-teams rule;
- the pre-spawn re-verification.

**Files to start from.** These exist on `main` today:

- `crates/core/src/repo/mod.rs` (`register`, `ensure_not_already_registered`,
  `remote_info`), `crates/core/src/repo/git.rs` (`remote_url`, `host_from_remote_url`),
  `crates/core/src/repo/naming.rs`.
- `crates/core/src/credentials/mod.rs` (`CredentialStore`, `KeyringStore`,
  `CredentialAccess`), `credentials/inject.rs`, `credentials/redact.rs`,
  `credentials/provision.rs` (`owner_repo_from_remote`), and
  `crates/core/src/testing/credentials.rs`.
- `crates/core/src/doctor/mod.rs` (`Check`, `Check::ALL`, `run`) and
  `crates/core/src/doctor/checks.rs` (`repository_path`, `github_cli`).
- `crates/core/src/runner/process.rs` (`repository_credentials`, `signal_group`).
- `crates/core/src/archive/mod.rs` and `crates/core/tests/archive.rs` (the timeout pattern).
- `crates/core/src/testing/repo.rs` (`TempRepo::with_remote`, which today sets a bare-path
  `origin`; the push tests set a forge URL plus `pushInsteadOf` instead).
- `crates/core/src/mcp/scope.rs` and `crates/core/src/mcp/server.rs`.
- `crates/core/tests/runner_credentials.rs`, `crates/core/tests/repo_service.rs`,
  `crates/core/tests/doctor.rs`, `crates/core/tests/mcp_scope.rs`.
- `src-tauri/src/commands/repositories.rs`, `src-tauri/src/lib.rs`.
- `src/views/settings/RepositoriesSection.tsx`, `CredentialSection.tsx` and
  `DoctorSection.tsx`; `src/components/board/Board.tsx`, `src/lib/commands.ts` and
  `src/types.ts`.

These arrive with the chain: `crates/core/src/board/` (036, 043, 045, 052),
`crates/core/src/machine/` (041), `crates/runner/src/adopt.rs` and the store (040, 041),
`crates/runner/src/board/http.rs` (052), `crates/core/src/api/registry.rs` (046) and
`crates/server/tests/commands.rs` (046).

**Migrations.** `src-tauri/migrations/20261003120500_repositories_by_remote.sql` and
`crates/runner/migrations/20261003130200_checkout_mapping.sql`, frozen once this task lands
on the branch.

**What the chain provides.**

- 038/039: `runners`, the personal team, team-scoped services, `TwoTeams`, and
  `ChangeEvent` tagged with its team.
- 040/041: `runner.db`, `runner_identity.server_url`, `checkouts` with the per-machine
  columns, `MachineContext`, `list_checkouts`, the adoption list, the host-injected local
  MCP router, and the synchronous archive path.
- 045: `unattended_consent` as the runner's consent, `ClaimTarget::Next.repositories`
  limited to consented checkouts, the pre-spawn re-check this task's remote check sits
  beside, and the first `Owner` check.
- 046: the registry, `board<T>`/`local<T>`, the dispatcher a local handler uses, and the
  cross-team command tests.
- 049: the folder picker behind a local command, and the HTTP test mock.
- 052: `/api/v1/runner/*`, `RunnerCaller`, the HTTP adapter, the contract suite over both
  adapters, and Run now aimed at one runner.
- 053: the heartbeat tick a dirty report rides.

If any of those is not where this file says, stop and ask rather than build a second copy.

**What the next tasks expect.**

- **055**: a runner whose clone is verified to be the board's repository, and a redactor
  applied to anything git says before it leaves the machine.
- **057**: `RemotePush` as the preflight for its postcondition, and `NormalizedRemote` to
  name the remote it verifies with `git ls-remote`.
- **058**: mapping, reporting and credentials that need no board file. The headless binary
  calls `map_repository_checkout`'s core function from its CLI.
- **059**: a connected desktop maps its existing clones to the server's repository ids.
  Credentials saved in solo are keyed by the solo board's repository ids, so 059 must say
  what happens to them, and this task's key makes that a visible question rather than a
  silent reuse.
- **061**: `list_repository_runners` and `list_runner_doctor_reports`, with every string
  already written.
- **The archive task this file asks for**: `report_runner` as the leaseless channel, to
  extend with results once its seam entry decides where they are kept.

**Size.** L, and past the comfortable edge of one session. Roughly: 60 lines of SQL; 250 of
normalisation and its table tests; 900 of board services, the report and the claim refusal
with their tests; 400 for the port method, both adapters and the contract cases; 500 for
mapping, re-verification and the local commands; 350 for the keychain re-key and its step;
550 for the two doctor checks and redaction; 200 of MCP; 550 of interface and Vitest. That
is about 3.8k lines before the caches. If it runs over, cut in this order:

1. **The interface (point 9's second half)** moves to the first commit of 061. The commands,
   DTOs and exact strings stay here, so 061 draws what this task decided.
2. **The doctor summary in the report and `list_runner_doctor_reports`** move with it. The
   push check and `CheckoutRemote` stay: 057 depends on them.

Never cut the normalisation, the mapping check, the pre-spawn re-verification, the report's
unknown-id rule, or the keychain re-key. Those are what make a second machine safe to add.
