---
id: "046"
title: The server crate and one command registry
milestone: v0.5
status: ready
depends_on: ["028", "037", "045", "072"]
adrs: ["0023", "0027", "0029", "0030", "0034", "0037"]
size: L
---

# The server crate and one command registry

## Goal

Put the board on HTTP without writing a second API. After this task one table in
`rimaia-core`, `crates/core/src/api/registry.rs`, says of every command whether it is a
**board** command or a **local** one (ADR-0034 point 1). Four things read that table: the
solo shell's `invoke` path, a new `rimaia-server` crate's `POST /api/v1/<name>` routes,
`src/lib/commands.ts`, and `scripts/check-command-wiring.sh`. A board command then runs
through one function, `api::dispatch`, whichever door it came through. The in-process
answer and the HTTP answer are the same by construction, and a contract test checks it
anyway.

Every board route takes a `Caller` as its first extractor, from the first route onward. At
046 the only production `Authenticate` is `RefuseAll`, so a built `rimaia-server` answers
every board command with `401 unauthenticated`. The solo shell's caller is
`Caller::solo(…)`, and tests authenticate with `testing::api::FixedCaller`. Real sessions
and tokens are 047's.

**Nothing the user sees changes.** Invoke names and payloads are what they are today, solo
still answers every command in process, and the 31 frontend test files that mock
`@tauri-apps/api/core` pass without edits.

## Why now

Seam-contract D32's "Why" gives the reason for the order: a caller on every route from
birth, or a retrofit. If routes existed before authentication, 047 would have to edit every
one of them, and any route it missed would serve every team's board to anyone. Landing the
registry, the dispatcher and the `Caller` extractor together, before 047, means no route can
be mounted without a caller. It also means the interim server fails closed.

The order behind it matters just as much. 039 made every board service honour its
context's scope and left a two-team fixture in `testing/`. 041 and 066 moved machine state
off the board DTOs, and 045 added the last commands of M2. Serving the board over a network before
all of that would put an unscoped query behind an open port. Serving it after means the
HTTP half is only a transport, which is ADR-0034 point 2's claim.

Everything after this in M3 and M4 builds on the table:

- 047 implements `Authenticate`;
- 048 puts `Caller` on `/api/v1/events` and flips `get_run_tail`;
- 049 gives `board<T>` its HTTP transport;
- 052 mounts `/api/v1/runner/*` beside these routes and flips the run controls;
- 054 and 056 each flip rows the appendix names, and 054 gives point 3a's machine-less
  path its cleanup channel. The planning rows stay local (D32's amendment on them), and 060
  adds two request rows instead.

Without one list, each of those would keep a list of its own.

## Scope

All nine points of D32 land here, except where a point names another task. D32 is the
specification. This section says where each part goes and fills in what D32 leaves to the
task that builds it.

**1. The `api` module in `rimaia-core` (D32 points 1, 2 and 7).** New files:

- `crates/core/src/api/mod.rs`: the `BoxFuture` alias, `BoardHost`, `BoardRequest`,
  `dispatch`, and `PROTOCOL_VERSION`.
- `crates/core/src/api/registry.rs`: `Command`, `Kind`, `Effect`, `BoardHandler`, the
  `board(…)` and `local(…)` constructors, `COMMANDS` and `find`.
- `crates/core/src/api/caller.rs`: `Caller`, `TeamGrant`, `Door`, `Door::source`,
  `Caller::solo`, `Credential`, `Authenticate` and `RefuseAll`.
- `crates/core/src/api/protocol.rs`: the version rule in point 5 below.
- `crates/core/src/api/board/{analytics,repositories,runs,settings,strategy,tasks}.rs`: one
  file per shell module a board handler comes from.
- `crates/core/src/testing/api.rs`: `FixedCaller`, `BoardCase`, `Foreign` and the board
  case table `BOARD_CASES` (point 11), behind the `testing` feature.

`ServiceContext::for_caller(&caller) -> Result<ServiceContext>` goes in
`crates/core/src/context.rs`, beside 038's `with_scope`. It sets `source` from
`caller.door.source()`, `scope` from the caller's grants and `actor` from `user_id`, and
keeps the same senders. A caller with no grants is `Error::invalid`, because
`TeamScope::of` refuses an empty set. That can first happen when 051 lets a person leave
their last team.

`Caller::solo(&SoloIdentity)` reads 038's `SoloIdentity` from `AppState`. It gives
`Door::Shell`, the solo user, and one grant: the solo team with `Role::Owner`.

**2. The registry rows.** One row for every command the shell registers when this task
starts: the 97 in D32's appendix, plus every row tasks 033 to 045 appended to it. Each row
takes the kind and effect in its appendix row's *From* column **as of 046**. That gives 37
board rows from the appendix. The eight "local until then" rows are `local`, each with a
line comment naming the task that flips it. The three planning rows are plain `local`
rows (D32's amendment on them). 060's two request rows are not in the table yet. Board
rows appended by 033 to 045 (034's review actions, 035's findings reads, 021's loop
controls, 045's consent and eligibility commands) are `board` rows here if their appendix
row says 046.

**If a command exists in `lib.rs` with no appendix row, stop.** Point 8 made appending the
row the earlier task's obligation. Classifying it here would be the judgement D32 says 046
must not make.

**3. Board handlers move into core.** Each board row's `#[tauri::command]` body leaves
`src-tauri/src/commands/*.rs` and becomes an `async fn(BoardRequest, Args) -> Result<O>` in
`api/board/`. Each handler is one call into the service the command called before. The
wire inputs move with them (`NewTaskInput`, `TaskPatchInput` with `opt_patch`/`to_patch`,
`TaskFilterInput`, `NewTaskLinkInput`, `TaskLinkPatchInput`, `UpdateRepositoryInput`,
`RunFilterInput`, and any 033–045 added) **with their serde attributes unchanged**. Each
top-level argument struct is `#[serde(rename_all = "camelCase")]` and ignores unknown keys,
so `{ id, column, beforeId, afterId }` deserializes exactly as Tauri's derived parameters
did.

**A board handler reads nothing but its `BoardRequest`.** Two handlers read
`request.host.provider`: `get_run_cost_summary` and `get_strategy_catalogue`. Six read
`request.machine`, and only to pass it on (point 3a). If any other handler reaches for
`paths`, the runner config, `in_flight`, the tails, the runner store or `request.machine`,
its appendix row is wrong. Stop and say which.

**3a. Six board rows still change this machine in solo, and keep doing so.** 041 left
`tasks::archive_task`, `archive_tasks` and `move_task` as one core function each over
`(&ServiceContext, Option<&MachineContext>, …)`: the board write, then, given a machine,
the reaction. Archiving runs the on-archive policy (ADR-0025, D26), and moving to `done`
runs D20.3's auto-removal. 034's `review::approve` runs that same auto-removal after its
commit, and `review::reject` removes the worktree **before** its transaction and refuses a
dirty one. After 066 both need a `Checkout` from `MachineContext` to do that, so they take
the same shape. So does 066's `repo::remove`, which forgets the repository's leftover
worktree records and removes its checkout after the board removal. The rows are:

- `archive_task`, `archive_tasks` and `move_task`;
- `approve_task` and `reject_task` (034);
- `remove_repository` (066).

All six are board rows from 046 in the appendix. Solo must keep every reaction, in the
same call, and keeps it after 054 too. Without it, archiving and dragging to `done`
silently stop cleaning up, reject leaves the worktree on disk with its branch cleared, and
removing a repository leaves its checkout behind.

Without a machine, which means on a server, the six split. Four leave a cleanup owed:
`archive_task`, `archive_tasks`, `move_task` and `approve_task`. 054 (Scope 5) gives those
a channel and a place to report: the board sets `tasks.cleanup_pending`, the holding
runner hears it on its heartbeat (D31's 2026-10-04 amendment), not through the change
event, and reports a `CleanupDone`. The other two have nothing to defer. Reject's machine
half is a guard that runs before its write, so on a server it stays skipped, as 041 left
it. `remove_repository`'s checkout lives on whichever machine mapped it, and 054 adds no
server reaction for it; a runner still holding that checkout gets the id back in
`report_runner`'s `unknown_repositories`. The appendix notes say "the runner's reaction to
the change event", and the amendment below corrects all six.

This task decides the interim as follows, and records it as a dated D32 amendment in the
same commit (points 2 and 3, and the six appendix rows' notes):

- **The machine rides on the request, and only the shell puts one there.**
  `BoardRequest` gains `machine: Option<MachineContext>`. `MachineContext` is
  `rimaia_core::machine`'s (041), so the server holds the type without linking
  `rimaia-runner`.
- **`BoardHost` does not change.** A server never has a machine.
  `api::dispatch(host, caller, name, args)` keeps D32's signature and sets `machine:
  None`. The server, every contract test and 059's connected desktop call it.
- **`api::dispatch_with_machine(host, caller, machine: &MachineContext, name, args)`** is
  the one variant, and only `commands::board::route` calls it. Both go through one
  private function, so lookup, argument parsing, re-scoping and the span are shared.
- **The six handlers each make one call:** 041's, 066's or 034's core function, as those
  tasks left it, with `request.machine.as_ref()`. That is what the MCP board tool passes
  (`self.local.as_ref().map(|l| &l.machine)`), so ADR-0006's one function behind every
  door still holds.
- **With `None`, the server behaves as 041's and 066's `None` paths do**: no reaction,
  and an archive report that says `Nothing`. That holds until 054, which changes the
  machine-less path inside the same core functions, not the handlers: for a task with a
  `runs` row naming a runner, archiving sets `cleanup_pending = 'archived'` and reports the
  new `OnArchiveOutcome::Pending` instead of `Nothing`, and entering `done` through
  `move_task` or `approve_task` sets `'done'`. The six handlers, `BoardRequest.machine` and
  `dispatch_with_machine` are unchanged by 054, because solo still reacts in the same
  call.

Why not run the reaction in `route`, after `dispatch`: reject's machine half comes first
and can refuse. Run after the board write, it could no longer refuse a dirty worktree, and
034's atomicity would break. The archive report (030) also carries the reaction's outcome
in the command's own answer, and rebuilding that in the shell would put a business rule in
a transport. Why not keep the six `local` until 054: no task flips them. 054 adds the
server's cleanup path inside the core functions these handlers already call, so it needs
them as board rows, not as rows to flip. And a `local` `move_task` means the browser (050)
cannot drag a card.

If 041's diff gave `approve` or `reject` a different shape, follow that diff. If it left
either one reaching the machine with no `Option<&MachineContext>` parameter, stop: that is
041's decision to finish, not this task's.

**4. `ProviderProfile` (D32 point 2).** It is new in
`crates/core/src/runner/provider/mod.rs`, with `id`, `display_name`, `inherit_cost_usd`
and `default_catalogue`, built by `ProviderProfile::of(&dyn AgentProvider)`. It has no
`fanout_noun`. `strategy::catalogue::catalogue` and `runner::outcome::observed_run_cost`
take `&ProviderProfile` in place of `&dyn AgentProvider`, and their MCP callers change
with them. `InProcessBoard`'s `provider` field becomes a `ProviderProfile` (D31 point 9), so
nothing on the board side can spawn.

**5. The protocol version (D32 point 7, ADR-0037 point 4).**
`rimaia_core::api::PROTOCOL_VERSION` is `"1.0"`. Its major matches the `/v1` path segment,
and there is no earlier minor to support yet.

The rule is written against a version it is given, not against the constant, because at
`1.0` half of it cannot be exercised: there is no previous minor and no older one.
`api::protocol::Version { major, minor }` parses `"<major>.<minor>"`, and
`api::protocol::check_against(current: Version, header: Option<&str>, effect: Effect) ->
Result<()>` is the whole rule. `check(header, effect)` is `check_against` with
`PROTOCOL_VERSION` parsed, and it is the only function the board routes call. 052's
"parser and supported window" are `Version::parse` and this rule.

A version is *supported* when its major is the current major and its minor is the current
minor or the one before it; at a current minor of 0, only the current version is. It then
answers as follows:

- a supported version is `Ok` for both effects;
- any other version is `Ok` for `Read` and `Error::UpgradeRequired` for `Write`, older and
  newer alike (ADR-0037: "a runner cannot be ahead of the board");
- **a missing or unparseable header counts as unsupported.** D32 says every client sends
  the header, and ADR-0037 fails closed, so a client that sends none cannot write.

The `UpgradeRequired` message names the version received, or says that none was sent, and
the oldest version the server accepts.

The check runs on the server only. Solo is one process on one version and has no skew.

**6. `ErrorCode` grows by two (D32 point 3, ADR-0030's Consequences).** `Error` gains
`Unauthenticated { message }` and `UpgradeRequired { message }`, with constructors
`Error::unauthenticated` and `Error::upgrade_required`. `ErrorCode` gains `unauthenticated`
and `upgrade_required` beside 043's `conflict`. `src/types.ts`'s `ErrorCode` union gains
the same two strings. Nothing else in `src/` changes because of them: any exhaustive
`switch` the typecheck flags gets the existing default presentation.

**7. The solo shell (D32 points 3 and 4).**

- `src-tauri/src/commands/board.rs` is new and holds `route(inner)`, the composite invoke
  handler. For a name `registry::find` reports as `Board`, it takes the payload from
  `invoke.message.payload()` and the `SoloBoard { host, caller, machine }` from
  `AppState.board`, and answers with
  `invoke.resolver.respond_async(dispatch_with_machine(…))` (point 3a). `machine` is the
  same `MachineContext` `setup()` puts on `AppState.machine` (041). An `Err` serializes to
  the same `{ code, message }` that a `#[tauri::command]` returning `Result<_, Error>`
  produces today. Any other name goes to the `generate_handler!` closure.
- `AppState` gains `board: Option<SoloBoard>`. It is `Some` in solo, which is the only mode
  that exists at 046. A board name with `None` is refused with `Error::invalid`, saying the
  command is sent to the server. 059 is what first sets `None`.
- `src-tauri/src/lib.rs` goes from two `cfg`-selected `generate_handler!` lists to **one**
  list of local rows. `commands::app::debug_provoke_error` is its single entry marked
  `#[cfg(debug_assertions)]`, and the list is wrapped in `commands::board::route(…)`. The
  comment above the list is rewritten to describe one list plus the registry.

**8. The server crate (ADR-0027 point 6).** `crates/server/` is `rimaia-server`, a library
plus a binary. The root `Cargo.toml` `members` becomes
`["crates/core", "crates/runner", "crates/server", "src-tauri"]`.

- **Dependencies.** `rimaia-core` by path, plus the workspace's `axum`, `tokio`, `serde`,
  `serde_json` and `tracing`. `tower-http` enters `[workspace.dependencies]` at `0.6` and is
  taken here with the `trace` feature only (D34). Dev-dependencies are `rimaia-core` with
  `features = ["testing"]`, `reqwest` (the workspace line, which already speaks plain HTTP
  to `127.0.0.1`), `tempfile` and `pretty_assertions`.
- **Not in the manifest:** `rimaia-runner`, any `tauri` crate, `sqlx`, and every crate D34
  gives to a later task (`axum-extra`, `sha2`, `rand`, `subtle`, `clap`). The server holds
  no query macro and has no `.sqlx/` (D33 point 2). It reaches the store only through
  `rimaia-core`.
- **`ServerState { host: BoardHost, auth: Arc<dyn Authenticate> }`** and
  `pub fn router(state: ServerState) -> axum::Router`. The router iterates `COMMANDS` and
  mounts `POST /api/v1/<name>` for each `Board` row, nested under `/api/v1`. That nest has a
  JSON fallback: an unknown name, a local name, or any method other than `POST` gets D8's
  error shape (`not_found` 404 for a name that has no route, `invalid` 400 for a wrong
  method), never axum's plain text.
- **The route handler** takes `Caller` as its first extractor, then the `Rimaia-Protocol`
  header, then the body as bytes. It runs `protocol::check` with the row's effect, parses
  the body itself (an empty body is `{}`, malformed JSON is `invalid`, and a missing
  `Content-Type` is fine), calls `dispatch`, and answers `200` with the output as JSON. A
  unit output is `null`, never `204`.
- **The status table.** `status_for(ErrorCode)` is D32 point 3's table, one arm per code.
  `unauthenticated` adds `WWW-Authenticate: Bearer`, and no code maps to 403.
- **`crates/server/src/caller.rs`: `impl FromRequestParts<ServerState> for Caller`.**
  - `Authorization: Bearer <token>` becomes `Credential::Bearer`.
  - A request that carries both that header and a `rimaia_session` cookie is
    `unauthenticated`.
  - A cookie alone is `unauthenticated` until 047 lands the session half. The cookie's
    name is D32's; nothing parses it yet.
  - After `auth.authenticate(…)` answers, the **pair** of credential shape and door must
    be one the board surface accepts. D32's table accepts `Browser` only with the CSRF
    header on every request, and only a session carries that header, so:
    - `Credential::Bearer` may yield only `Door::Desktop`;
    - `Door::Browser` is accepted only from a `Credential::Session`, which cannot reach
      this check until 047 lands the cookie half.

    Any other pair is `unauthenticated`. A runner token, an MCP token or a `Shell` caller
    therefore cannot drive board commands over HTTP, and neither can a bearer that some
    future `Authenticate` resolves to a browser session. The check is one `match` on the
    pair, with the `Session`/`Browser` arm already written, so 047 makes that arm reachable
    without editing the match.
- **The `trace` layer** records method, matched route and status. It never records a body,
  a header value or a query string (ADR-0037 point 6).
- **`dispatch`'s `command` span** records `name`, the door's variant name and `user_id`.
  It never records the arguments or the output.
- **The binary**, `crates/server/src/main.rs`, reads two variables. `RIMAIA_DATA_DIR` is
  required and validated by the rules `AppPaths::resolve` applies to an override (ADR-0023:
  absolute, no unexpanded `~`). The server has no platform default. `RIMAIA_LISTEN` is a
  socket address. The binary then:
  1. opens `<data dir>/rimaia.db` with `db::connect` and `db::migrate`;
  2. builds a `BoardHost` with `ProviderProfile::of(&ClaudeProvider)` and a context whose
     scope is refused for everything, since `dispatch` always re-scopes from the caller;
  3. serves `router` with `RefuseAll`.

  A missing or invalid variable, a failed migration or a failed bind exits non-zero with a
  message that names what failed. Config parsing is a function over a lookup closure, so
  it is tested without touching the process environment. It never calls
  `identity::ensure_solo`: a server board is not solo. The container, the lock file, the
  public URL and logging setup are 062's. The binary installs no tracing subscriber,
  because no crate for one is approved here.

**How `dispatch`'s base context is built** is the implementer's choice, within one rule:
`BoardHost.context` is never handed to a handler as it is (D32 point 2). If
`ServiceContext::new` cannot be given a scope that refuses everything without widening
`TeamScope`, build the base from any valid scope. `dispatch` replaces it before any handler
runs, and a test proves that.

**9. `commands.ts` (D32 point 6).** The private `call<T>` splits into `board<T>(command,
args?)` and `local<T>(command, args?)`. Every wrapper calls the one that matches its
registry row. In 046 both go through 028's `CommandTransport` exactly as `call` does, so
nothing changes in behaviour. A new exported
`PROTOCOL_VERSION = "1.0"` sits beside them and is not sent yet (049 sends it). 028's
fixture-coverage test changes its extraction from `call<` to `board<`/`local<` **in the same
commit** as the script (028's Notes).

**10. `scripts/check-command-wiring.sh` (D32 point 5).** Rewritten to read four files and
compare them, bash 3.2-safe, no `jq`, and failing closed when a file no longer has the shape
it parses. It checks:

- the registry rows (duplicate names fail, and so does a file with no rows);
- the single `generate_handler![` block, which must equal the local rows in both directions,
  with `#[cfg(debug_assertions)]` allowed only on `debug_`-prefixed entries;
- every `#[tauri::command]` definition, which must be in the handler list;
- `commands.ts`: every row has exactly one wrapper, the wrapper uses the row's kind, and no
  `call<` remains;
- the two `PROTOCOL_VERSION` literals, which must be equal.

Its header comment is rewritten to describe what it now guards. It still needs no Rust
toolchain, so it stays first in CI's `frontend` job.

**11. Re-keying 039's registry test.** Right now `crates/core/tests/tenant_isolation.rs`
reads command names from `lib.rs`'s two blocks. After this task it reads them from
`api::registry::COMMANDS`, and board rows run through `api::dispatch` with team A's owner
as the caller.

The board cases become data, and the shape of a case is pinned here, because 047, 050 and
060 append rows to the same table:

```rust
// crates/core/src/testing/api.rs
pub struct BoardCase {
    pub name: &'static str,
    pub effect: Effect,
    /// Arguments that succeed for team A's owner.
    pub own: fn(&TwoTeams) -> Value,
    pub foreign: Foreign,
}

pub enum Foreign {
    /// Arguments naming team B's entities. Run as team A, the answer is `not_found`.
    Ids(fn(&TwoTeams) -> Value),
    /// A filter naming a team B entity (`list_tasks`, `list_runs` by `repositoryId`).
    /// Run as team A, the answer equals the one for a never-issued id.
    Filter(fn(&TwoTeams) -> Value),
    /// The command can name no entity: a team setting, an aggregate.
    EntityLess,
}

pub static BOARD_CASES: &[BoardCase] = &[ /* one per board row */ ];
```

`Foreign` has no default, so a case cannot skip the cross-team check by leaving a field out.
A command that acts on an entity it names, even optionally, uses `Foreign::Ids` with the
entity named: a `repositoryId` inside `create_task`'s `input`, or the repository a strategy
default is set for. A command that only filters by one uses `Foreign::Filter`, because a
filter on an unknown id is an empty answer, not a missing entity. `EntityLess` is for
commands that can name none, such as `list_repositories`,
`get_base_instructions`/`set_base_instructions`, `get_analytics`, `get_run_cost_summary`,
`get_subscription_cost`/`set_subscription_cost` and the strategy catalogue. The reviewer
checks each `Filter` and `EntityLess` marker against the command's argument struct.

Every case gets 039's checks 1 and 3: run as team A, the answer contains none of team B's
ids and not `team-b-sentinel`, and team B's rows are unchanged afterwards. An `Ids` case
must also answer `not_found`, and a `Filter` case must answer exactly as it does with a
freshly generated id in B's id's place (039's check 2).

That table lives in `crates/core/src/testing/api.rs` so that
`crates/server/tests/commands.rs` iterates **the same table**. Lift it; do not copy it. A
second table is a second answer to "does every board command have a case".
`tenant_isolation.rs` keeps its local cases in its own table, since only core runs them.

If 039 used its size cut and left the Tauri-command half of the table to this task, the
board half is written here, and the remaining local half with it (see Size in Notes for
what moves out first if that makes this task too large).

**12. The HTTP contract suite, `crates/server/tests/commands.rs` (D32 point 5).** The
harness is shared, not inline, because 048's events tests reuse it:
`crates/server/tests/common/mod.rs`, pulled in by each test file with `mod common;`. It
holds:

- `TestServer::spawn(state: ServerState) -> TestServer`, which binds a
  `tokio::net::TcpListener` on `127.0.0.1:0`, reads the port it got, and spawns
  `axum::serve` on that listener. The listener is bound before the spawn, so there is no
  startup race to wait out, and there is no `sleep`;
- `TestServer::post(name, token: Option<&str>, protocol: Option<&str>, body) ->
  reqwest::Response`, over one `reqwest::Client`, and `TestServer::base_url()`;
- `two_teams_state(&TwoTeams) -> ServerState`, a `BoardHost` over the fixture's pool and
  039's `TestClock`, authenticated by `FixedCaller::two_teams(&fixture)`.

`FixedCaller` maps bearer strings to callers, and the map is configurable per token.
`FixedCaller::two_teams` maps two strings to team A's and team B's owners with
`Door::Desktop`. `FixedCaller::with(token, caller)` adds any other token with any `Caller`,
any door included, which is how the runner-door and browser-door tests below are written
and how 047's tests stand in for a session before theirs exists.

**13. The crate boundary (ADR-0027 point 6).** There are two checks, because Cargo alone
enforces neither: it refuses a cycle through `[dependencies]` but not a sibling edge.

- A test in `rimaia-server`, `rimaia_server_does_not_depend_on_rimaia_runner`, reads
  `include_str!("../Cargo.toml")`. It fails if any dependency table names `rimaia-runner`,
  `crates/runner`, or a crate whose name starts with `tauri`. Together with 040's
  `rimaia_core_does_not_depend_on_rimaia_runner`, this covers every path.
- A new `scripts/check-crate-boundaries.sh` runs
  `cargo tree -p rimaia-server -e normal,build,dev --prefix none` and fails if any line
  names `rimaia-runner`, the `rimaia` shell package, or a `tauri` package. It also fails
  closed if the output does not contain `rimaia-core`. This is the transitive answer from
  the build graph itself, and it is what the CI step runs.

**14. CI and CLAUDE.md, in the same commit as the crate.**

- **`.github/workflows/ci.yml`'s `core` job.** Add `Clippy (server)` after
  `Clippy (runner)` (Linux only), `Test (server)` after `Test (runner)` (all three
  operating systems), and `Crate boundaries` after `Clippy (server)` (Linux only), running
  `./scripts/check-crate-boundaries.sh`. Existing job names do not change. The comment on
  the `frontend` job's wiring step is rewritten, because it describes two lists.
- **CLAUDE.md's commands block.** Add `cargo test -p rimaia-server`, `cargo clippy -p
  rimaia-server --all-targets -- -D warnings` and `./scripts/check-crate-boundaries.sh`,
  each next to its twin. Rewrite the wiring script's line comment (D32's Binds). CLAUDE.md
  and `ci.yml` must agree line for line.
- **CLAUDE.md's layout table** gains `crates/server/`: `rimaia-server`, the board over HTTP
  and its binary, which never links the runner (ADR-0027, ADR-0034).
- **CLAUDE.md's Gotchas** gains one bullet on running the server, beside the one on running
  the app with `RIMAIA_DATA_DIR`:
  `RIMAIA_DATA_DIR=<absolute path> RIMAIA_LISTEN=127.0.0.1:8787 cargo run -p
  rimaia-server`. `RIMAIA_DATA_DIR` is required, has no platform default, and follows the
  app's rules (absolute, no unexpanded `~`); the server keeps `rimaia.db` there, so never
  point it at a desktop app's data directory. `RIMAIA_LISTEN` is the socket address to bind.
  Until 047 the server answers every board command `401`. This bullet is where 047 lists its
  variables and 050 adds how to run the web shell, so keep it one bullet they can extend.
- **CLAUDE.md's Conventions** gains one bullet: every command is a row in
  `crates/core/src/api/registry.rs`, a board handler receives a `BoardRequest` and nothing
  else, and a new board row is not done until it has a case in `testing/api.rs`.

## Out of scope

- **Sessions, tokens and CSRF.** The `rimaia_session` cookie half of the extractor, the
  `subtle` comparison, and the `Authenticate` that reads `sessions` and hashed tokens are
  all 047's. **No development token exists in the binary.** The only credential that
  authenticates at 046 is `FixedCaller`'s, and it compiles only under `testing`. A
  `RIMAIA_DEV_TOKEN`-style shortcut would be the unauthenticated server D32 point 7 exists
  to prevent.
- **`/api/v1/events`, SSE, the tail relay, and flipping `get_run_tail`.** 048.
- **Sending anything over HTTP from the frontend**, `get_client_capabilities`, and the
  browser's refusal of local commands. 049. In 046 `board<T>` and `local<T>` both still
  call `invoke`.
- **Serving the web bundle at `/`, CORS, and `tower-http`'s `cors` and `fs` features.**
  050.
- **Roles.** Owner-only refusals are 051's, as `invalid` with a sentence naming the role.
- **The runner protocol, `/api/v1/runner/*` and `RunnerCaller`.** 052.
- **Flipping any "local until then" row.** 052, 054 and 056 each flip theirs, one commit
  per flip.
- **Hosted `/mcp` and MCP parity.** 060. The registry records no MCP pairing (D32 point 9).
- **Converting today's local handlers that read `AppState.context`.** D32 point 8's rule is
  that a local handler never reads the board's context. The failure it prevents, a stale
  answer from a board file nobody writes, first becomes possible when 059 sets
  `AppState.board` to `None`, and D32's Binds give 059 "local handlers reach the board over
  HTTP". 046 writes the rule into `AppState.context`'s doc and adds **no new** local read
  of it. If the reviewer reads point 8 as binding the conversion on 046, that is a
  stop-and-ask, not a silent expansion of this diff.
- **How a caller in several teams names one team for an entity-less command.** 039 refuses
  `list_tasks` under a two-team scope, and ADR-0029 notes that the UI needs a team switcher.
  This task scopes to every team the caller has. 050 decides the narrowing, a
  `Rimaia-Team` request header applied by `Caller::narrow_to` (050 Scope 3), and records it
  in its seam entry.
- **Docker, the lock file, the public URL, logging setup and metrics.** 062.
- **Any migration, any query change, and any dependency** beyond `tower-http` with `trace`
  (D4, D6, D34).

## Acceptance criteria

- **The registry is the list.** `crates/core/src/api/registry.rs` has one row per command
  the shell served when this task started, and no others. Each row's kind and effect match
  its appendix row as of 046. `every_command_is_registered_once` (core unit test) fails on a
  duplicate name. `find` returns `None` for an unknown name.
- **`dispatch` is the only way a board handler runs**, and it behaves as D32 point 2 says:
  - `dispatch_refuses_a_local_or_unknown_name_as_not_found`;
  - `dispatch_reads_null_arguments_as_an_empty_object`;
  - `a_malformed_argument_is_invalid_and_names_the_command`;
  - `dispatch_rescopes_from_the_caller_and_never_hands_over_the_host_context`: a caller
    whose only team is B, dispatched through a host whose base scope names A, sees only B.
- **Solo keeps its machine reactions (point 3a).** Core tests over the two-team fixture's
  real repository and `TestContext::machine()`, each through the dispatcher the shell uses:
  - `a_solo_archive_through_dispatch_runs_the_on_archive_policy`: `archive_task` through
    `dispatch_with_machine`, with the repository's checkout set to remove the worktree,
    leaves no worktree directory and answers the same `OnArchiveOutcome` 041's function
    reports;
  - `a_solo_move_to_done_through_dispatch_runs_the_auto_removal`: `move_task` to `done`
    with auto-cleanup on removes the worktree, as D20.3 does today;
  - `a_solo_approve_and_reject_through_dispatch_reach_the_worktree`: approve with
    auto-cleanup removes the worktree, reject removes it and keeps the branch, and reject
    of a dirty worktree is still refused with nothing written;
  - `the_server_dispatch_passes_no_machine`: each of the six through `dispatch` leaves the
    machine untouched. `archive_task`, `archive_tasks`, `move_task` to `done` and
    `approve_task` leave the worktree in place, and the archive report says `Nothing`;
    `reject_task` writes its board half and leaves the worktree, dirty or not;
    `remove_repository` removes the board row and leaves the checkout and its worktree
    records. A comment at the `Nothing` assertion names 054, which changes it to `Pending`
    for a task with a `runs` row naming a runner.
  - `a_solo_remove_repository_through_dispatch_removes_the_checkout`: a repository whose
    deleted task left a worktree record is removed, and neither the record nor the
    checkout remains;
  - Exactly the six handlers in point 3a read `request.machine`; a `grep` in the PR body
    shows it.
- **Invoke payloads are unchanged.** `invoke_payloads_dispatch_unchanged` dispatches the
  exact argument objects that `src/lib/*.test.ts` and `src/**/*.test.tsx` assert
  `invoke` is called with, for at least `create_task` (`{ input }`), `update_task`,
  `move_task` (`{ id, column, beforeId, afterId }`), `list_runs`, `set_strategy_defaults`
  and `give_up_on_task` (`{ taskId }`). Each one succeeds. No serde attribute on a moved
  input struct changed; `git diff -M` shows them as moves.
- **The caller.**
  - `every_door_maps_to_one_of_adr_0019s_three_sources`: `Shell`, `Browser` and `Desktop`
    map to `Ui`, `Mcp` to `Mcp`, and `Runner` to `System`.
  - `for_caller_sets_source_scope_and_actor`, and `a_caller_with_no_team_is_refused`.
  - `the_solo_caller_owns_the_solo_team_through_the_shell`, built from `ensure_solo`'s
    identity.
  - `refuse_all_refuses_every_credential`: both `Credential` shapes are
    `Unauthenticated`.
- **The protocol rule.** `api::protocol`'s unit tests call `check_against` with a synthetic
  current version of `1.3`, and cover `1.3`, the previous minor `1.2`, an older minor
  `1.1`, a newer minor `1.4`, another major `2.3`, a missing header and a malformed one,
  each for `Read` and `Write`. Only `1.3` and `1.2` may write. Every unsupported read is
  answered. One more test, `the_real_protocol_version_writes_and_its_neighbours_do_not`,
  calls `check` with the real `PROTOCOL_VERSION`: `1.0` writes, and `0.9`, `1.1` and `2.0`
  do not. The `upgrade_required` message names `1.0` as the oldest accepted.
- **The error shape.** `Error::unauthenticated` and `Error::upgrade_required` serialize to
  `{"code":"unauthenticated",…}` and `{"code":"upgrade_required",…}`. `src/types.ts`
  carries both strings. `every_error_code_has_one_status_and_none_is_403` (server unit test)
  walks every `ErrorCode` variant.
- **D32 point 5's HTTP suite passes, against a real listener on an ephemeral port:**
  - `every_board_command_has_a_case` fails for a board row with no case in
    `testing/api.rs`, for a case whose name is not a board row, and for a case whose
    `effect` differs from its row's. Every case names a `Foreign` variant, so no row can
    skip the cross-team check by omission;
  - `every_board_command_has_a_route_and_no_local_command_does`: every board name sent with
    no credentials gets `401` with `{"code":"unauthenticated",…}` and
    `WWW-Authenticate: Bearer`. Every local name gets `404 not_found` in the error shape;
  - `a_team_cannot_see_another_teams_ids`: every case is run as team A. Every
    `Foreign::Ids` case, with team B's ids, answers `404 not_found`. Every
    `Foreign::Filter` case answers exactly as it does with a never-issued id. Every
    `EntityLess` case, with its `own` arguments, answers with none of team B's ids and no
    `team-b-sentinel`, and so does every other case. After every case, team B's rows are
    unchanged (039's snapshot);
  - `both_transports_answer_every_case_identically`: each case's `own` arguments run
    through `dispatch` (no machine, as the server) and over the route, each against the
    same fixture state. The two JSON answers are equal once ids minted by the call itself
    are normalized, and nothing else is normalized;
  - `reads_publish_no_change_event`: after every `Read` case, the change channel is empty.
- **These HTTP behaviours each have a test in `crates/server/tests/`:**
  - `an_empty_body_is_an_empty_object`;
  - `malformed_json_is_invalid_in_the_error_shape`;
  - `a_unit_output_is_null_with_200`;
  - `a_wrong_method_is_invalid_in_the_error_shape`;
  - `an_unknown_name_is_a_json_not_found_and_never_html`;
  - `a_cookie_and_a_bearer_together_are_unauthenticated`;
  - `a_cookie_alone_is_unauthenticated_until_sessions_exist`;
  - `a_runner_door_cannot_drive_a_board_command`, where `FixedCaller` returns
    `Door::Runner` for a bearer;
  - `a_bearer_resolving_to_a_browser_door_is_unauthenticated`, where `FixedCaller` returns
    `Door::Browser` for a bearer;
  - `an_unsupported_protocol_can_read_but_not_write`: a `Read` case answers `200`, and a
    `Write` case answers `426 upgrade_required` with a message naming the minimum version.
    The write is made as team A, and team A's rows, and B's, are unchanged afterwards;
  - `a_missing_protocol_header_cannot_write`;
  - `authentication_is_checked_before_the_protocol`: no credentials and no header gives
    `401`, not `426`.
- **The binary.** `config_from_env` has unit tests for a missing `RIMAIA_DATA_DIR`, a
  relative one, a missing `RIMAIA_LISTEN` and an unparseable one. Each error names the
  variable. `the_binary_refuses_to_start_without_its_environment` runs
  `env!("CARGO_BIN_EXE_rimaia-server")` with an empty environment and asserts a non-zero
  exit and a stderr naming `RIMAIA_DATA_DIR`.
- **The crate boundary.** `rimaia_server_does_not_depend_on_rimaia_runner` passes.
  `./scripts/check-crate-boundaries.sh` exits 0. It exits non-zero on a scratch edit that
  adds `rimaia-runner` to `crates/server/Cargo.toml`'s `[dev-dependencies]`; this is
  checked by hand and listed in the PR body. `crates/server/` contains no `sqlx::query`
  macro and no `.sqlx/`, and its manifest names neither `sqlx` nor any crate D34 gives to a
  later task.
- **The shell.** `src-tauri/src/lib.rs` has exactly one `generate_handler![` block, it
  holds only local rows, and its one `#[cfg(debug_assertions)]` entry is
  `debug_provoke_error`. No board row's `#[tauri::command]` remains under
  `src-tauri/src/commands/`. `AppState` has `board: Option<SoloBoard>`, set in `setup()`
  from `Caller::solo` and `AppState.machine`, and `route` calls `dispatch_with_machine`.
- **D32 is amended.** A dated D32 amendment, in the same commit as point 3a, records
  `BoardRequest.machine`, `dispatch_with_machine`, `SoloBoard`'s third field and the six
  rows of point 3a, `remove_repository` included. Each of those six appendix notes says the
  reaction stays synchronous in solo and what the server does without a machine: for
  `archive_task`, `archive_tasks`, `move_task` and `approve_task`, that 054 records the
  cleanup for the holding runner's heartbeat and turns the archive report from `Nothing`
  into `Pending`; for `reject_task` and `remove_repository`, that the machine half is
  skipped. No note still says "the change event".
- **The wiring script** passes, and each of these scratch edits makes it fail. They are
  checked by hand and listed in the PR body:
  - a board row whose wrapper calls `local<`;
  - a local row missing from `generate_handler!`;
  - a board name added to `generate_handler!`;
  - a `#[tauri::command]` in no list;
  - a `#[cfg(debug_assertions)]` on a name without the `debug_` prefix;
  - a leftover `call<`;
  - a mismatched `PROTOCOL_VERSION`;
  - a duplicated registry row.
- **039's test is re-keyed.** `crates/core/tests/tenant_isolation.rs` takes its command
  names from `api::registry::COMMANDS` and parses no Rust source. Its board cases and
  `crates/server/tests/commands.rs` iterate the one table in `testing/api.rs`.
- **Nothing else moved.** No file under `src/` changes except `src/lib/commands.ts`, its
  own test, 028's fixture-coverage extraction and `src/types.ts`. The other frontend test
  files that mock `@tauri-apps/api/core` pass unedited. No migration is added. Neither
  `crates/core/.sqlx/` nor `crates/runner/.sqlx/` changes. If one has to, run D33's recipe
  and say in the PR why a transport task changed a query.
- CLAUDE.md and `ci.yml` carry Scope 14's edits and agree line for line.
- **Every CI check passes**, run with `SQLX_OFFLINE=true` exported: `npm run typecheck`,
  `npm run test`, `npm run build`, `cargo test -p rimaia-core`,
  `cargo test -p rimaia-runner`, `cargo test -p rimaia-server`, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo clippy -p rimaia-runner --all-targets -- -D warnings`,
  `cargo clippy -p rimaia-server --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`,
  `./scripts/check-crate-boundaries.sh`.
- **Needs a person, and the PR body carries it as a checklist:**
  - `RIMAIA_DATA_DIR=/tmp/rimaia-046 npm run tauri dev`: create, edit, drag, link, archive
    and unarchive a task, change the base instructions and the strategy catalogue, and open
    Analytics. Each works as before. Archiving a task with a worktree under a removing
    on-archive policy, and dragging one to `done` with auto-cleanup on, still remove the
    worktree. The debug-only "provoke an error" action still shows its error;
  - `cargo build --release -p rimaia` compiles, which proves the single list has nothing
    to forget in a release build;
  - `RIMAIA_DATA_DIR=/tmp/rimaia-046-server RIMAIA_LISTEN=127.0.0.1:8787 cargo run -p
    rimaia-server`, followed by `curl -si -X POST localhost:8787/api/v1/list_tasks`, prints
    `401` and the JSON error shape.

## Notes

**Read first.** Seam-contract **D32** in full, including its appendix: it is this task's
specification, and all nine points are this task's unless a point names another. Also:

- **D31** points 8 and 9: the in-process adapter's provider becomes a profile;
- **D33** point 2, no query macros in `rimaia-server`, and point 5 for the shape of the CI
  steps copied here;
- **D34**: `tower-http` with `trace` is this task's and nothing else is;
- **D28** part 3 (the solo identity `Caller::solo` is built from) and **D29** (the kind
  rules a moved runs handler must keep);
- **D8**, which this task amends by two codes as D32 says; **D7**, since `commands.ts`
  stays the only module that imports `invoke`; **D10**; **D16.1**, since HTTP bodies are
  `camelCase` and MCP's are `snake_case`;
- **D4** and **D6** as prohibitions;
- for point 3a, 041's "Machine reactions to board actions", 066's `remove_repository`, 034's
  approve and reject with their refusal table, 054's Scope 5 ("cleanup runs on the holding
  runner"), and D31's 2026-10-04 amendment on `Heartbeat::cleanup`.

ADR-0034 in full, ADR-0027 point 6, and ADR-0037 points 4 and 6. Also ADR-0029 point 5 and
ADR-0030 points 2–8, for what `Caller` stands in for.

**Files to start from.**

- `src-tauri/src/lib.rs`: the two handler lists at lines ~394–593 on `main` @728a049, the
  comment above them, and `setup()`, where `AppState` is built.
- `src-tauri/src/state.rs` (`AppState`) and every file under `src-tauri/src/commands/`.
  `tasks.rs`, `settings.rs`, `strategy.rs`, `analytics.rs`, `repositories.rs` and `runs.rs`
  hold the board rows. `settings.rs:54` and `strategy.rs:88` are the two provider reads.
- `crates/core/src/context.rs` (after 038: `scope`, `actor`, `with_scope`),
  `crates/core/src/error.rs`, and `crates/core/src/lib.rs`.
- `crates/core/src/runner/provider/mod.rs`, `crates/core/src/strategy/catalogue.rs`
  (`catalogue`), `crates/core/src/runner/outcome.rs` (`observed_run_cost`) and
  `crates/core/src/board/in_process.rs` (036).
- `crates/core/src/mcp/mod.rs`: the loopback bind and the axum serve pattern the server
  copies. Tests bind `127.0.0.1:0` the same way.
- `crates/core/src/testing/` after 039 (`teams.rs`, `TwoTeams`), and
  `crates/core/tests/tenant_isolation.rs`. `crates/core/tests/mcp_scope.rs` shows a real
  client against a bound loopback server.
- `crates/core/src/paths.rs` (`AppPaths::resolve`'s override rules, `DATA_DIR_ENV`).
- `scripts/check-command-wiring.sh`, `src/lib/commands.ts`, `src/lib/commands.test.ts`,
  `src/types.ts`, 028's fixture-coverage test, the root `Cargo.toml`,
  `crates/runner/Cargo.toml` (040's manifest shape to copy), `.github/workflows/ci.yml`
  and `CLAUDE.md`.

**Migration.** None. This task changes no schema and no query.

**What the chain provides, and what this task assumes about it.**

- 038: `Role`, `SoloIdentity` in `AppState`, `ServiceContext::new` with a required scope and
  actor, and `TeamScope::of`.
- 039: every board service honours the scope, the `TwoTeams` fixture in `testing/`, and the
  `tenant_isolation.rs` table this task re-keys.
- 040: the `crates/runner` manifest and CI steps to copy, and
  `rimaia_core_does_not_depend_on_rimaia_runner`.
- 041 and 066: machine state is off the board DTOs, `update_repository` has lost
  `worktreeRoot`, `MachineContext` is in `rimaia_core::machine` and on `AppState.machine`,
  and the archive, move, remove, approve and reject functions take
  `Option<&MachineContext>` (point 3a).
- 028: `CommandTransport` and the fixture-coverage test whose extraction point 9 rewrites.
- 034, 021, 035 and 037: the review, findings, loop-control and `get_review_history` rows
  they appended to D32's appendix.
- 042: `LocalSlot`.
- 043: `ErrorCode::Conflict`.
- 045: its consent and eligibility commands, each with an appendix row.

If any of these has a different name in its task's diff, follow that diff. If one is
missing, stop: it belongs to that task, and this task should not improvise it.

**What the next tasks expect.**

- 047: `Authenticate`, `Credential`, `Door::Browser { session_id }` and
  `Door::Desktop { token_id }`, the extractor with its cookie half stubbed as
  unauthenticated, the credential-and-door pairing it fills in for `Session`, a
  `FixedCaller` that returns any `Caller` for any token, and `ServerState.auth` to swap
  `RefuseAll` for its own implementation. It lists its variables in the CLAUDE.md bullet
  Scope 14 adds. 047 also adds `axum-extra` and the TLS feature (D34), and appends a
  `BoardCase` to `BOARD_CASES` for every board row it adds.
- 048: `BoardHost` to gain the tail relay, a `Caller`-taking route to copy for
  `/api/v1/events`, and the loopback harness in `crates/server/tests/common/mod.rs`.
- 049: `board<T>`/`local<T>` to give transports, and `PROTOCOL_VERSION` in `commands.ts`
  to send.
- 050: `router` to mount the bundle and CORS on, `upgrade_required` to reload the page
  on, and the CLAUDE.md bullet on running the server to add the web shell beside.
- 052: the router to nest `/api/v1/runner/*` in, beside the board routes but not in the
  registry, and `protocol::Version::parse` with `check_against` for the runner protocol's
  window.
- 054: the six rows of point 3a as board rows whose handlers make one call each, so that
  it changes only the machine-less path inside 041's, 066's and 034's functions: the
  `cleanup_pending` flag, `OnArchiveOutcome::Pending`, and the
  `the_server_dispatch_passes_no_machine` assertion that goes with it. Solo's synchronous
  reaction stays, so `BoardRequest.machine` and `dispatch_with_machine` stay; no task
  removes them.
- 059: `AppState.board` to set to `None`.

**Why the case table lives in `testing/`.** D32 point 5 and ADR-0029 point 5 each want a
registry test that fails when a board command is added without a cross-team case. If that
test existed twice, once in core over `dispatch` and once in the server over HTTP, two
tables could each be complete and still disagree. One table, run through two doors, is the
same argument D32 makes for `dispatch` itself.

**Where a leak would hide.** `both_transports_answer_every_case_identically` is only as
good as its normalization. Normalize the ids the call mints (a `create_task`'s new id,
`add_task_link`'s link id) by order of first appearance, and nothing else. In particular,
do not normalize timestamps: the fixture's `TestClock` makes them equal. If the test only
passes with more normalized, the transports differ, and that is the finding.

**Size.** L, and at the upper edge of one session:

- the registry, about 150 rows: ~250 lines;
- `dispatch`, the caller and the protocol rule, with their unit tests: ~500;
- moving the 37-plus board handlers and their inputs: ~900, mostly moves, which `git diff
  -M` shows as such;
- the shell's `route` and the single list: ~150;
- point 3a's machine pass-through and its tests: ~250;
- the server crate and the binary: ~400;
- the HTTP suite and the case table: ~700, data-heavy;
- the script rewrite: ~250;
- `commands.ts`, CI and CLAUDE.md: ~250.

That is roughly 3,650 lines. If 039 used its size cut, the Tauri-command half of
`tenant_isolation.rs` arrives here too: four checks for each of roughly 150 commands, local
rows included, which is another ~1,000 lines of case data and well past one session. Cut in
this order, and stop cutting as soon as the rest fits:

1. **The local-row half of `tenant_isolation.rs`**, if 039 left it here. It moves to 049's
   first commit, or to a task appended under the next free number and placed before 050.
   Every **board** row keeps its `BoardCase` in 046 whatever else is cut, because
   `every_board_command_has_a_case` and 047's authentication tests stand on the board half.
   Until the local half lands, `tenant_isolation.rs` requires a case for every board row
   only. A comment at that check names the task that owes the local rows, and that task
   removes the exemption.
2. **The TypeScript half**, into the first commit of 049: the `board<T>`/`local<T>` split,
   the `PROTOCOL_VERSION` copy, 028's extraction change, and the script's wrapper and
   protocol assertions. In 046 those change no behaviour, because both functions still call
   `invoke`, and 049 rewrites the same wrappers anyway. This saves only ~500 lines.

Everything else in Rust stays here, because 047 cannot start without the extractor, the
registry and the board cases. If you cut, say so in the PR and amend the receiving task's
file in the same commit, as 039's cut does for this task.
