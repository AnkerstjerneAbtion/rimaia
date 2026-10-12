---
id: "055"
title: The run-scoped proxy, and runs that cannot launder consent
milestone: v0.5
status: ready
depends_on: ["053", "054"]
adrs: ["0035", "0032", "0026"]
size: L
---

# The run-scoped proxy, and runs that cannot launder consent

## Goal

Two halves, which ship together because the second is only worth having once the first puts
a run's Rimaia access on the network.

**The run-scoped proxy
([ADR-0035](../docs/adr/0035-mcp-when-the-board-is-remote.md) point 5).** A run still
receives `http://127.0.0.1:<port>/mcp/run/{token}`, served by its runner, registered as
`rimaia-run` (D30). What answers that URL changes. Today it builds a `RimaiaServer` over the
board's own `ServiceContext`, which only works while the board and the runner are one
process. After this task it is a proxy that holds no board at all:

1. The runner checks the tool against `Tool::run_access` for the token's grant, and refuses
   there if it can, before anything leaves the machine.
2. It forwards a permitted call through `BoardPort` under the run's lease: the two typed
   write-backs through `record_strategy` and `record_review_findings`, everything else
   through a new `run_tool` method (D31 point 4).
3. The board checks again: the calling runner holds the live lease at that generation, and
   the lease's purpose grants the tool on that task. It then runs the call through the same
   handler bodies the operator's `/mcp` uses.

The run token never leaves the runner. The server sees a runner token and a `LeaseRef`, which
is what it already trusts for every other report.

**Runs that cannot launder consent
([ADR-0032](../docs/adr/0032-assignment-and-consent-to-run-on-a-machine.md) point 6).** A run
on Bob's machine executing Alice's plan must not be able to write a new plan *as Bob* that
then passes Bob's runners and everyone who trusts Bob. Three rules, each with tests:

- **Rimaia's credentials are removed from the run.** Every inherited variable with the prefix
  `RIMAIA_`, matched case-insensitively, is stripped from the child (D30 point 6).
- **Rimaia's operator surfaces are denied by name, wherever they are registered.** Before each
  spawn under `run_environment = inherit`, the runner reads the MCP registrations the child
  will load, finds every one whose URL points at this machine's operator port or at the
  connected server, and denies it exactly as `rimaia` is denied (D30 point 6).
- **What a run writes is marked, end to end.** Task 045 wrote the written-during-a-run mark.
  This task makes sure every write that reaches the board from a run, or from its owner's
  credentials while it runs, arrives with the actor the mark reads. It then proves across the
  HTTP adapter that a laundered revision passes nobody on trust.

**In solo nothing a user can see changes.** The proxy forwards through the in-process
adapter, every forwarded call answers byte for byte what the in-process tool answered, the
personal team makes the mark always false, and an operator with no alias registrations gets
exactly today's argv.

## Why now

Task 052 put `BoardPort` on the network, and 053 and 054 made a remote runner able to claim,
keep and check out work. What a remote runner still cannot do is give a planner, a reviewer
or a fixer a handle that works. `mcp::dispatch` answers `/mcp/run/{token}` by building
`RimaiaServer::scoped` over the board's `ServiceContext` (`crates/core/src/mcp/mod.rs`). On a
headless runner (058) there is no board context, and on a connected desktop (059) the local
one is not the board. So until this task, every strategy, review and fix phase is solo-only.
D31 point 7 leaves exactly this gap open: "`run_tool` … 055 adds it", and "the planner's own
`set_task_strategy` over the scoped handle" moves to `record_strategy` "from the run route".

The laundering rules have to land before the first machine holds a Rimaia credential. 058
pairs a headless runner with an `rmr_` token, and 059 signs a desktop in with an `rmd_` token
and puts a personal access token into the operator's Claude Code registration. Before those,
nothing on a runner can write to a shared board as its owner. After them, an unattended
`bypassPermissions` run inherits the environment and the MCP registrations that can.
ADR-0032 point 6 exists for that moment, and it must already be true when the moment arrives.

## Scope

Read D30 and D31 in full before starting. Everything below refines them; nothing contradicts
them. Where this file makes a choice neither entry makes, Scope 10 records it.

### Part A: the run-scoped proxy

**1. The run-tool dispatch, extracted from `mcp::server`.** New file
`crates/core/src/mcp/run_tools.rs`:

```rust
pub async fn call(
    ctx: &ServiceContext,
    scope: &RunScope,
    tool: Tool,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Value, ToolError>;
```

- It has one arm per tool that some grant may reach through `run_tool`: `get_task`,
  `get_base_instructions`, `list_repositories`, `update_task`, `add_task_link`,
  `remove_task_link` and `resolve_review_finding`. Each arm deserializes the tool's request
  type from `mcp::requests` and calls a body function. Every other tool falls through to
  `scope.authorize(tool, None)` and returns its `Err`, so a refusal still comes from the one
  table, never from a missing arm. If `authorize` passes a tool with no arm (an `Unscoped`
  tool, say), the fallthrough returns `Error::internal`. It never returns `Ok`.
- **The body functions are the handlers' bodies.** Each of the seven `RimaiaServer` handlers
  becomes a one-line call into the same function, so the operator's door and a run's door
  cannot diverge. `authorize` stays the first statement of each body (`mcp/scope.rs`'s "one
  decision point"). `remove_task_link` keeps resolving its link before it authorizes.
- The answer is the `serde_json::Value` that the handler's `Json<T>` carried.
- `RimaiaServer::scoped` loses its last production caller in point 4. It moves behind
  `#[cfg(any(test, feature = "testing"))]`, and `tests/mcp_scope.rs` keeps driving the table
  through it unchanged.

**2. `BoardPort::run_tool`.** D31 point 2's signature, exactly:

```rust
fn run_tool<'a>(&'a self, lease: &'a LeaseRef, call: RunToolCall)
    -> BoardFuture<'a, serde_json::Value>;

pub struct RunToolCall { pub tool: Tool, pub arguments: serde_json::Map<String, Value> }
```

- `BoardMethod::RunTool` joins `BoardMethod::ALL`. Its `as_str()` is `run_tool`.
- `Tool` serializes as `Tool::as_str()` and deserializes through `Tool::from_name`. A name
  the server does not know is `Error::invalid`, never a panic or a 500, because a newer
  runner talking to an older server is ordinary version skew (ADR-0037).
- The in-process body calls `board::service::run_tool`.
- The HTTP body, in `crates/runner/src/board/http.rs`, is
  `POST /api/v1/runner/run_tool` with `{ lease, call }` as JSON (D31 point 10).
- The server route goes in `crates/server/src/runner_api.rs`, beside 052's. 052's wiring test
  asserts one route per `BoardMethod` variant, so it covers this one.

**3. The board's check, in `board::service::run_tool`.** One transaction, in this order:

1. **The fence.** 043's `board::lease::current(conn, lease, runner_id)`, with the calling
   runner's id from the adapter (D31 point 3), never from the body. A lease another runner
   holds, a stale generation or a released lease is `Conflict`. A task outside
   `LeaseRef.team_id` is `NotFound`. Nothing is written.
2. **The grant, derived from the lease row.** D30 point 5's mapping, read from
   `runner_leases.purpose` and `runner_leases.run_id`, never from the request (035: "a run
   id is never a request argument"):
   - `strategy` is `Grant::Strategy`, and so is `implementation` while `run_id IS NULL`.
     A fresh claim's purpose is `implementation` (043), and `run_task` resolves the strategy
     before `start_run`, so the planner a run spawns (`process.rs`, `strategy::resolve`)
     holds exactly that lease. Nothing else is handed a handle before `start_run`. This
     refines D30 point 5, which maps `implementation` to no tool, and Scope 10 records it.
   - `review` is `Grant::Review { run_id }`, and `fix` is `Grant::Fix { run_id }`.
   - `implementation` with a `run_id` is allowed no tool. `start_run` has moved the purpose
     to the run's kind (043), so this is the implementation run itself. Every call under it
     is refused with exactly:
     `"<tool> is not available to an implementation run: it was given no Rimaia handle."`
3. **The typed doors.** `set_task_strategy` and `record_review_findings` are refused with
   `Error::invalid`, because each has its own port method and each write keeps one door
   (D31 point 4).
4. **The call.** `run_tools::call` over a context built for this call:
   - `scope = TeamScope::one(<the leased task's team>)`, which step 1 checked against
     `LeaseRef.team_id`, so an `Unscoped` read such as `list_repositories` sees that team
     and no other team the runner's owner is in;
   - `actor = Actor::User(<the runner's owner>)`, which is what 045's mark reads;
   - `source = MutationSource::Mcp`. The write is an agent's tool call (ADR-0019), whichever
     door carried it, and today's scoped route records exactly that. The adapter's own
     `System` source (D31 point 9) stays for reports.

`RunScope::Run { task_id: lease.task_id, grant }` is the scope `call` authorizes against. So
the server's check is the **same** `run_access` table the runner applies (D30 point 5), and
the grant it evaluates is one the runner could not have chosen.

**4. The proxy.** New file `crates/core/src/mcp/run_proxy.rs`. It depends only on
`Arc<dyn BoardPort>`, `RunHandles` and a fence hook, so the desktop and 058's headless
runner serve the same code, and `rimaia-core` gains no dependency.

- **The fence hook.** `pub type FenceHook = Arc<dyn Fn(LeaseRef) + Send + Sync>`. Core
  cannot call `crates/runner`, where 053 puts `fence::on_fenced`, so the host injects it:
  the desktop shell and the headless binary each pass a closure over `fence::on_fenced`.
  The hook returns at once, and a host whose reaction is async spawns it. Tests pass a
  recording closure.
- **`RunHandles` carries the lease.** `grant(task_id, Grant)` (035) becomes
  `grant(lease: LeaseRef, Grant)`, and `resolve(token)` returns the lease with the scope.
  Every caller that mints a grant already holds a claim, so already holds the `LeaseRef`.
- **A hand-written rmcp `ServerHandler`**, one value per request, built over the same
  stateless transport configuration as `service_over`:
  - `get_info` reports `RUN_MCP_SERVER_NAME`.
  - `list_tools` returns the combined router's tool list (041's board and local routers),
    unfiltered, for the reason `mcp/scope.rs`'s header gives. It is the same list, schemas
    included, that the in-process server offered.
  - `call_tool` resolves the name with `Tool::from_name`, and deserializes the arguments into
    the tool's request type, so a malformed call fails on the runner exactly as it did
    before. It then makes **the runner's check**: `tool.run_access(grant_kind)` must not be
    `Refused`, and a tool that names a `task_id` must name the lease's task. Both refusals
    are `RunScope::authorize`'s own errors, with its own wording. `remove_task_link` names a
    link, not a task, so the runner checks only that the grant may call it, and the board's
    check resolves the link.
  - A permitted call is forwarded:
    - `set_task_strategy` becomes `record_strategy(lease, request.into_plan())`;
    - `record_review_findings` becomes `record_review_findings(lease, run_id, findings)`, with
      `run_id` taken from the grant;
    - everything else becomes `run_tool(lease, RunToolCall { tool, arguments })`.
  - **The answer is the in-process answer.** A `run_tool` value is wrapped the way rmcp's
    `Json<T>` wraps it. `set_task_strategy` answers the `TaskView` it answers today, read back
    with a forwarded `get_task`, which every grant may call on its own task. If a typed
    method's reply cannot reproduce its tool's answer, the method returns that answer's view
    instead of `()`, and Scope 10 records it as a D31 amendment. An `Err` from the port is
    rebuilt from its `code` (D31 point 10) and handed to `ToolError`, so the payload is D8's
    `{ code, message }` whichever check refused.
- **Mounting.** `mcp::build` takes the `Arc<dyn BoardPort>` that D31 point 8 puts on
  `AppState` as `board_port`, and a `FenceHook`. It mounts
  `run_proxy::router(handles, board, on_fenced)` at the scoped path in place of today's
  `dispatch`. `RunRoute`'s `ctx` and local-tools fields go with it.
  `run_proxy::bind(handles, board, on_fenced, port: u16) -> (RunProxyHandle, RunProxyTask)`
  is for 058's headless runner, which has no operator endpoint. It binds the literal
  `127.0.0.1` on `port`, where `0` means OS-chosen (058 passes `0`: `mcp_port` is the
  operator listener's setting), and calls `RunHandles::set_endpoint` with the bound address. It has the same
  infallible, status-carrying shape as `mcp::build`, for the same reason.
- The doc comments in `mcp/mod.rs` and `mcp/scope.rs` that describe the scoped route as
  "the same `RimaiaServer` over the same `ServiceContext`" are rewritten to say what is now
  true: two checks, one table, and a route that holds no board.

**5. What the proxy does when the board cannot answer.**

- **Unreachable, or a 5xx.** The run gets a tool-level error it can read, saying the board
  could not be reached and the call was not applied. Nothing is written locally, and nothing
  is queued. `run_tool` is not a report, so 056's outbox never holds it. The HTTP adapter
  does not retry (D31 point 10), and neither does the proxy. The agent may call again.
- **`Conflict`.** The run gets the `Conflict` as a tool-level error, and the proxy calls the
  `FenceHook` with the lease. Through it the lease reaches 053's one reaction to a fenced
  lease (D31 point 11): stop the process, keep the worktree, do not push, drop the lease
  from `held`. The proxy never decides any of that on its own.

### Part B: runs that cannot launder consent

**6. The `RIMAIA_` strip.** In `crates/core/src/runner/process.rs`, `inherited_identity_vars`
takes the union of every provider's identity prefixes **and** Rimaia's own `RIMAIA_`, matched
case-insensitively by the same `is_identity_of_any_provider` rule. That covers runner and
personal access tokens, whatever 058 and 059 name them, and `RIMAIA_DATA_DIR`. Variables
Rimaia sets for a child are applied after stripping and survive it: the on-archive script
(`crates/core/src/archive/mod.rs`) already strips first and sets `RIMAIA_TASK_ID` and its
siblings after. The doctor's probes strip the same set, because they call the same function.

The existing test `a_run_never_hands_the_child_an_inherited_claude_variable_in_either_mode`
(`crates/core/tests/runner_process.rs`) uses `RIMAIA_TEST_MARKER` as the variable that must
**survive**. Under this rule it must not survive. The marker is renamed to a name with no
Rimaia prefix, and a second marker with the prefix is asserted stripped. This is the one
existing assertion this task changes, and it changes because the rule it pinned changed.
No test ever sets `RIMAIA_DATA_DIR` in the test process: sibling tests resolve paths from it.

**7. Deny by URL, resolved to names before the spawn (D30 point 6).**

- **`AgentProvider::inherited_mcp_servers(&self, home: &Path, workspace: &Path) ->
  InheritedMcp`**, with a default that returns nothing, so Ledger (`testing/provider.rs`) and
  any future provider inherit no denial by accident of omission. `InheritedMcp { servers:
  Vec<McpRegistration { name, urls, source }>, unreadable: Vec<(PathBuf, String)> }`.
- **Claude's implementation** reads exactly D30 point 6's files:
  - `<home>/.claude.json`: the top-level `mcpServers`, and `projects.*.mcpServers` under
    **every** project key;
  - `<workspace>/.mcp.json`, whether or not the operator approved it;
  - `managed-mcp.json` at the platform's path.

  The managed path is one `cfg`-selected function, used unless `RunnerConfig::managed_mcp`
  (below) overrides it. `urls` holds the `url` of an `http` or `sse` entry. For a `stdio`
  entry it holds every element of `command`, `args` and the values of `env` that parses as
  an `http(s)` URL. `env` is this task's addition to D30's list, because `mcp-remote`-style
  bridges are configured either way. `source` names the file, and for `.claude.json` the key, for
  example ``~/.claude.json (projects["/Users/bob/src/app"])``.
- **`RunnerConfig` gains three fields, all `None` in `Default` and in production:**
  - `home: Option<PathBuf>`. When set it is the resolver's home and is exported to the
    child as `HOME`. Otherwise `home` is the child's: the plan's `env_set` when a provider
    sets `HOME`, then this process's `HOME`, then `USERPROFILE` (the fallback
    `crates/core/src/openers/mod.rs` uses);
  - `managed_mcp: Option<PathBuf>`;
  - `server_origin: Option<Url>`, for `OwnEndpoints` below. The host sets it from
    `runner_identity.server_url` (040) through 049's `identity::current`, as 054 fills
    `doctor::Environment.connected`, so a spawn in `rimaia-core` never reads `runner.db`.

  So that no test depends on the machine running it, every config a test spawns with sets
  `home` to a `TempDir` and `managed_mcp` to a missing path in it: `RunnerFixture`,
  `TestContext`, `testing::doctor`, and each test that builds a `RunnerConfig` itself. The
  test repository keeps its git identity in repository config (`testing/repo.rs`), so a
  child still commits under that `HOME`.
- **`mcp::OwnEndpoints { loopback_ports, server_origin }` and its pure `is_own(&self, url) ->
  bool`**, parsed with `reqwest::Url` (no new dependency). A URL is Rimaia's when either of
  these holds:
  - its host is loopback and its port is one of `loopback_ports`, whatever the path.
    Loopback means `localhost` and any `*.localhost` name (with or without a trailing dot),
    `127.0.0.0/8`, `[::1]`, IPv4-mapped `[::ffff:127.0.0.0/104]`, and the unspecified
    addresses `0.0.0.0` and `[::]`. The last three are this task's widening of D30's list:
    on Linux and macOS a connection to the unspecified address reaches a listener bound to
    `127.0.0.1`, so `http://0.0.0.0:4517/mcp` is the operator endpoint under another
    spelling;
  - its origin (scheme, host, port) equals `server_origin`.

  `loopback_ports` is built per spawn from `RunHandles::endpoint()`'s bound port,
  `mcp::configured_port` and `DEFAULT_PORT`. `server_origin` is `RunnerConfig`'s. A
  hostname that only DNS resolves to loopback is not recognised, and Scope 10 states it as
  residual.
- **The denial.** `ForbiddenOperation::RimaiaToolSurface` becomes
  `RimaiaToolSurface { aliases: Vec<String> }`. `claude::spell_out` spells each alias exactly
  as `rimaia`: `mcp__<name>`, then `mcp__<name>__<tool>` for every `Tool::ALL`, with the name
  normalised by `tool_handle`'s `[A-Za-z0-9_-]` rule (D30 point 3). `rimaia`'s own patterns
  come first, then the aliases in the order the files were read, deduplicated keeping the
  first occurrence. So the commonest setup, `rimaia` registered at the operator port, adds
  nothing to argv.
- **Where it runs.** Per spawn, never cached, and only when the intent's `run_environment`
  is `inherit`. It runs at the point where `prompt` and `workspace` are filled in, for every
  intent the runner builds: implementation, review and fix (021). The planner is
  `strict_local`, so it never reads a file. `plan_spawn` stays a pure function of the intent,
  and argv stays pinnable byte for byte.
- **What the files can do to a run.**
  - A missing file contributes nothing.
  - An unparseable file is logged with `tracing::warn!` and the task id, exactly as
    `plan.warnings` are logged today, and contributes nothing, because the CLI cannot load
    servers from it either. The same goes for an unreadable one. That log line is what this
    task takes D30 point 6's "a warning on the run" to mean; nothing is stored on the run.
  - An inherited registration **named** `rimaia-run` refuses any intent that carries a
    handle, on `RefusalAxis::HandleInjection`, before anything is spawned. The message is
    exactly:

    ```
    An MCP server named "rimaia-run" is already registered in <source>. Rimaia gives this run its own server under that name, and which of the two the agent CLI would load is not known, so the run was not started. Rename or remove that registration.
    ```

    Only review and fix can meet this, since the planner reads no file. Either is 021's
    phase refused before it spawns: a row of its kind, finished `fatal`, with this message
    as `error_message`. For a review that is a failed review (D30 point 7). An intent
    without a handle is not refused, and if that registration's URL is Rimaia's it is
    denied like any alias. That emits `mcp__rimaia-run` patterns for a handle-less intent,
    so D30 point 2's "no `mcp__rimaia-run` pattern for any intent" narrows to any intent
    that carries a handle. Scope 10 records the narrowing.

**8. Two CLI facts, recorded (D30 point 8).** One recording against the pinned CLI in
`crates/core/tests/fixtures/cli/`, captured the way 035 captured its fixture: from a shell,
with stand-in stdio servers. It is added to `RECORDED` in `tests/harness.rs`, and the
fixtures README gets a section saying what was faked. The recording pins:

- a server whose name has a character outside `[A-Za-z0-9_-]` (`my.board`) is called as
  `mcp__my_board__<tool>`;
- which of D30's files the pinned CLI loads servers from, read off the `init` event's
  `mcp_servers`. That covers a top-level `.claude.json` entry, an entry under a project key
  that is **not** the working directory, and an unapproved `.mcp.json`.

The resolver reads every file D30 names, whatever the recording shows, because reading one
too many costs a denial and missing one costs the rule. The tests are named for the
behaviour that was recorded. If `managed-mcp.json` cannot be written on the recording
machine, the README says so, and no test name claims it was observed.

**9. The mark, end to end.** Task 045's `written_during_run(conn, actor)` needs no change.
This task makes sure every door that writes consent-gated content on behalf of a runner's
owner reaches it with that owner as `ctx.actor`:

- a run's forwarded `update_task`, through Scope 3's context;
- the owner's own desktop token or browser session, through 046's `/api/v1` routes and
  047's authentication. D32's surface table admits only those two doors there. The owner's
  `rmp_` token reaches the board only through the hosted `/mcp`, which is 060's.

The tests in the acceptance criteria prove it across `HttpBoard` and the real server. This
task adds no code path that writes plan text without going through 045's helper.

### Records

**10. The seam entry, and the rest.**

- `docs/seam-contract.md`: a new entry under the next free D number, "Task 055's
  cross-cutting choices", in the four-part shape. It records:
  - where the proxy lives, that it holds only a port, handles and a `FenceHook`, and that
    the hosts pass `fence::on_fenced` through it;
  - `RunHandles` carrying the `LeaseRef`, and `bind`'s `port`;
  - the context of a forwarded call (the leased task's team, `Actor::User(owner)`, `Mcp`
    source);
  - the grant read from the lease row, including the D30 point 5 refinement: an
    `implementation` lease with no `run_id` is `Grant::Strategy`;
  - the implementation-lease refusal wording;
  - no outbox and no retry for `run_tool`, and `Conflict` handed to the hook;
  - `RimaiaServer::scoped` behind `testing`;
  - `RunnerConfig::home`, `managed_mcp` and `server_origin`, and who sets the last;
  - the widened loopback set and the `env` values;
  - the `rimaia-run` collision message, and the narrowing of D30 point 2's test to
    handle-carrying intents;
  - the unparseable-file warning as a log line only;
  - the residual, stated: DNS names that resolve to loopback, plugin servers, claude.ai
    connectors, and a `bypassPermissions` run reading a token out of a file (ADR-0032 point
    6's last bullet). On a connected machine, 059's token on the loopback operator endpoint
    is what holds against those.
- D30 gets dated one-line pointers to that entry: under point 2 for the narrowing, under
  point 5 for the planner's grant, and under point 6 for the loopback and `env` widening and
  the log line. D31 gets one under point 7 if Scope 4's typed-method reply had to change.
  Nothing else in either is edited.
- The "How to use this" table in `docs/seam-contract.md` gains a row for 055: D4 · D6 ·
  D8 · D17 · D27 · D28 · D30 · D31 · D32 · D33 · D34, 045's entry and the new one.
- `rimaia-runner` gains `rmcp`, with the version and client features `rimaia-core` already
  uses, as a dev-dependency for `tests/run_proxy_http.rs`. That is not a new dependency
  under D6 or D34.
- `CLAUDE.md`, Gotchas: "Always strip inherited `CLAUDE_*` env vars" becomes "Always strip
  inherited `CLAUDE_*` and `RIMAIA_*` env vars", with its sentence about process identity
  extended to say that `RIMAIA_*` is credentials. Nothing else in CLAUDE.md changes. No
  command is added, so CI does not change.
- If a query is added or changed (the lease-row read in Scope 3 is the likely one), both
  offline caches are regenerated with D33's recipe and committed.

## Out of scope

- **The loopback operator endpoint's token** once connected (ADR-0030 point 6, ADR-0035
  point 4). That is 059's, and it is the control that holds against what the resolver cannot
  see.
- **The hosted operator `/mcp`**, the `team` argument, `list_teams` and strategy planning as a
  request: 060.
- **Headless pairing, and the binary that calls `run_proxy::bind`:** 058. This task provides
  the function and tests it.
- **Consent itself**: revisions, acceptances, trust, and the mark's rule. All of these are
  045's. This task changes no rule in `crates/core/src/consent/`.
- **Showing the resolved aliases** in the doctor or the run detail. They are in argv, which
  the transcript's `init` event and the log already show.
- **Resolving hostnames**, reading plugin-provided servers, and claude.ai connectors (D30
  point 6's residual).
- **Any interface change.** No component under `src/` changes.
- **Any migration.** None is reserved for 055 in D28's D4 amendment. If one seems needed,
  stop and ask.
- **Any new dependency.** `reqwest::Url`, rmcp's server and client halves, and the runner
  and server crates' HTTP stacks are all already present (D6, D34).

## Acceptance criteria

**Part A: dispatch, port and server check**

- `crates/core/src/mcp/run_tools.rs` exists, and the seven handlers call its body functions.
  `every_tool_a_grant_may_call_has_exactly_one_run_door` iterates `Tool::ALL` × every
  `GrantKind`. A tool whose `run_access` is not `Refused` reaches either a `run_tools::call`
  arm or one of the two typed port methods, never both. Every other tool is refused by
  `authorize`'s own wording.
- `BoardMethod::ALL` includes `RunTool`. 043's exhaustive
  `every_lease_method_refuses_a_stale_generation` gains exactly one `RunTool` arm, which
  calls `run_tool` with a stale generation, expects `Conflict` and checks that nothing was
  written. It passes through both adapters with no other change. 036's
  `every_lease_method_answers_not_found_for_a_task_that_does_not_exist` gains `RunTool`, and
  `every_board_dto_round_trips_through_json` gains a `RunToolCall`, the same way.
- These cases are added to `crates/core/src/testing/board_contract.rs` and pass through
  `InProcessBoard` (`tests/board_port_in_process.rs`) and through `HttpBoard` against the
  real server (`crates/runner/tests/board_port_http.rs`):
  - `run_tool_refuses_a_tool_the_grant_does_not_allow` (`move_task` under a strategy lease);
  - `run_tool_refuses_another_task` (`update_task` naming a second task in the same team);
  - `run_tool_refuses_the_two_typed_write_backs`;
  - `run_tool_refuses_every_tool_to_an_implementation_lease`, asserting the exact message
    from Scope 3, after `start_run`;
  - `a_planner_inside_a_run_may_read_and_amend_its_own_task_before_start_run`: under a
    fresh `implementation` claim with no `run_id`, `get_task` and `update_task` on the
    leased task succeed. After `start_run` the same lease is refused with Scope 3's message;
  - `run_tool_under_a_lease_another_runner_holds_is_a_conflict_and_writes_nothing`;
  - `a_forwarded_write_is_the_runners_owners_and_reads_as_an_agents`: the published change
    event and the task row carry the owner as actor and `MutationSource::Mcp`;
  - `an_unscoped_read_through_run_tool_sees_only_the_leases_team`: the owner is a member of
    two teams, and `list_repositories` returns only the lease's team's repositories;
  - `a_fix_lease_resolves_findings_under_its_own_run_and_a_review_lease_cannot`, where
    `fix_run_id` is the lease row's `run_id`.
- `a_tool_name_the_server_does_not_know_is_invalid`: a `run_tool` body naming a tool that
  does not exist is answered `Invalid` over HTTP, with no 500.

**Part A: the proxy**

Core tests over the in-process adapter, with a real rmcp client at a real loopback URL:

- `the_proxy_introduces_itself_as_rimaia_run` checks `get_info`'s name.
- `the_proxy_offers_the_tools_the_in_process_server_offers` compares `tools/list` with the
  combined router's list, schemas included.
- `a_forwarded_call_answers_what_the_in_process_tool_answered` compares the `CallToolResult`
  with a direct `RimaiaServer::scoped` call, value for value, for `get_task`, `update_task`,
  `set_task_strategy` and `record_review_findings`.
- `the_runner_refuses_before_the_board_is_asked`: a counting `BoardPort` wrapper records
  **zero** calls for a `Refused` tool and for another task's `task_id`, and the refusal text
  equals `RunScope::authorize`'s.
- `a_refusal_reads_the_same_whichever_check_made_it`: `remove_task_link` on another task's
  link, refused by the board, has the same `{ code, message }` payload shape as `update_task`
  on another task, refused by the runner. Both equal the strings
  `tests/mcp_scope.rs` already asserts.
- `the_planners_strategy_arrives_through_record_strategy`: the counting wrapper sees
  `RecordStrategy`, never `RunTool` with `set_task_strategy`, and the task's strategy source
  is `planner`.
- `an_unreachable_board_is_an_error_the_run_can_read_and_nothing_is_written`: over an
  `HttpBoard` pointed at a closed port.
- `a_fenced_lease_answers_conflict_and_hands_the_lease_to_the_hook`: the lease is released
  under the proxy. The run's call gets `Conflict`, and a recording `FenceHook` receives
  exactly that `LeaseRef`, once.
- These existing tests pass with their assertions unchanged, now through the proxy:
  `an_unknown_token_is_not_routed_at_all`, `a_token_stops_working_when_its_run_ends`,
  `a_real_client_at_a_scoped_url_is_refused_a_task_that_is_not_its_own`, and every test in
  `tests/runner_strategy.rs`.

End-to-end, in `crates/runner/tests/run_proxy_http.rs`: `run_proxy::bind` forwarding through
`HttpBoard` to the real `rimaia-server` router on `127.0.0.1:0`, with one `TestClock`:

- `a_run_on_a_remote_runner_reaches_its_own_card_through_the_server`;
- `the_run_token_never_reaches_the_server`: a recording layer on the server captures every
  request line, header and body, and the grant's token appears in none of them;
- `a_runner_that_skips_its_own_check_is_still_refused_by_the_server`: `HttpBoard::run_tool`
  called directly with `move_task`, and with another task's id, under a valid strategy lease.
  Both are refused, and neither writes;
- `a_forwarded_call_after_the_lease_expired_is_a_conflict`: the clock advances past 053's
  `LEASE_LIFETIME` without a heartbeat;
- `a_fenced_lease_stops_the_run_and_keeps_its_worktree`: the hook is `fence::on_fenced`,
  and a gated `FakeCli` planner's call arrives after the lease was fenced with the faked
  clock (no `sleep`). The child is stopped through the normal cancel path, and the worktree
  in the `TempDir` repository is kept with its commits;
- `bind_serves_on_the_port_it_is_given_or_one_the_os_chooses`: `bind` with a free port
  serves on it, and with `0` serves on the address `RunHandles::endpoint()` reports.

**Part B: the strip**

- `a_run_inherits_no_rimaia_variable`, a pure test of `inherited_identity_vars` over
  `RIMAIA_RUNNER_TOKEN`, `rimaia_personal_token`, `Rimaia_Data_Dir`, `RIMAIAX` and
  `MY_RIMAIA_NOTE`. The first three are stripped and the last two are kept.
- The existing process-level test changes exactly as Scope 6 says. A spawned `FakeCli` child
  whose parent carries `RIMAIA_TOKEN_UNDER_TEST` never sees it, in either `run_environment`.
- The on-archive script tests in `tests/archive.rs` pass unchanged, so the variables Rimaia
  sets still arrive.

**Part B: deny by URL**

- `every_config_file_the_cli_reads_contributes_its_servers`: a `TempDir` home with top-level
  servers and servers under two project keys, a workspace `.mcp.json`, and a managed file at
  an injected path. Every registration appears with its `source`.
- `a_stdio_bridge_to_the_operator_endpoint_is_found_by_its_arguments_or_its_env`: one
  registration with `mcp-remote http://127.0.0.1:4517/mcp` in its args, and one with the URL
  in an `env` value.
- `a_missing_file_contributes_nothing_and_an_unparseable_one_is_reported`: the unparseable
  file is in `unreadable`, and the others still contribute.
- `own_endpoints_recognise_every_spelling_of_the_operator_port_and_the_server`, a table test:
  - own: `127.0.0.1`, `127.8.9.10`, `localhost`, `LOCALHOST`, `localhost.`,
    `rimaia.localhost`, `[::1]`, `[::ffff:127.0.0.1]`, `0.0.0.0` and `[::]` on a listed port,
    with any path;
  - own: the server's origin with any path, and with its default port written out;
  - not own: a loopback host on an unlisted port, a LAN address on a listed port,
    `rimaia.example.com.evil.net`, the server's host under the other scheme, and a string
    that is not a URL.
- `an_inherited_alias_of_the_operator_endpoint_is_denied_by_name`: argv pinned byte for byte
  with `FakeCli`, for an operator whose `.claude.json` registers `board` at the bound port.
  The disallowed list is `rimaia`'s patterns, then `mcp__board`, then `mcp__board__<tool>` for
  every `Tool::ALL`.
- `a_server_name_the_cli_normalises_is_denied_as_the_cli_spells_it`: `my.board` is denied as
  `mcp__my_board` and `mcp__my_board__<tool>`.
- `strict_local_reads_no_registrations`: the same home, with `strict_local`. Argv carries no
  alias, and the resolver is not called.
- `a_mcp_json_committed_by_the_implementation_run_is_denied_to_the_review_after_it`: a real
  repository in a `TempDir`, and a `FakeCli` implementation attempt that commits a `.mcp.json`
  registering an alias of the operator endpoint. The review phase's argv denies it.
- `an_operator_registration_of_rimaia_at_the_operator_port_changes_nothing`: a home whose
  `.claude.json` registers `rimaia` at 4517 gets exactly the argv an empty home gets.
- `an_inherited_registration_named_rimaia_run_fails_the_review_before_it_spawns`: the
  implementation run with that home is not refused. The review phase never invokes the
  fixture CLI. It writes a `kind = review` row, finished `fatal`, whose `error_message` is
  Scope 7's message exactly, and the card lands in `in_review`, flagged unreviewed.
- D30 point 2's test that `claude::spell_out` emits no `mcp__rimaia-run` pattern passes for
  every handle-carrying intent, as Scope 7 narrows it.
- Every exact-string prompt test in `tests/prompt.rs` and `tests/runner_strategy.rs` passes
  with its expected string untouched.

**Part B: the fixture**

- A new recording in `crates/core/tests/fixtures/cli/` is listed in `RECORDED`, with a README
  section. Its tests are named for what it shows, for example
  `a_dotted_server_name_is_called_with_an_underscore` and
  `the_init_event_lists_servers_from_every_project_key`. If the recording shows otherwise,
  the tests say otherwise.

**Part B: the mark, across the network**

In `crates/runner/tests/run_proxy_http.rs`, with members Alice, Bob and Carol in one shared
team. 045 runs no task assigned to someone else, so the arrangement is: Alice's task is
unassigned, with no base instructions and no dependency, so its plan is its only piece.
Bob's and Carol's runners are both `assigned_then_pool` with the team listed. Bob accepts
Alice's plan revision, his runner claims `Plan` on the task, and Carol trusts Bob:

- `a_plan_a_run_rewrites_through_its_handle_on_someone_elses_task_is_marked`: a forwarded
  `update_task` that changes the plan leaves `plan_updated_by = bob` and
  `plan_written_during_run = 1`.
- `a_plan_the_owner_writes_from_their_desktop_while_their_runner_holds_someone_elses_lease_is_marked`:
  Bob's `rmd_` token (`Door::Desktop`) edits another of Bob's own tasks through `/api/v1`
  while the lease is live. That revision is marked.
- `a_laundered_plan_passes_nobody_on_trust`, after Bob's lease is released:
  - Carol's runner's `claim(Next)` returns `None`, and 045's `consent::status` for her
    runner reports the plan piece `Missing`: the skip is `ConsentMissing`, not a race.
  - Bob's own runner gets the same two answers.
  - After Bob accepts exactly that revision, Bob's runner claims it.
- `a_run_on_its_owners_own_task_writes_unmarked`.

**Records**

- The seam entry, the D30 pointers (and the D31 pointer if it applies), the "How to use
  this" row and the CLAUDE.md line from Scope 10 exist.
- **Solo is unchanged.** No component under `src/` changes, and the 31 frontend test files
  pass. Every pre-existing Rust test passes. The only edits to existing tests are Scope 6's
  marker, the `RunTool` arms in the three contract cases above, and the test configs
  gaining Scope 7's `home` and `managed_mcp`. The full CLAUDE.md command list passes,
  including `cargo test` for the runner and server crates.

## Notes

**Read first.** ADR-0035 (point 5, and point 4 for what 059 adds), ADR-0032 (points 3 and 6),
ADR-0026 (points 1, 2 and 4), ADR-0006 and its 2026-08-28 amendment, ADR-0021, ADR-0012,
ADR-0019, ADR-0030 points 3 and 6, and ADR-0031 points 3 and 4.

Seam entries:

- **D30**, all of it, especially points 2, 3, 5 and 6. It is this task's specification.
- **D31** points 2, 3, 4, 7, 9, 10, 11 and 13.
- **D27** point 5 (the identity-variable union) and point 6 (fixtures).
- **D32**: the `Runner` door and `RunnerCaller`, and the `Door::source` mapping this task
  deliberately does not use for `run_tool`.
- **D28**: the `runner_leases` DDL and 045's written-during-run columns.
- **D33**, if a query changes. D8 for the error payload. D17.4 and D17.9.
- D4 and D6 (with D34) as prohibitions. This task adds no migration and no dependency.
- Task 045's seam entry, "Task 045's cross-cutting choices", for the mark's rule.

**Files to start from (on `main` today):**

- `crates/core/src/mcp/mod.rs` (`RunRoute`, `dispatch`, `scoped_service`, `service_over`,
  `MCP_SERVER_NAME`, `DEFAULT_PORT`);
- `crates/core/src/mcp/scope.rs` (`RunScope`, `Tool`, `run_access`, `authorize`,
  `RunHandles`);
- `crates/core/src/mcp/server.rs` (the seven run-reachable handlers, and `set_task_strategy`'s
  `StrategySource::Planner`);
- `crates/core/src/mcp/requests.rs`, `responses.rs`, `error.rs` and `settings.rs`
  (`configured_port`);
- `crates/core/src/runner/process.rs` (`strip_process_identity`, `inherited_identity_vars`,
  `RIMAIA_TOOL_SURFACE`, `forbidden_operations`, the intent built around `workspace` and
  `prompt`, and `spawn`);
- `crates/core/src/runner/provider/mod.rs` (`AgentProvider`, `negotiate`, `RefusalAxis`,
  `identity_prefixes`), `provider/claude.rs` (`tool_handle`, `rimaia_tool_surface`,
  `spell_out`, `mcp_config_json`) and `provider/intent.rs` (`ForbiddenOperation`,
  `RimaiaHandle`, `RunIntent`);
- `crates/core/src/runner/strategy.rs` (the planner's handle and `PlannerAccess`);
- `crates/core/src/archive/mod.rs` (strip first, then set), and
  `crates/core/src/openers/mod.rs` (the `HOME` and `USERPROFILE` fallback);
- `crates/core/src/testing/cli.rs` (`FakeCli`, `commits_on_attempt`, `gates`) and
  `crates/core/src/testing/provider.rs` (Ledger);
- `crates/core/tests/mcp_scope.rs`, `mcp_tools.rs`, `runner_process.rs`, `runner_strategy.rs`,
  `provider_seam.rs`, `prompt.rs`, `archive.rs` and `harness.rs` (`RECORDED`);
- `crates/core/tests/fixtures/cli/README.md`;
- `src-tauri/src/lib.rs` (where `mcp::build` is called).

**Files earlier tasks create, which this task edits:**

- `crates/core/src/board/{port,types,service,in_process}.rs`,
  `crates/core/src/testing/board_contract.rs` and `crates/core/tests/board_port_in_process.rs`
  (036);
- `Grant`, `GrantKind`, `RUN_MCP_SERVER_NAME`, the findings tools and the 035 fixture (035);
- review and fix intents (021);
- `ServiceContext.actor` and `TeamScope` (038);
- the combined board and local routers, and `LocalTools` (041);
- `board::lease::current`, `Conflict` and `LeaseRef.generation` (043);
- the consent module, the `written_during_run` columns and helper, and the `testing` helper
  that adds a member (045);
- `Caller`, `Door::Runner` and the `/api/v1` routes (046); tokens (047);
- `crates/runner/src/board/http.rs`, `crates/server/src/runner_api.rs` and
  `crates/runner/tests/board_port_http.rs` (052);
- `LEASE_LIFETIME`, the expiry sweep and `fence::on_fenced` (053);
- `runner_identity.server_url` (040) and its reader, `identity::current` (049);
- pathless repositories (054).

**What the chain provides.** 035 gives the `rimaia-run` handle, grants keyed by kind, and the
unconditional operator-surface denial. 036 gives the port without `run_tool`. 043 gives the
fence and generations. 045 gives the mark, so this task only has to deliver the right actor to
it. 052 gives the HTTP adapter, the server's runner routes and the contract suite over both
adapters. 053 gives expiry and the fenced-lease reaction. **If any of these is not where this
file says, stop and ask.** In particular, if 053's reaction to `Conflict` is not one callable
function, do not write a second one behind the `FenceHook`.

**What the next tasks expect.**

- 056 expects `run_tool` never to be in the outbox.
- 057 expects a fenced run's worktree to be kept. That is 053's reaction, reached from here
  too.
- 058 calls `run_proxy::bind` from the headless binary with its `mcp_port` or `0` and a
  `FenceHook` over `fence::on_fenced`, sets `RunnerConfig::server_origin`, and relies on the
  `RIMAIA_` strip for its token variable, whatever it is named.
- 059 swaps the desktop's `board_port` to `HttpBoard`, and the proxy picks it up with no
  change. It sets `RunnerConfig::server_origin` once connected, adds the operator-endpoint
  token that closes what the resolver cannot see, and keeps `rimaia` in every
  `claude mcp add` line.
- 060 reuses `run_tools`' body functions for the hosted `/mcp`, and must not register
  `rimaia-run` anywhere. It carries this task's desktop-door mark test for an `rmp_` token
  on the hosted `/mcp`, since that is the only door the token opens.
- 064 folds Scope 10's CLAUDE.md line into its final pass.

**A consequence worth knowing.** `RIMAIA_DATA_DIR` is now stripped from every run. A run that
starts the app from a worktree gets no inherited data directory. That is the safer failure,
because the inherited one was the running app's own, but it is a change. CLAUDE.md already
tells an agent to set it explicitly, so no instruction changes.

**Size.** This is an L at the top of the range, about 3,500 lines of diff with tests. The
tests are most of it: the contract cases, two proxy suites, the resolver's table, and the
three-member end-to-end. The two parts share only Scope 10's records, which is why they can
be cut apart. **If it runs over one session, cut after Part A.** Part A (Scopes 1 to 5, with
its criteria) lands as 055. Part B (Scopes 6 to 9) moves to a new task with the next free
number, placed in `tasks/README.md` directly after 055 and before 058. 058 and 059 are the
first tasks that put a Rimaia credential on a runner, so they must not start until it lands.
Neither part is useful split further: a proxy without the server's check, or a resolver
without the alias denial, is one of ADR-0035's two checks, or half of ADR-0032's first rule.
