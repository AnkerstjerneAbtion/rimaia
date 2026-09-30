---
id: "064"
title: Docs, CLAUDE.md and CI final pass
milestone: v0.5
status: ready
depends_on: ["060", "061", "062", "063"]
adrs: ["0027", "0015"]
size: S
---

# Docs, CLAUDE.md and CI final pass

## Goal

Make `CLAUDE.md`, the repository's `README.md` and `.github/workflows/ci.yml` describe the
code as it stands at the end of team mode, read as three whole documents rather than as the
sum of thirty appended edits. Then prove the one rule `CLAUDE.md` states most firmly: its
command list is exactly what CI runs, with the same compiler and the same two offline query
caches.

**This task changes no behaviour.** It adds no crate, no migration, no query, no command and
no dependency. If the pass finds a sentence that is true in the docs and false in the code,
that is a bug in the code or a stale sentence. It is never a reason to edit the code here
beyond the failing test and the smallest fix (see Notes, "When the docs and the code
disagree").

## Why now

Every task from 033 to 063 was told to update `CLAUDE.md` and CI in the same commit as the
crate, migration or query that needed it, and each did so for its own slice:

- 040 added the runner crate's lines and D33's two-cache recipe.
- 046 added the server crate's lines and `scripts/check-crate-boundaries.sh`.
- 038, 041, 042, 047 and 052 each appended one Gotchas bullet.
- 039, 043, 045 and 051 each added a line to the "must have tests" list.
- 050 added how to run the web shell. 028 added the screenshot section.

Nobody has read the result top to bottom. The Status paragraph still says "tasks 001–009,
019 landed". "Read these before writing code" still counts 18 ADRs, and there are 37. The
repository `README.md` still says "Everything is local. There is no server, no account, and
no telemetry", which is now true of solo mode only.

This matters more than stale prose usually does, for two reasons:

- **`CLAUDE.md` is loaded by every run in this repository.** Task 028 made that load-bearing
  on purpose: it is where a run learns to screenshot its own UI. A run that reads "18 ADRs"
  or a command list one crate short works from a wrong picture of the codebase, all night.
- **The commands block is how "passes locally, fails CI" is prevented.** `CLAUDE.md` says
  `--all-targets` is load-bearing and that the pinned toolchain must be used. D33 point 5
  and 046's Scope 14 added steps to both files in step. Twenty-five tasks later, only a
  line-for-line comparison can say they still agree. D33's Binds names this task as the one
  that makes it.

It lands last among the v0.5 tasks because 060 to 063 are the last to touch these files.
062 adds the container image and its build context, and 063 adds the updater and its
signing step. Only 065 comes after, and it is `not-ready` for a release this PR does not
ship.

## Scope

**1. `CLAUDE.md`, section by section.** Rewrite in place. Keep its voice, its order and every
rule that is still true. Nothing here changes a decision. Where a bullet restates an ADR or a
seam entry, it links there instead of paraphrasing.

- **Title paragraph and Status.** Describe the three ways Rimaia runs (ADR-0027 point 4:
  solo, connected, headless) in one short paragraph, and name the roles: a server owns the
  board, a runner owns the work, and solo is both in one process. Status says what is built
  **without listing task numbers**, and points to `tasks/README.md`'s Landed column for
  that. Listing task numbers is exactly how the current paragraph went stale. It also says
  what has not yet been proven. That means the manual smoke checks from the plan's end of
  M1, M2, M4 and M6: a real unattended night, a real `rimaia.db` migrated in place, two
  users on two machines, and a Litestream restore. Each is described as unproven unless the
  PR body's checklist records a person doing it (tasks/README.md, "Landed is not the same as
  proven"). The `src/` and `src-tauri/` sentences are replaced, because after 042 the
  scheduler does not live in `src-tauri/`.
- **Read these before writing code.** Drop the ADR count, and link the index instead. Item 4,
  `spike/`: the fixtures were promoted by 019, and only `spike/FINDINGS.md` remains. Say so,
  and say that `FINDINGS.md` stays as the record ADR-0004 and ADR-0011 cite. Do not tell the
  reader to delete a directory that is already reduced to the one file other documents
  point at. Items 1 to 3 keep their rule text unchanged.
- **Layout.** One row per workspace member and per migration set, matching the root
  `Cargo.toml` `members` exactly:
  - `crates/core/`: add "the board's services, and both roles' logic (ADR-0027 point 6)" to
    the existing "Must not depend on `tauri`".
  - `crates/runner/`, `crates/runner/migrations/` (040), and `crates/server/` (046), with the
    wording D33 point 4 and 046's Scope 14 gave them.
  - `src-tauri/`: "the shell that hosts a runner and, in solo mode, a server" (ADR-0027
    point 6).
  - `scripts/`: the check scripts CI runs, and `screenshot.mjs`.
  - Whatever 062 put at the root, such as the `Dockerfile` and the Litestream configuration,
    at the paths 062 actually used.
- **Commands.** The block becomes **exactly** CI's `run:` steps, in CI's order, and nothing
  else (Scope 3 defines "exactly"). Two consequences:
  - `npm run tauri dev` leaves the block. It moves to a new `## Running it` section (below),
    because CI does not run it, and the block's first sentence says the block is what CI
    runs.
  - The block's first line is `export SQLX_OFFLINE=true`, with the comment "CI sets this
    workflow-wide". D33 point 6 already requires the export for local verification. Today a
    reader learns it only from the paragraph after the block.

  The paragraphs after the block keep their content, with two changes. The `SQLX_OFFLINE`
  paragraph is D33 point 4's text, followed by D33 point 3's recipe verbatim, including
  `rm -f target/sqlx-prepare-*.db*` and the offline `cargo check -p rimaia-runner` between
  the two prepares. The "`cargo test -p rimaia-core` needs no `--features testing`" paragraph
  now names every crate the block tests.
- **`## Running it`, new, after the screenshot section.** One fenced block per mode, each
  copied from the task that built it rather than written fresh:
  - the solo app with a scratch data directory (029);
  - the server with `RIMAIA_DATA_DIR`, `RIMAIA_LISTEN`, `RIMAIA_PUBLIC_URL` and the GitHub
    client variables (046, 047);
  - the web shell with `RIMAIA_WEB_ROOT` (050), moved here from wherever 050 put it;
  - the headless runner's `pair` and `run` (058);
  - the container image (062): build, run, and where the restore runbook is.

  Each block carries the ports it uses, per the Gotchas bullet below. No secret appears in
  any of them. OAuth values are `<placeholders>`.
- **Testing.** The must-test list gains nothing in this task. It gets checked instead:
  - tenant isolation (ADR-0029's Consequences, added by 039);
  - the lease protocol, with claim races, expiry, fencing, pinning, restart grace and sleep
    recovery (ADR-0031's Consequences, added by 043, and by 053 for the network half);
  - consent and eligibility (045);
  - roles, the last-owner rule and invitations (051);
  - anything 053 to 063 added.

  Each line must name a module path that exists. The Testing section also names
  `src/test/http.ts` beside the `@tauri-apps/api/core` mocks, as 049 required. The rule
  lines keep "Fake the clock", "No `sleep`", the fixture rule, and exact strings for prompt
  composition. ADR-0032 point 7 made the prompt facts part of composition, and 045 tests them
  as whole strings.
- **Conventions.** Every bullet is checked against the code as it now is. Three are known to
  need rewording:
  - "No `String` errors across the Tauri boundary" becomes the Tauri **and HTTP** boundary
    (D32 extends D8 to HTTP).
  - "Tauri commands and MCP handlers are thin adapters" gains the `/api/v1` routes and the
    runner protocol (ADR-0034, ADR-0031).
  - The enum list gains the enums team mode added in place of strings: run `kind` (D29) and
    lease `purpose` (D28), plus any others D28 declares with a `CHECK`. Name only the ones
    that exist as Rust enums, and check each by `grep`.

  046's bullet on the command registry stays as 046 wrote it.
- **Gotchas.** Keep every bullet 038 to 063 added, and merge any two that say the same thing.
  Correct the pre-team-mode bullets whose facts changed:
  - **Unattended runs.** `bypassPermissions` is consented per runner, under a team ceiling,
    and given to a plan revision (ADR-0032, amending ADR-0012). The "do not weaken or widen
    this without amending" sentence now names both ADRs.
  - **Dependencies.** A dependency is still satisfied when its run succeeds. On a connected
    runner, success requires a pushed branch (ADR-0033, 057), and a dependent branches from
    the dependency's recorded `head_sha`, not from a local branch name (044).
  - **`CLAUDE_*` stripping.** Rimaia's own `RIMAIA_*` variables are also stripped from every
    run's environment, matched case-insensitively (D30 point 6). That includes
    `RIMAIA_DATA_DIR`. A run in this repository that launches the app therefore has to set
    its own scratch directory. It never inherits the operator's.
  - **The MCP server names.** `rimaia` is the operator's registration, and `rimaia-run` is
    the run-scoped handle. Nothing ever registers `rimaia-run` in a user's configuration
    (D30 point 1).
  - **`claude` is a prerequisite.** This now applies to every runner, headless included, and
    never to the server, which spawns nothing (ADR-0027 point 1).

  Add three bullets the brief names, each only as far as the code and the ADRs already
  decide it:
  - **`RIMAIA_DATA_DIR` relocates both stores.** `rimaia.db` and `runner.db` live side by
    side, and one variable moves both (ADR-0028 point 4). A scratch directory therefore
    isolates a branch's runner migrations as well as its board migrations. Deleting that
    directory resets both. The server requires the variable and has no platform default
    (046). Say what 058 decided for the headless runner, and nothing beyond that.
  - **Ports.** One bullet replaces the scattered mentions:
    - 1420 is the Vite dev server `npm run tauri dev` holds with `strictPort`, so only one
      worktree at a time can run the app (`vite.config.ts`, `src-tauri/tauri.conf.json`).
    - `npm run screenshot` takes a free port of its own and never 1420 (028).
    - The loopback MCP server defaults to 4517 (`mcp::DEFAULT_PORT`) per desktop runner. A
      development instance with a scratch data directory still defaults to 4517, so it finds
      the port taken whenever the installed app is running. The doctor's "MCP port free" row
      then warns, and that is expected: a busy port is surfaced, not fatal (D16 point 7).
    - The server listens only where `RIMAIA_LISTEN` says, with no default (046). The
      examples in `## Running it` use a port that is none of the above.
  - **Run environment in team mode.** `inherit` stays the default, and its ~3.6× cost note
    stays. Pairing a runner recommends `strict_local` for team use, because it removes the
    inherited MCP registrations entirely (ADR-0032 point 6). Under `inherit`, the resolver
    from D30 point 6 denies Rimaia's own endpoints by name, and it cannot see plugin servers
    or claude.ai connectors. State that residual the way ADR-0032 does, not more strongly.

**2. The repository `README.md`.** It is written for a person installing or evaluating the
app, not for an agent, and keeps that register.

- **Opening and Status.** Keep the first paragraph's promise and add the team sentence:
  plans shared through a server, work still done on each person's own machine and
  subscription (ADR-0027's Consequences, "The premise survives"). Status loses "Thirteen of
  twenty-four tasks" and points at `tasks/README.md`. It keeps the "watch the first few times"
  honesty, and extends it to connected and headless modes, which have had fewer real nights
  than solo.
- **Modes.** A new short section with ADR-0027 point 4's table, in reader terms: who each
  mode is for, and what leaves the machine in each.
- **Prerequisites.** Split the table by role. A desktop or headless runner needs `claude`,
  `git`, and `gh` when pull requests are asked for. The server needs none of them. Building
  the headless runner needs Rust and no Tauri prerequisites.
- **First run.** Describe the mode chooser (059) and then the solo steps as the welcome
  screen now presents them. Check the wording against the `welcome` capture from
  `npm run screenshot`, not from memory. The MCP step shows both registrations: loopback for
  solo, and hosted with a personal token for connected (060). It keeps the instruction to
  copy the line from Settings.
- **The doctor.** Add the rows 054 added (the push check), and any others 053 to 063 added,
  each with the "what it prevents" wording the table uses.
- **Where your data lives.** Replace "Everything is local. There is no server, no account"
  with one table per mode:
  - **Solo:** both files, worktrees and transcripts local; nothing sent anywhere.
  - **Connected and headless:** plans, runs, review bundles and uploaded transcripts on the
    server, subject to the team's retention and summaries-only settings (ADR-0036).
    `runner.db`, checkouts, worktrees and credentials stay on the machine (ADR-0033).

  Keep the `<app-data>/logs/rimaia.log` paragraph. Add ADR-0037 point 6's precondition in one
  sentence: the hosted instance is not for teams outside Abtion until a data processing
  agreement is in place.
- **Cost.** Add the team-mode recommendation from Scope 1's run-environment bullet.
- **Building a bundle.** State what 063 requires at build time. If 063 left
  `bundle.createUpdaterArtifacts` on, `npm run tauri build` needs the updater signing key in
  its environment. Name the variables 063 documented, and state what happens without them:
  the build fails, or the updater artefacts are skipped. Name which one, from 063.
- **Hosting.** A new short section, for self-hosting (ADR-0037 point 1): one instance, one
  volume, the environment variables, and a link to 062's restore runbook. Nothing about the
  platform Abtion uses, which ADR-0037 leaves open.
- **Design.** Drop the "22 ADRs" and "24 tasks" counts. Keep the four links.
- **Layout and Tests.** Mirror `CLAUDE.md`'s layout rows and its Rust test lines, and keep
  the pointer that the full CI list is in `CLAUDE.md`.

**3. `ci.yml`'s final shape, and what "exactly" means.** `CLAUDE.md` and `ci.yml` agree when
each of the following holds. The reviewer checks it the same way:

- **Every `run:` step in `ci.yml` is a line in the commands block, and the reverse.** The only
  exceptions are setup steps that exist because a CI runner is a blank machine: `apt-get`,
  `npm ci`, and the `uses:` actions. A step's `if: matrix.os == 'ubuntu-latest'` is not a
  difference in the command. It is recorded in that step's comment, as `Format` and `Clippy`
  already are.
- **Order.** The block lists commands in the order a person should run them locally:
  - frontend: wiring check, typecheck, test, build;
  - then `cargo fmt`;
  - then each crate's clippy;
  - then the crate-boundary script;
  - then each crate's tests;
  - then `cargo check --workspace --all-targets`.

  The jobs may run in parallel. The order inside each job matches the block's order for the
  lines that job carries.
- **Job names are unchanged**: `Frontend`, `Core (logic) — ${{ matrix.os }}`,
  `Tauri shell (check only)`. Required status checks match on them (D33 point 5). A job 062
  or 063 added keeps the name it landed with.
- **Comments are true.** Each comment in `ci.yml` that describes something that changed is
  rewritten. Known candidates:
  - the wiring step's comment, which 046 was bound to rewrite;
  - the `core` job's comment, which speaks of `cargo test -p rimaia-core` alone;
  - the Clippy step's "Linux only" comment, which now covers three crates.

  No comment names a task as if it were pending.
- **The toolchain.** No step names a Rust version. `rust-toolchain.toml` stays the only place
  one is named (CLAUDE.md, "run it with the same compiler").
- **Offline caches.** `SQLX_OFFLINE: "true"` stays workflow-level. There is no
  `SQLX_OFFLINE_DIR`, no `sqlx-cli` and no `prepare --check` (D5, D33 point 5).

If 062 added a CI job, for example one that builds the container image, its command appears
in the block like every other. If it cannot be run locally, `CLAUDE.md` says why in one
sentence under the block. It is never silently CI-only.

**4. The seam contract's "How to use this" table.** Every task from 033 to 063 has a row, and
each row lists the D entries whose Binds name that task. This task's own row is
`D30 · D32 · D33 · D34`, with D4 and D6 as prohibitions. The table is filled from the Binds
lines. It never changes an entry.

**5. `tasks/README.md`.** The format block's milestone comment lists `v0.5`. The table's row
for this task gets its Landed cell when the PR number exists (CLAUDE.md, "When you finish a
task").

## Out of scope

- **A script that compares `CLAUDE.md` with `ci.yml`.** Considered and declined here. The
  comparison is a mapping, not a text match: it has to understand `if:` conditions, a
  three-OS matrix and setup-only steps. Parsing that in bash, with no `yq` (D6), is the
  fragile kind of check that fails open. If drift recurs after this pass, a checker is its
  own task with its own seam entry, and it adds a CI step that the block would then have to
  list.
- **Editing any ADR's decision or status line.** ADR-0027's "What this supersedes" table is
  the record of what changed in ADR-0002 and ADR-0014. The docs link to it, and the older
  records stay as they are (docs/adr/README.md, Conventions).
- **Editing any seam entry's Question, Decision, Why or Binds.** Scope 4 fills a table
  derived from them.
- **Deleting `spike/`.** Only `FINDINGS.md` is left, and other documents cite it.
- **Deleting retired columns or naming 065's migration.** 065 is `not-ready`, and D4's
  amendment leaves its file unnamed on purpose.
- **User documentation beyond the README**, such as a docs site, a hosted-service onboarding
  guide or a data processing agreement. ADR-0037 point 6 makes the agreement a precondition
  for external teams, and it is a person's work, not an agent's.
- **Any new dependency, crate, migration, query, command or CI job.**

## Acceptance criteria

The contract. A reviewer checks each one at the task's tip.

- **The commands block is CI, line for line.** Each `run:` step of `ci.yml` is paired with
  the block's line that has the same command text. That includes the crate-boundary script
  and the wiring script, and for each of the three crates `rimaia-core`, `rimaia-runner` and
  `rimaia-server`, one clippy line with `--all-targets -- -D warnings` and one
  `cargo test -p` line with no feature flags. The only unpaired steps are the setup steps
  Scope 3 names. The PR body carries the pairing as a two-column table: step name, block
  line.
- **Every command in the block passes** on a clean checkout, run through rustup with
  `SQLX_OFFLINE=true` exported, on the implementer's machine. The draft PR's CI is green on
  all three operating systems at the task's commit (`gh pr checks`).
- **The regeneration recipe regenerates nothing.** Run from the workspace root on a clean
  `target/`, with `rm -f target/sqlx-prepare-*.db*`, D33 point 3's recipe as `CLAUDE.md` now
  prints it leaves `git status --porcelain crates/core/.sqlx crates/runner/.sqlx` empty, and
  no `.sqlx/` at the workspace root. `no_offline_query_cache_at_the_workspace_root` still
  passes. If the recipe does change a cache, a query was committed without its cache. That
  query's task left a bug, and it is fixed in its own commit, cache included.
- **The crate boundaries still hold.** `./scripts/check-crate-boundaries.sh` passes, and so
  do `rimaia_server_does_not_depend_on_rimaia_runner` and
  `rimaia_core_does_not_depend_on_rimaia_runner`. No test is changed to make them pass.
- **The layout table is the workspace.** Every entry in the root `Cargo.toml` `members` has a
  row in `CLAUDE.md`'s layout table and in the README's, and every row names a path that
  exists.
- **Every cited path exists.** Every backticked token in `CLAUDE.md` and `README.md` that
  contains a `/` and names a repository path resolves in the tree at the tip. URLs, globs
  and `<placeholder>` paths are excluded. The one-liner in Notes produces no output.
- **Every `## Running it` block was run.** The PR body records, for each block, the exact
  command, the port it bound, and one line of output. The server block used placeholder OAuth
  values, and was run until it answered 062's health endpoint. The headless block was run
  until `pair` refused a made-up code with the error 058 defines. The container block was
  run until the image built and started. A block that could not be run records why. It is
  not dropped from the file.
- **Status is honest.** The Status paragraphs in `CLAUDE.md` and `README.md` contain no task
  count and no task-number list. Each of the manual smoke checks named in Scope 1 is either
  marked as proven by the PR checklist or described as not yet proven.
- **The Gotchas carry the brief's three facts** (both stores under one `RIMAIA_DATA_DIR`,
  ports, the run-environment advice), plus the `RIMAIA_*` stripping and the `rimaia` /
  `rimaia-run` names. Each cites its ADR or seam entry. No bullet from 038 to 063 was lost:
  the PR body lists each task's bullet and where it now lives.
- **The must-test list covers every ADR that asked for a line.** ADR-0029 (tenant isolation)
  and ADR-0031 (the lease protocol) each have a line, and so do 045's and 051's additions.
  Each line names a module path that exists.
- **The README no longer says there is no server.** "Where your data lives" has a solo table
  and a connected and headless table, and the first says nothing leaves the machine. "22
  ADRs" and "24 tasks" are gone. The doctor table includes 054's push check. Building a
  bundle names 063's signing variables.
- **`ci.yml` is in its final shape.** Job names are unchanged. No step names a Rust version.
  `SQLX_OFFLINE` is set once, workflow-wide. No comment describes two `generate_handler!`
  lists or names a pending task. The file parses: the PR's CI run at the task's commit
  starts every job, which GitHub refuses to do for an invalid workflow.
- **Dependencies match their approvals.** Every dependency in `package.json` and in each
  crate's `Cargo.toml` that was added on this branch is on D6's list or on D34's, and is used
  only from the crates D34 names. `cargo tree -d` shows one `axum`. This task adds nothing,
  and if it finds a dependency that is on no list, it stops and asks.
- **The seam table has a row for every task from 033 to 064**, filled from the Binds lines as
  Scope 4 says. No entry's four parts changed (`git diff docs/seam-contract.md` touches only
  the table).
- **No behaviour changed**, unless a docs-versus-code disagreement was found. In that case the
  commit adds the failing test first, named for the behaviour, and then the fix, as CLAUDE.md
  requires for a bug. Otherwise `git diff --stat` for the task touches only `CLAUDE.md`,
  `README.md`, `.github/workflows/ci.yml`, `docs/seam-contract.md`, `tasks/README.md` and
  this file.
- **No new tests are required, and the reason is stated.** This task changes documents, and
  the existing tests are the check that the documents' commands work. The time-dependent
  behaviour that the Gotchas describe, such as lease expiry and restart grace, is already
  covered with the fake clock by 043 and 053. This pass adds no test that sleeps, and none
  that mocks git.

## Notes

**Seam entries to read.** D33 in full: point 3 is the recipe the block prints, point 4 is
the `CLAUDE.md` text, point 5 is CI, and its Binds line names this task. D32: point 4 (one
handler list), point 5 (the wiring script) and its Binds, which name this task as "the final
docs pass". D30 point 1 (the two server names) and point 6 (`RIMAIA_*` stripping and the
resolver's residual). D34 (the dependency lists). D28's "Retired columns stay", so the docs
do not promise a drop that 065 has not made. D4's team-mode amendment, for the migration
list the Layout section may mention. D16 point 7 (a busy MCP port). Read D4 and D6 as
prohibitions.

**Files to start from.** These exist at the start of the chain:

- `CLAUDE.md`, `README.md` and `.github/workflows/ci.yml`: the three documents.
- `scripts/check-command-wiring.sh`, as 046 rewrote it.
- `docs/seam-contract.md`, `docs/adr/README.md` and `tasks/README.md`.
- The root `Cargo.toml` (`members`, `[workspace.dependencies]`), `package.json` and
  `rust-toolchain.toml`.
- `vite.config.ts` (`port: 1420`), `src-tauri/tauri.conf.json` (`devUrl`, and 063's
  `plugins.updater` and `bundle.createUpdaterArtifacts`).
- `crates/core/src/mcp/mod.rs` (`DEFAULT_PORT`, `MCP_SERVER_NAME`, and after 035
  `RUN_MCP_SERVER_NAME`).
- `crates/core/src/paths.rs` (`AppPaths::resolve`; after 040 `runner_db_file`).
- `src-tauri/src/logging.rs` (`RIMAIA_LOG`).
- `spike/FINDINGS.md`.

These are created by earlier tasks on this branch, so confirm the paths they landed at:

- `scripts/check-crate-boundaries.sh` (046);
- `crates/runner/`, `crates/runner/migrations/` and `crates/runner/.sqlx/` (040);
- `crates/server/` and `crates/server/src/main.rs` (046, and 047 for its variables);
- `crates/core/src/api/registry.rs` (046);
- `src/test/http.ts` (049);
- `playwright.config.ts`, `scripts/screenshot.mjs` and `screenshots/views.shot.ts` (028);
- the `Dockerfile`, the Litestream configuration and the restore runbook (062);
- the headless binary's subcommands (058).

**The path check.** Run this from the workspace root. It prints any backticked path in
either document that does not exist. It must print nothing, apart from the lines it is
already told to skip:

```bash
grep -ohE '`[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]*`' CLAUDE.md README.md \
  | tr -d '`' | grep -vE '^(https?:|~|/tmp/|<)|[*<>]|(^|/)target/' | sort -u \
  | while read -r p; do [ -e "$p" ] || echo "missing: $p"; done
```

A path under the app data directory (`<app-data>/logs/rimaia.log`) is written with its
placeholder and is skipped by the `<` rule. Build output under a `target/` directory, such as
the README's bundle path, does not exist on a clean checkout and is skipped too. If a real path trips the check because it is
relative to a crate rather than to the root, write it from the root in the document. Do not
widen the filter.

**Migrations.** None. This task writes no migration, and it regenerates the caches only to
prove they are already current. The complete lists are D4's team-mode amendment: nine board
files in `src-tauri/migrations/` and four runner files in `crates/runner/migrations/`. The
Layout section may say where each set lives, and it does not list them.

**When the docs and the code disagree.** Treat it as a bug found by reading. Examples: a
Gotchas bullet says `RIMAIA_DATA_DIR` moves `runner.db` and it does not, or a Running-it
block fails for a reason other than a missing secret. Write the failing test first, named for
the behaviour, for example `a_data_dir_override_relocates_the_runner_store`. Then make the
smallest fix, in its own commit. If the fix is more than a few lines, or touches a decision,
stop and report it with `status: blocked`. The doc is then corrected to say what the code
does, with the discrepancy named in the PR. A docs pass must not turn into the task that
should have caught it.

**What the previous tasks provide.** 040 and 046 each left `CLAUDE.md` and `ci.yml` agreeing
for their crate. 041, 045, 047, 049, 051 and 052 each claim that the block still matched CI
at their tip. 060 finishes the command registry flips, so D32's appendix and the registry
agree, and the wiring script passes. 061 finishes the UI, so the `welcome` and Settings
captures are the ones the README describes. 062 adds the image, its build context
(`src-tauri/migrations` and `crates/core/.sqlx/` included, per D33's Binds) and the runbook.
063 adds the updater, its plugin and its signing configuration.

**What the next task expects.** 065 drops the retired columns a release later. It is
`not-ready`, and the workflow skips it. It expects `CLAUDE.md` to already say that retired
columns exist until then, and not to be read, so that nothing written between this PR and
065 starts reading them again. One sentence in the migration or Gotchas text is enough,
pointing at D28's "Retired columns stay". After this task the PR leaves draft, so the
documents written here are what a reviewer of the whole branch reads first.

**Size.** S. Expect about 150–250 changed lines in `CLAUDE.md`, 150–250 in `README.md`,
fewer than 40 in `ci.yml`, and about 35 table rows in the seam contract. That is well under
one session. The time goes into running every command and every Running-it block, not into
writing. If the diff passes about 800 lines, a bug fix from "When the docs and the code
disagree" has grown into a feature. Stop and split it into its own task, numbered with the
next free id and placed directly before this one in `tasks/README.md`.
