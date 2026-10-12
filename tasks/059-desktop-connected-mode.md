---
id: "059"
title: Desktop connected mode
milestone: v0.5
status: ready
depends_on: ["058"]
adrs: ["0027", "0030", "0031", "0032", "0033", "0034", "0035", "0036", "0006", "0020"]
size: L
---

# Desktop connected mode

## Goal

Make the desktop app the second host of a connected runner, beside 058's headless binary,
and make "solo or connected" a choice the user makes once and can change later
([ADR-0027](../docs/adr/0027-a-server-for-the-board-and-runners-for-the-work.md) point 4).
After this task:

- **A first launch with nothing to adopt asks one question**: this computer only, or a team
  server. An existing install is already solo and never sees the question.
- **Connecting is a browser sign-in**, never a password field in the app
  ([ADR-0030](../docs/adr/0030-identity-people-sign-in-machines-pair.md) point 4). The app
  opens the system browser on the server's `/auth/desktop`, with a loopback redirect
  `http://127.0.0.1:<ephemeral>/callback` and a PKCE `S256` challenge (RFC 8252). It
  receives a one-time code, exchanges it for an `rmd_` desktop token, and keeps that token
  in the OS keychain. The app never sees the user's GitHub credentials or a server cookie.
- **The desktop pairs its own runner with no code** (ADR-0030 point 5), using the desktop
  token and 047's `pair_own_runner`. The runner is 058's `RunnerHost`, started by the shell
  instead of the binary. There is one connected runner, hosted twice.
- **Pairing says what leaves the machine, and recommends the safer environment.** Before
  the browser opens, the window shows `transcripts::UPLOAD_DISCLOSURE` and the full or
  summaries-only choice ([ADR-0036](../docs/adr/0036-transcripts-and-review-artifacts-leave-the-machine.md)
  point 5), and the run environment with `strict_local` recommended for team use
  ([ADR-0032](../docs/adr/0032-assignment-and-consent-to-run-on-a-machine.md) point 6), as
  058's `pair` does at a terminal. Every Rimaia token this machine holds is redacted from
  every run's transcript, stderr log and tail.
- **The frontend talks to the server.** Board commands go over HTTP with the desktop token
  and board events arrive over SSE, through the transports 049 built and left waiting for
  this task (ADR-0034 point 4). `AppState.board` is `None`, and `rimaia.db` is not opened
  (D32 point 3).
- **Local handlers read the board through one seam.** Every board read a local command
  makes, which 041 and 066 left as named core functions, goes through the runner's
  `BoardPort` or a board command, in process in solo and over HTTP when connected (D32
  point 8 and its 2026-10-04 amendment). Nothing local reads a board pool.
- **Run now on this window is still the interactive run**
  ([ADR-0031](../docs/adr/0031-runners-claim-work-with-leases.md) point 7). The owner
  is at the machine, so it defaults to `acceptEdits`, as it does in solo.
- **The loopback operator endpoint stays, behind a personal access token**
  ([ADR-0035](../docs/adr/0035-mcp-when-the-board-is-remote.md) point 4, ADR-0030 point 6).
  It serves this machine's local tools. The app offers to update Claude Code's existing
  registration with the token, and that costs the user one confirmation. Relaying board
  tools to the server is 060's first commit (Scope 7).
- **Solo is unaffected.** A solo launch opens no socket to anything off the machine, and the
  loopback endpoint is as open as ADR-0006 left it. Its one visible change is Scope 11's:
  a worktree's live status and diff measure from the recorded run's base, never from a
  fresh resolution, so both modes answer alike.

## Why now

058 made the connected runner real without a window. 047 left the server half of the
desktop's sign-in waiting (`/auth/desktop`, `/api/v1/auth/desktop_token`,
`pair_own_runner`), and 049 left its `connected` branch failing loudly, naming this task.
Both are half-built seams until one task joins them. The desktop is also how most people
join a team: they already have the app and a solo board, and the step from there to a
shared board should be one sign-in, not a second install.

The loopback gate cannot wait for later. The moment the app connects, anything that can
reach `127.0.0.1:<mcp_port>` reaches this machine's runner on behalf of every team the user
belongs to, not one person's local board (ADR-0035 point 4). ADR-0030 point 6 makes the
token part of connecting. It is not hardening that can come afterwards. D30 point 6 relies
on it too: against a registration the run-denial resolver cannot see, the token is the
control that holds.

## Scope

**1. Where the mode lives.** There is no new column and no migration. The mode is read from
what already exists (D28 part 6):

- **`connected`**: `runner_identity` has a row with a `server_url`.
- **`solo`**: `runner_identity` has a row with no `server_url`. Or there is no row, but
  `rimaia.db` exists in the data directory. That is an existing install, and setup's solo
  path, 040's adoption included, writes the row as it does today.
- **`unchosen`**: no `runner_identity` row, and no `rimaia.db` file. Check that the file
  exists *before* `db::connect` runs, because `db::connect` opens with `mode=rwc` and would
  create the file.

The rule is one pure function in `rimaia-runner`,
`connection::resolve_mode(board_file_exists: bool, identity: Option<&RunnerIdentity>) ->
DesktopMode`, and `DesktopMode` is an enum, not a string. It lives in the runner crate
because CI tests that crate and runs nothing in `src-tauri`. It takes plain values, so core
still does not depend on the runner. A second pure function beside it,
`connection::desktop_state(mode, desktop_token: StoreStatus, host_end: Option<&HostEnd>)
-> ConnectionState`, adds `signed_out` (Scope 5) and is what `get_connection` answers.

**2. Setup, once per mode.** `src-tauri/src/lib.rs`'s `setup()` branches on
`resolve_mode`, and each branch builds everything once:

- **`solo`:** today's path, unchanged. The board opens in process, `AppState.board` is
  `Some`, the board port is 036's `InProcessBoard`, and the loopback endpoint is open.
- **`connected`:** `rimaia.db` is neither opened nor created. Every `AppState` field that
  held a board `ServiceContext` is `None` here, or moves under `board`. No local handler
  reads `board` in either mode: each board read goes through the runner's port or
  `AppState.board_commands`, as Scope 11 lists them (D32 point 8).
- **`unchosen`:** `runner.db` is opened and migrated. Nothing else is: no board, no
  runner, no MCP listener. The window opens on the chooser (Scope 10).

**The connected runner is `rimaia_runner::host::RunnerHost::start`**, and nothing else.
`setup()` does not call `scheduler::build` in this mode, and builds no `HttpBoard` of its
own. It passes 058's `HostConfig` with the desktop's paths, runner store, `KeyringStore`,
provider and clock, and differs from the headless binary in three places:

- **The lock.** Before opening `runner.db`, the shell takes 058's `<data>/runner.lock`
  with `File::try_lock` and holds it for the life of the process. A held lock refuses
  startup with 058's sentence naming the file. `RIMAIA_DATA_DIR` pointed at a desktop and a
  headless runner at once then cannot put two loops on one `runner.db`.
- **One run route.** `HostConfig` gains `serve_run_proxy: bool`, `true` in the binary.
  The desktop passes `false`, so `start` skips `run_proxy::bind` in its step 6. The host
  exposes the `RunHandles`, the `Arc<dyn BoardPort>` and the `FenceHook` it built, and
  `setup()` hands them to `mcp::build`. `mcp::build` mounts `/mcp/run/{token}` on the
  configured loopback port beside the gated `/mcp`, and calls `RunHandles::set_endpoint`
  once, as in solo (055). There is one listener, one endpoint, and no second claim on
  `mcp_port`.
- **The queue stays where D15 left it.** `HostConfig` gains `start_queue: bool`, `true` in
  the binary (058 step 7). The desktop passes `false`: a desktop launch starts paused, and
  the user presses Start, as in solo.

`HostConfig` carries no presence. The host obeys `Claim::trigger`, and 043 decides presence
by the door, which for this window is the next paragraph.

**Run now from this window is the runner's own claim** (ADR-0031 point 7, 052's
"interactive path"). A new local command, `run_here { taskId, continueSession }`, calls
`RunnerHost::run_here`. That runs 036's starter (D19's slot, the opt-in, `negotiate`,
`probe_cli`) and `claim(Run { trigger: Manual, continue_session })` over the runner's own
`rmr_` token, then spawns `run_task`. It is ADR-0012 point 6's interactive run. 043 decides
presence by the door, and this door is this machine's window. Run now pressed for another
runner (061's picker), from a browser, or from another machine stays the board row and
052's relay, and runs unattended. `run_here` answers `invalid` unless connected.

**How this reads ADR-0031 point 7's "with the app in the foreground"**, recorded as D35
point 7 so that the next task inherits it rather than adding a check. The condition is met
by how the request arrives, not by a query made when it does:

- `run_here` is a `local` command with no MCP tool and no HTTP route. The only way to
  reach it is a click in this machine's Rimaia window, and a window that takes a click is in
  the foreground at that moment.
- An OS focus query at command time could only disagree with the click by racing it: the
  user clicks, then switches application before the handler runs. Refusing then would turn
  a person's deliberate click into an unattended run, which is the opposite of what the
  condition protects.
- Everything that is not a click in this window is unattended: the board row, 052's relay,
  the loopback `/mcp` (which serves no run-starting tool, Scope 7), and a schedule.
- The posture is decided once, at the claim. A run started here that later stalls on a
  prompt after the user walks away stalls exactly as a solo Run now does today. ADR-0012
  point 6 already accepts that.

**When the host ends.** `RunnerHost::ended()` resolves on the server's answer about the
runner itself (058):

- `HostEnd::Unauthenticated`: the host shuts down, `desktop_state` answers `signed_out`,
  and the shell emits a local `connection_changed` event (D7) so the window shows Scope 5's
  sign-in view;
- `HostEnd::UpgradeRequired`: the host shuts down, and the window shows 046's sentence in
  `ErrorBanner`, where 063's updater later offers the fix;
- quitting the app runs `RunnerHost::shutdown` on D15's exit path.

**Changing mode restarts the app.** The choice is written first. Then a local command,
`restart_app`, calls `AppHandle::restart()`. That is the relaunch D34 names in place of
`tauri-plugin-process`. `setup()` builds the runner, the MCP listener, the board port and
`AppState` once. A mode is a property of all four. A restart keeps one construction path
per mode, instead of adding a teardown path that nothing else uses.

**3. Browser sign-in (RFC 8252).** New: `crates/runner/src/connection/sign_in.rs`.

- **The server address** is normalised to an origin: scheme, host and port, with no path,
  query, fragment or userinfo. It must be `https://`, except that `127.0.0.1`, `localhost`
  and `[::1]` may use `http://` for development. Anything else is refused before a
  listener is bound or a browser opened, with exactly: `A team server address must start
  with https://. Only 127.0.0.1, localhost and [::1] may use http://.`
- **The listener** binds `127.0.0.1:0`. It uses the literal loopback address, never
  `localhost` (RFC 8252 §7.3, and 047's `/auth/desktop` refuses anything else). It
  answers `GET /callback` and nothing else:
  - any other path gets a `404`;
  - a callback whose `state` differs from the desktop's own ends the sign-in, stores
    nothing and closes the listener. The comparison is constant time, through 047's
    `identity::secret`;
  - the page it returns is fixed text ("Signed in. You can close this tab and return to
    Rimaia."). Nothing is echoed from the query string.

  The listener closes after one successful callback.
- **The URL the browser opens** is `<origin>/auth/desktop?redirect_uri=…&state=…&
  code_challenge=…&code_challenge_method=S256&label=…`, in that order, built with
  `reqwest::Url::parse_with_params`. `label` is the host label (Scope 4), which 047's
  route takes as the `rmd_` token's label. The verifier and the state come from 047's
  `mint_verifier` and `pkce_challenge`. Opening the browser is an injected
  `FnOnce(&Url) -> Result<()>`. The shell passes `tauri_plugin_opener`'s `open_url`, and
  the tests pass a closure that plays the browser. `opener:default` is already granted,
  and no capability changes.
- **The exchange** is `POST <origin>/api/v1/auth/desktop_token` with `{ code, codeVerifier,
  redirectUri }` and the `Rimaia-Protocol` header, as 047 defines it. The answer is 047's
  `NewApiToken`.
- **The wait ends in one of three ways:** the callback arrives; the user cancels
  (`cancel_connect`); or ten minutes pass. Ten minutes is how long 047 keeps the pending
  entry. The deadline is awaited through `Clock::sleep_until`, so a test advances a
  `TestClock` and never sleeps. An expiry or a cancel stores nothing and closes the
  listener.
- **`upgrade_required`** from any call in this flow becomes: `This version of Rimaia is
  too old for <origin>. Update Rimaia, then connect again.` `unauthenticated` and
  `invalid` are passed through unchanged (D8). A `fetch`-level failure names the origin.

**4. Pairing, and what is stored where.** After the exchange, and in the same command:

- **Pair.** Call `pair_own_runner` with `{ label, provider }` over HTTP with the desktop
  token. The label is the host label: 058's hostname function, falling back to D28 part 3's
  `This computer` where that function refuses. `provider` is the configured provider's
  `ProviderId::as_str()` (D27).
- **Store the tokens in the keychain** (ADR-0030 point 4), through ADR-0020's
  `CredentialStore` and 058's key convention. 056 already added `RunnerToken { runner_id }`
  and `DesktopToken { runner_id }` to 054's `CredentialKey`, with the accounts
  `runner-token:<runner_id>` and `desktop-token:<runner_id>`, because its
  `secrets::host_secrets` reads them first. This task adds the third,
  `LoopbackMcpToken { runner_id }`, account `loopback-mcp-token:<runner_id>`, and writes
  the first two through 058's `save_runner_token` and a `save_desktop_token` beside it.
  Keying by runner id is what stops two development data directories (ADR-0023) from
  sharing one keychain item.
- **The two pairing choices** travel with `connect_to_server` (Scope 9) as
  `uploadTranscripts: "full" | "summaries_only"` and `runEnvironment: "inherit" |
  "strict_local"`. Both are required on a first connect and both are optional on **Sign in
  again**, where an absent value leaves the stored one alone. They are not written up
  front: a solo machine whose connect fails keeps its own `run_environment`, because
  041 made that a runner key both modes share.
- **Record the server last.** Write `runner_identity` (`runner_id`, `server_url` = the
  origin), `upload_transcripts` (056's typed accessor) and `run_environment` (041's) in one
  runner-store transaction, *after* the desktop and runner tokens are stored. If either
  accessor takes only a context, add a variant that takes the transaction beside it, so
  the value is still parsed in one place (D3). A machine is then connected with its
  choices, or not connected at all.
- **A failure before the server is recorded** leaves the machine in the mode it was in.
  It deletes every keychain item the attempt wrote. Then, best effort and with the desktop
  token, it revokes the runner token, which unpairs the runner, and then the desktop token.
  The error names each credential it could not revoke, in Disconnect's words (Scope 5).
- **Nothing secret reaches a log line.** No log line, `Debug` output, error message or
  returned DTO carries a token, code, verifier or state. There are two exceptions, both
  named in Scope 9: `get_server_connection`'s answer, which is the desktop token that 049's
  transport is built to carry; and the `token` field of the `RegistrationReport` that minted
  a personal access token, which is that token shown once (ADR-0030 point 3).
- **Every Rimaia token this machine holds is a host secret** (ADR-0036 point 4).
  056's `secrets::host_secrets` reads the runner and desktop tokens. This task extends it to
  read `LoopbackMcpToken` too, so a connected desktop's `RunnerConfig::host_secrets`
  redacts all three from the transcript, the stderr log and the tail. The loopback token
  matters most of the three: in `inherit` mode Claude Code's own configuration, which a run
  can read, holds it as a header (Scope 8).
  - **A token minted while the host runs** reaches later runs without a restart.
    `RunnerConfig::host_secrets` becomes `HostSecrets`, an `Arc` over a lock around 056's
    `Redactor`, keeping its hand-written `Debug`. `execute` reads it once per spawn, and
    `RunnerHost::add_host_secret(&Secret)` merges into it through `Redactor::merged`.
  - **A run already in flight cannot gain it**, because its redactor was taken at spawn.
    So `update_claude_code_registration` refuses to mint while this machine's `InFlight` is
    not empty, as `invalid` with exactly: `Let this computer's runs finish before issuing
    an MCP token. <n> running.` Only minting is refused. A call that finds a token already
    stored updates registrations at any time.

**5. Switching later.** Settings → Connection offers "Connect to a team server" in solo and
"Disconnect" when connected.

- **Both are refused unless the runner is idle.** The queue must be stopped, `InFlight`
  empty (D19), and `held_leases` empty. The refusal is `invalid`, with exactly:
  `Stop the queue and let this computer's runs finish before switching. <n> running,
  <m> held.` A lease held across a switch would belong to a board this process no longer
  talks to.
- **Connecting from solo** runs Scopes 3 and 4, then restarts. The solo board, its tasks
  and its `solo_identity` are left exactly as they are. `checkouts`, `worktrees`,
  schedules and `runner_settings` are kept: they are this machine's, and ADR-0033 point 2
  keys checkouts by repository id, so solo and team rows cannot collide.
- **Disconnecting** is best effort toward the server and certain on this machine:
  1. Using the desktop token, revoke this runner's `rmr_` token, which unpairs the runner
     (047's `unpair_runner`, and 057's pin release). Revoke the loopback personal access
     token, and the desktop token itself last. Each is `revoke_api_token` on an id from
     `list_api_tokens`.
  2. Delete the three keychain items and `loopback_mcp_token_id`. Every other
     `runner_settings` key stays, `run_environment` and `upload_transcripts` included. A
     `strict_local` chosen at pairing stays this machine's choice in solo, where Settings →
     Instructions shows it, and `upload_transcripts` has no effect in solo.
  3. Create and migrate `rimaia.db` if it does not exist, and run `identity::ensure_solo`
     on it.
  4. Replace `runner_identity` with `(solo_identity.runner_id, NULL)` in one runner-store
     transaction. D28's "The runner set" names disconnecting as a writer of this row. It
     must be written here: 040's adoption writes it only while the `settings` adoption row
     is absent, and on a machine that was solo before connecting that row exists.
  5. Restart. The next launch resolves to `solo`, and 040's refusal of a store from
     another board passes, because the ids match.

  If the server cannot be reached, steps 2–5 still happen. The result then names each
  credential that is still listed on the server's account page, so the user can revoke it
  there.

  On a machine that went from `unchosen` straight to connected, that relaunch runs 040's
  adoption for the first time, over a runner store that already holds this machine's
  connected-era rows. **Adoption keeps what the store already has.** The `settings` step
  inserts a `runner_settings` key only where the store lacks it, and leaves an existing
  `runner_identity` row alone. If 040 landed overwriting either, change it here, with the
  test below.
- **Signing in again.** The window shows 050's `SignInView` with the desktop's action,
  **Sign in again**, in place of the GitHub link. It does so in three cases:
  - a connected launch finds the desktop token missing or the keychain locked
    (`get_connection` answers `signed_out`);
  - the runner host ended `Unauthenticated` (Scope 2);
  - at runtime, a board command or `subscribeToEventStreamFailure` answers
    `unauthenticated`. 050 already returns to that view on either.

  It never falls back to solo, for D32 point 3's reason: a transport bug should fail on the
  first click, not write to a board nobody reads. **Sign in again** is `connect_to_server`
  with the stored origin, then `restart_app`. It replaces the desktop token, and pairs
  again only when `list_api_tokens` no longer shows a runner token for
  `runner_identity.runner_id`. A runner whose own token is still valid keeps working while
  the window is signed out. It answers to its `rmr_` token, not to the window's.

**6. The loopback endpoint, gated.** In `crates/core/src/mcp/mod.rs`, `mcp::build` gains a
`LoopbackGate` argument: a shared handle over `LoopbackAuth::{Open, Locked, Token { hash
}}`, read on every request. Solo passes `Open`. Connected passes `Token` when the keychain
holds a loopback token and `Locked` when it does not, which is the case after **Not now**.
The command that mints a token (Scope 8) sets `Token` on the same handle, so a locked gate
opens without a restart.

- **Only the operator route is gated.** Under `Token`, `/mcp` requires `Authorization:
  Bearer rmp_…`, and the presented token's `identity::secret::hash` must equal the stored
  one, compared in constant time. Under `Locked`, nothing passes. Anything refused gets
  `401` with `WWW-Authenticate: Bearer` and D8's body:
  `{"code":"unauthenticated","message":"This Rimaia is connected to a team server. Its MCP
  endpoint needs the token from Settings → MCP."}`. `/mcp/run/{token}` is never gated by
  this: it has its own token and its own table (D30, 055).
- **The bind does not change.** It is still the `127.0.0.1` literal and the configured
  port, and a busy port is still a status, not a startup failure (D16 point 7).
- **`McpStatus` gains `requiresToken: bool`**, on both sides, so the add line and Test
  connection can say what the endpoint expects. In connected mode, Test connection sends
  the keychain's loopback token.
- **A revoked token locks the gate.** At every connected launch the app checks that
  `loopback_mcp_token_id` is still in `list_api_tokens`. If it is not, the app deletes the
  keychain item and the id, and the gate is `Locked` until Settings → MCP issues a new
  token. If the server cannot be reached, the stored token stays. A token revoked from the
  account page therefore stops working locally at the next launch that reaches the server.
  060's relay adds the faster path: an upstream `401` locks the gate at once.

**7. Board tools on the loopback: refused here, relayed from 060.** When connected, the
loopback server does not serve 041's board router, because there is no board to serve it
from. It serves 041's local router, through a `BoardTools::{Local(ServiceContext), Remote
{ origin }}` argument to `mcp::build`.

- **`tools/list`** answers the local tools. These are ADR-0035 point 6's machine tools,
  `plan_task_strategy` and `plan_tasks_strategy` included, which start the planner locally
  over the runner's board port.
- **`tools/call`** for any other name that `Tool::from_name` knows is a tool error, with
  exactly: `Rimaia's server at <origin> does not serve MCP yet.`
- **The server name stays `rimaia`.** `get_info` still reports `MCP_SERVER_NAME`, and
  nothing registers `rimaia-run` in a user's configuration (D30 point 1).

**The relay, which 060's first commit builds** in place of that refusal. It is decided
here because it is this endpoint's behaviour:

- `Remote` gains an `Arc<dyn McpUpstream>`: a core trait in `crates/core/src/mcp/relay.rs`,
  implemented in `crates/runner/src/connection/mcp_upstream.rs` over rmcp's
  streamable-HTTP client. It lives in the runner crate because that crate already enables
  reqwest's TLS (052), and core's reqwest stays plain HTTP to loopback (D34).
- Non-local calls are forwarded to `<origin>/mcp` with the loopback token as the bearer.
  That is ADR-0035 point 4's "as its signed-in user": the server applies the token's team
  restriction and records `Door::Mcp`, so a plan created from a Claude Code session keeps
  `tasks.source = mcp` (ADR-0019). Relaying through `/api/v1` with the desktop token would
  need a second copy of ADR-0035 point 2's `team` rules, and would record the source as
  `ui`.
- `tools/list` answers the upstream's tools merged with the local ones. **A local name
  shadows an upstream tool of the same name**, so `plan_task_strategy` and
  `plan_tasks_strategy` appear once and start the planner here. An upstream failure during
  `tools/list` answers the local tools alone and logs a warning, so Claude Code never marks
  the whole server failed.
- Upstream failures on `tools/call` are tool errors: a `401` is `Rimaia's server at
  <origin> refused this machine's MCP token. Issue a new one in Rimaia → Settings → MCP.`
  and locks the gate; a connection failure is `Could not reach Rimaia's server at <origin>:
  <error>.`
- 060's tests for it: `tools_list_without_a_hosted_mcp_is_the_local_tools`,
  `a_local_tool_shadows_an_upstream_tool_of_the_same_name`,
  `board_tools_are_forwarded_with_the_loopback_token`,
  `local_tools_are_answered_here_and_never_forwarded`,
  `an_upstream_401_locks_the_loopback_until_a_new_token_is_issued`, and
  `an_unreachable_server_is_a_tool_error_naming_it`.

**8. One step to update Claude Code's registration.** New:
`crates/core/src/mcp/registration.rs`. It lives in core because it spawns a local program
and makes no network call.

- **When it is offered.** After the restart into connected mode, while
  `loopbackTokenIssued` is false. The gated listener is bound then, so the URL written is
  `McpHandle::url()`, the bound port. If the listener is not bound (D16 point 7), the
  update is refused with exactly: `Rimaia's MCP endpoint is not listening, so there is no
  address to register. Choose a free port in Settings → MCP first.`
- **Minting.** `update_claude_code_registration` (Scope 9) calls 047's
  `create_personal_access_token` with the desktop token, `{ label: "Claude Code on
  <host label>" }`, and no team restriction. It does this only when no loopback token is
  stored, and it is the only command that mints one. The secret goes into the keychain
  (Scope 4), the gate (Scope 6) and the host's secrets (`add_host_secret`, Scope 4), and
  its id goes into one new `runner_settings` key, `loopback_mcp_token_id`, with a typed
  accessor (D3). No migration is needed.
- **Finding the registrations.** Use 055's reader of Claude Code's configuration, the
  function behind `ClaudeProvider::inherited_mcp_servers` (D30 point 6), called directly:
  this is Claude Code as an MCP client of Rimaia, which
  `src/components/McpAddCommand.tsx`'s comment already distinguishes from the agent CLI.
  Split `OwnEndpoints::is_own` into `own_kind(url) -> Option<OwnKind::{Loopback, Server}>`,
  with `is_own` kept as `own_kind(url).is_some()` so the run denial is unchanged. A
  **candidate** is an `http` or `sse` registration, under any name, whose URL is
  `Loopback`. Two other matches are reported and never written:
  - a `Server` URL: `Not updated: <name> points at <origin>, the team server, and keeps
    its own token.` A user who chose a team-restricted hosted registration keeps it;
  - a `stdio` entry with a `Loopback` URL in its arguments: `Not updated: <name> runs
    <command> to reach Rimaia. Register Rimaia over http instead, or add the header to that
    command yourself.`
- **Updating each candidate** takes two argument vectors, with no shell (CLAUDE.md):
  `claude mcp remove <name> --scope <scope>`, then `claude mcp add --transport http --scope
  <scope> <name> <url> --header "Authorization: Bearer <token>"`.
  - **User scope** runs from the home directory.
  - **Local scope** (`projects.<path>.mcpServers`) runs with `current_dir` set to that
    project path.
  - **Project scope** (`<repo>/.mcp.json`) and **managed** registrations are never written.
    A token in `.mcp.json` would be committed to the repository, and a managed file belongs
    to an administrator. Each is reported as not updated, with exactly: `Not updated:
    <path> is shared through the repository, and a token written there would be committed.
    Register Rimaia at user scope instead.` (and the managed variant naming the file).
  - **No candidate at all** means one is added at user scope, under `rimaia`.
- **The token on argv is accepted, and the reason is recorded in a comment.** Every
  process that can read another process's argv on this machine runs as the same user, and
  can read `~/.claude.json`, which is where Claude Code stores the header anyway. Editing
  `~/.claude.json` directly is refused: Claude Code rewrites that file while it runs, and
  two writers would race.
- **The report**, `RegistrationReport { token, outcomes }`. `token` is the minted secret on
  the call that minted it and `null` on every other call. Each outcome gives a
  registration's name, scope and result: `updated`, `added`, `not_updated` with its
  sentence, or `failed` with the CLI's own stderr. The token is redacted from stderr with
  D25's exact-value redaction.
- **The CLI facts this relies on are recorded, not assumed** (CLAUDE.md, D30 point 8). Run
  the pinned `claude` with `HOME` set to a `TempDir` and record three things into
  `crates/core/tests/fixtures/cli/mcp-registration/`, with a `README.md` stating the
  version and the commands:
  - `mcp add` refuses a name that already exists in the same scope (so remove-then-add is
    necessary);
  - `--header` lands under the registration's `headers`;
  - `mcp remove --scope` removes only that scope's entry.

  If one of them does not hold, stop and say which, before designing around it.

**9. Local commands.** Each is registered as a `local` row in `api/registry.rs`, gets one
entry in the single `generate_handler!` list, and gets a `local<T>` wrapper in
`src/lib/commands.ts` (D32 points 4–6). None is a board command, and none gets an MCP
tool. Two carry a secret, `get_server_connection` and `update_claude_code_registration`,
which is D25 point 6's exception. The rest act for a person at this machine's window:
opening a browser, starting an interactive run, or deciding where the machine reports.
That is the desktop-referent exception D20.6's 2026-09-04 amendment names. A run able to
reconfigure which server its runner answers to is the laundering path ADR-0032 point 6
exists to close.

| Command | Answers |
| --- | --- |
| `get_connection` | `DesktopConnection { state: "unchosen" \| "solo" \| "connected" \| "signed_out", serverUrl, login, runnerLabel, loopbackTokenIssued }`. Never a secret |
| `get_server_connection` | `{ serverUrl, token }` for 049's `installTransports("connected", …)`. `invalid` unless connected |
| `choose_solo` | Creates and migrates `rimaia.db`, then answers. The frontend calls `restart_app` |
| `connect_to_server` | `{ serverUrl, uploadTranscripts?, runEnvironment? }` → `{ login, runnerLabel }` once Scopes 3–4 complete. A first connect without both choices is `invalid` |
| `cancel_connect` | Ends a pending sign-in. Answers `null` whether or not one was pending |
| `disconnect_from_server` | Scope 5. Answers the list of credentials still on the server, which may be empty |
| `update_claude_code_registration` | Scope 8's `RegistrationReport`. Refuses to mint while a run is in flight (Scope 4) |
| `run_here` | `{ taskId, continueSession }`. Scope 2's interactive Run now. `invalid` unless connected |
| `restart_app` | `AppHandle::restart()` |

`bootstrapClient` branches on `get_connection` and on nothing else. 049's
`get_client_capabilities` stays the components' source (061). In `connected` and
`signed_out` it answers `connected`. In `unchosen` it answers `solo` with `localRunnerId:
null`, because there is no `runner_identity` row to read, and nothing renders it then.
028's fixture table gains a row for each new command, answering the solo shape.

**One existing command's answer widens.** 056's `get_transcript_upload` answers
`TranscriptUpload { value: "full" | "summaries_only", disclosure }`, where `disclosure` is
`transcripts::UPLOAD_DISCLOSURE`. That keeps the paragraph in one place, `rimaia-core`,
instead of a TypeScript copy that could drift from what 058's `pair` prints. The command
reads only `runner.db`, so it answers in `unchosen` too, which is when the chooser needs
it. Its MCP tool keeps its view, and `set_transcript_upload` still has no tool (056).

**10. The frontend.**

- **`src/lib/client.ts`'s `bootstrapClient()`** asks `get_connection` first. `unchosen`
  renders the chooser. `connected` calls `getServerConnection()` and
  `installTransports("connected", { serverUrl, token })`, replacing 049's loud failure.
  `signed_out` renders Scope 5's sign-in view without mounting the board. `solo` is
  today's path. A `connection_changed` event re-runs it.
- **Run now and Retry now.** `startTaskRun` and `retryTaskNow` in `src/lib/commands.ts`
  call `run_here` when the client is connected and the target is this desktop's runner (no
  `runnerId`, or `localRunnerId`). Otherwise they call the board row. The choice lives in
  `src/lib/`, and no component reads the mode (049).
- **`src/views/ModeChooserView.tsx`** shows two choices, in ADR-0024's calm register:
  - *This computer only*: the board stays in a file here, with no account, and nothing is
    sent anywhere.
  - *A team server*: an address field, the two pairing choices below, and **Sign in with
    your browser**.

  While the browser is open, the view says so and offers **Cancel**. Refusals render in
  `ErrorBanner`. On success it restarts.
- **The pairing choices**, in one component, `src/components/ConnectForm.tsx`, which the
  chooser and Settings → Connection's **Connect to a team server** both render, so the two
  doors cannot ask different questions:
  - **Transcripts.** The disclosure paragraph from `get_transcript_upload`, verbatim, then
    two choices, *Upload full transcripts* (selected) and *Upload summaries only*. Full is
    ADR-0036 point 5's default, and 058's `pair` defaults the same way.
  - **Run environment.** The two options `InstructionsSection.tsx` already shows, with
    their labels and descriptions moved into `src/lib/runEnvironment.ts` so both render one
    list. *Strict / local* is selected, with exactly: `Recommended for a team server. A run
    then cannot use the MCP servers in your own Claude Code configuration, or the
    credentials they hold.` That is ADR-0032 point 6's recommendation, and the same default
    058's `pair` gives an empty answer at a terminal. Choosing *Inherit* is one click and
    is never warned against twice. CLAUDE.md's "Inherit is the default" still holds for
    every machine that never pairs.

  **Sign in again** renders neither choice, and sends neither field.
- **The registration offer.** In connected mode, while `loopbackTokenIssued` is false, the
  board shows *Claude Code's registration of Rimaia needs a token now*, with **Update** and
  **Not now**, then the report. **Not now** hides it until the next launch.
- **Settings → Connection** (`src/views/settings/ConnectionSection.tsx`) shows the mode,
  the server, the signed-in login and this runner's label, with Connect or Disconnect.
  Both are disabled while the runner is not idle, and the reason is shown beside them.
- **Transcripts, in Settings → Connection, when connected.** The same disclosure and the
  same two choices as `ConnectForm`, reading `get_transcript_upload` and writing
  `set_transcript_upload` (056). This is the setting's only control after pairing, so it
  lives here, not in 069's `This machine's limits`. It is not rendered in solo, where
  nothing leaves the machine. A change applies to runs started afterwards: 056 fixes
  `transcript_uploads.upload` when a run's row is written.
- **050's account page and team switcher** render in connected mode as they do in the
  browser, which is where Disconnect's leftover credentials are revoked. **Sign out** is not
  rendered there in a desktop: its credential is an `rmd_` token, not a session, and
  leaving the server is Disconnect.
- **Settings → MCP and the Welcome flow's last step** render `McpAddCommand` with
  `--header "Authorization: Bearer rmp_…"` when `requiresToken`, and show **Update Claude
  Code** there. The full token appears once, from the report's `token`, and never again.
- The Welcome flow is otherwise unchanged. It renders after either choice, through
  whichever transports are installed.

**11. Local handlers' board reads.** D32 point 8 says a local handler never reads the
board's `ServiceContext`. Its 2026-10-04 amendment let 041 and 066 call named core read
functions over `AppState.context` until now, each listed in their PRs, and names this task
as the one that converts them. With `AppState.board` set to `None`, an unconverted read
panics on a connected desktop, or, if it had reached a stale `rimaia.db`, answers from a
board nobody reads. Both are worse than a refusal.

- **One seam for a board command, two implementations.** `rimaia_core::api::BoardCommands`
  is a trait with one method, `call(name, args) -> BoardFuture<Value>`, boxed for D31
  point 2's reason. `InProcessCommands { host: BoardHost, caller: Caller }` calls 046's
  `api::dispatch` with the solo `Caller`. `rimaia_runner::connection::HttpCommands {
  origin, token }` posts to `<origin>/api/v1/<name>` with the desktop token and the
  `Rimaia-Protocol` header, and passes D8's error body through unchanged. Scope 4's
  `pair_own_runner`, `list_api_tokens`, `revoke_api_token` and
  `create_personal_access_token` calls go through the same client. `AppState` gains
  `board_commands: Arc<dyn BoardCommands>`, built once per mode in `setup()`.
- **Typed helpers, not names in handlers.** Each read is a core function over `&dyn
  BoardCommands` or `&dyn BoardPort` that deserializes into the core DTO, so a handler
  never spells a command name and solo and connected run the same function.
- **The inventory.** The lists in 041's and 066's PR bodies are authoritative. The table is
  what their task files and 044's hand-off predict. A call one of those PRs lists and the
  table lacks is converted the same way, and this task's PR adds it to the table.

  | Local command | Board fact | Read through |
  | --- | --- | --- |
  | `get_worktree_status`, `get_diff_summary` | The task's repository, and the base that 044's fallback resolved fresh from `dependencies_of` and `latest_successful_head` (044's hand-off to "059 (or 049)") | `get_task`, then `list_runs_for_task`: the latest `implementation` or `fix` run's `base_sha`, or its `base_ref` for a run from before 033 (D29 names the kinds). With no such run, the repository's `default_branch`. Fresh resolution and its warning are removed in both modes, so solo and connected measure the same way. The recorded bundle (033) is what the morning review shows, and it was measured from that same `base_sha` |
  | `remove_task_worktree`, `cleanup_done_worktrees`, `cleanup_merged_worktrees` | D20 guard 1's run state, the `done` selection, and the repository's `default_branch` for "merged" | `get_task` and `list_tasks`. If the board cannot be reached, guard 1 refuses with the transport's error. It has no override (D20 point 1), and an unknown run state is not a spare directory |
  | `get_worktree_inventory` (and its tool `list_worktrees`) | A task's title, column and run state, wherever the inventory shows them beside a record | `list_tasks`. A record whose task the board no longer has is shown as such, as for a deleted task today |
  | `preview_composed_prompt` | The task, base instructions, effective strategy and catalogue | `BoardPort::preview` on the runner's own port. That is the context a claim returns, so the preview stays byte-for-byte a run's prompt (task 006). This adds a second reader to D31 point 4's "only for a starter's preflight", recorded in D35. It is still advisory and writes nothing |
  | `prune_run_logs` | For a run with no `transcript_uploads` row (from before 056), whether it exists and its kind (066's D29 rule) | `list_runs`. 056's never-prune rule for unacknowledged bytes is unchanged |
  | `get_queue_status`, `preview_schedule_preflight` | The plan half | None when connected. The host's queue is 058's runner-only view, which has no plan half, and the preview says, exactly: `On a team server, the server chooses which tasks this computer runs when the window opens.` Solo keeps 042's `SoloBoard` |
  | `plan_task_strategy`, `plan_tasks_strategy`, `cancel_plan_pass` | The planner's refusals and its claim | The runner's own port (`preview`, `claim(Plan)`), as 036 and 041 left them. No change, listed so nobody converts them twice |
  | 054's mapping commands | Which board repository a clone maps to | `find_repositories` and `report_runner` on the runner's own port, as 054 built them. No change |

- **`src-tauri/src/commands/` names `state.board` nowhere.** In solo `setup()` hands the
  board context to `InProcessCommands`, the in-process board port and the MCP server, and
  to nothing else. `./scripts/check-command-wiring.sh` gains that grep, so a later handler
  that reaches for the pool fails CI instead of a connected desktop.

**12. Documentation.** CLAUDE.md's Gotchas gains one bullet. A connected desktop's loopback
`/mcp` requires the personal access token that Settings → MCP issues, and
`/mcp/run/{token}` does not. To develop connected mode, run a local `rimaia-server` and the
app with its own `RIMAIA_DATA_DIR`. Keychain items are keyed by runner id so that two
data directories do not share them. CI gains no command. This task adds queries only to the
runner store, so the runner `.sqlx` cache is regenerated with D33's recipe, and the board
cache is not touched.

**D35 in `docs/seam-contract.md`** records this task's cross-cutting choices, and was
written with this task file: how the mode is derived (Scope 1), the keychain
accounts and the one `runner_settings` key (Scope 4), host secrets (Scope 4), the gate and
`BoardTools` (Scopes 6 and 7), `HostConfig`'s two flags (Scope 2), the reading of ADR-0031
point 7 (Scope 2), `BoardCommands` and the second `preview` reader (Scope 11), the pairing
choices and `get_transcript_upload`'s answer (Scopes 4 and 9), and restart as the only
mode change (Scope 2). "How to use this" has 059's row. Where the code has to differ from
D35, the PR amends D35 in the same commit as the code, as a dated amendment, never by
rewriting the entry.

## Out of scope

- **The hosted `/mcp` itself**, and its `team` argument, `list_teams`, assignee and
  planning-as-a-request: 060. **The relay** to it: 060's first commit, as Scope 7 decides
  it.
- **The server half of sign-in and pairing**: 047. If `/auth/desktop`, `desktop_token` or
  `pair_own_runner` differ from 047's task file as landed, follow what landed. If one is
  missing, stop and say so. Do not add a server route here.
- **Moving or copying a solo board into a team.** ADR-0028's Consequences leave it
  undecided, and ADR-0029's "Copy to team" is one task at a time (051). Connecting leaves
  the solo board exactly as it was.
- **More than one server, or several connected identities at once.** One
  `runner_identity` row per store.
- **A token restricted to some teams for the loopback endpoint.** The minted token is
  unrestricted. A user who wants a restricted one registers the hosted `/mcp` directly
  (ADR-0035 point 1), and Scope 8 never rewrites that registration.
- **Desktop notifications about runs on other machines.** The shell keeps notifying about
  this machine's runs from the runner's own events.
- **Assignment, consent, runners and pinned cards in the interface**: 061. **The signed
  updater**: 063.
- **Browser-mode states** for local-only actions: 050.

## Acceptance criteria

- `resolve_mode` and `desktop_state` are pure functions, with these tests in
  `crates/runner/tests/connection.rs`:
  `an_existing_board_is_solo_and_never_sees_the_chooser`,
  `no_board_and_no_identity_is_unchosen`,
  `a_server_url_is_connected_whether_or_not_a_board_exists`, and
  `a_missing_desktop_token_is_signed_out`.
- These tests pass in `crates/runner/tests/connection.rs`. They run against the real
  `rimaia-server` router on `127.0.0.1:0` with a temporary board, 047's
  `FakeIdentityProvider`, a `TestClock`, and `MemoryStore`. No test sleeps. The closure
  that plays the browser spawns a task that, with `reqwest` following no redirects:
  `GET`s the opened URL; reads the `Location` (the fake's `https://idp.test/authorize?…`,
  which it does not follow) and the `rimaia_signin` cookie; fills the fake's code table;
  `GET`s `<origin>/auth/github/callback?code=…&state=…` with `Cookie: rimaia_signin=…`;
  and `GET`s the loopback `Location` that answers.
  - `a_desktop_sign_in_opens_exactly_this_url`: with an injected state, verifier and host
    label, the opened URL is byte-for-byte Scope 3's, with the listener's port in
    `redirect_uri`.
  - `the_desktop_signs_in_pairs_its_runner_and_records_the_server`. The keychain holds an
    `rmd_` token under `DesktopToken { runner_id }` and the runner token under 058's key.
    `runner_identity` holds the paired runner id and the normalised origin.
    `list_api_tokens` shows exactly one desktop token and one runner token, both labelled
    with the host label.
  - `a_callback_with_the_wrong_state_ends_the_sign_in_and_stores_nothing`.
  - `a_sign_in_nobody_finishes_expires_after_ten_minutes`, driven by advancing the
    `TestClock`.
  - `a_cancelled_sign_in_stores_nothing_and_closes_its_listener`.
  - `the_callback_listener_binds_loopback_and_answers_only_the_callback`.
  - `a_server_address_that_is_not_https_is_refused_before_anything_opens`, with the exact
    sentence from Scope 3. The `http://` forms of `127.0.0.1`, `localhost` and `[::1]` are
    accepted.
  - `a_refused_protocol_version_asks_for_an_update`, with the exact sentence.
  - `a_failure_after_the_exchange_leaves_the_mode_unchanged_and_the_keychain_empty`, and
    `list_api_tokens` no longer shows the desktop or runner token it minted.
  - `a_connected_desktop_serves_one_run_route_on_its_loopback_port`: with
    `serve_run_proxy: false`, `RunHandles::endpoint()` is `mcp::build`'s bound address and
    the host bound no listener of its own.
  - `a_connected_launch_keeps_a_stopped_queue_stopped`: with `start_queue: false`,
    `queue_state` is `paused` and the loop claims nothing.
  - `a_revoked_runner_token_shows_signed_out`: revoking the `rmr_` token resolves
    `ended()` with `Unauthenticated`, and `desktop_state` answers `signed_out`.
  - `the_desktops_own_run_now_is_an_interactive_run`: `run_here` reaches the server as
    `claim(Run { trigger: Manual })` over the runner's own token.
  - `switching_is_refused_while_this_runner_holds_a_lease`, with the exact sentence.
  - `disconnecting_revokes_this_machines_tokens_and_forgets_the_server`: afterwards the
    server's `Authenticate` refuses all three tokens, and the runner row has `unpaired_at`.
  - `disconnecting_from_an_unreachable_server_still_disconnects_and_names_what_is_left`.
  - `disconnect_then_relaunch_is_solo_with_the_solo_runner_identity`, once for a machine
    that was solo before connecting and once for one that was `unchosen`: the relaunch's
    `ensure_solo` and `adopt_board` succeed, and `runner_identity` is
    `(solo_identity.runner_id, NULL)`.
  - `disconnecting_keeps_this_machines_runner_settings_and_checkouts`, through that
    relaunch's adoption: a connected-era `runner_settings` value survives.
  - `signing_in_again_keeps_a_runner_the_server_still_knows`, and
    `signing_in_again_pairs_again_when_the_server_does_not`.
  - `a_token_revoked_on_the_account_page_locks_the_gate_at_the_next_launch`, and
    `an_unreachable_server_at_launch_keeps_the_stored_token`.
  - `the_pairing_choices_are_recorded_with_the_server`: after a connect with
    `summaries_only` and `strict_local`, both accessors answer those values, and they were
    written in the transaction that wrote `runner_identity`.
  - `a_failed_connect_leaves_this_machines_run_environment_alone`: a solo machine set to
    `inherit` that fails after the exchange is still `inherit`, and `upload_transcripts` is
    still absent.
  - `a_first_connect_without_both_choices_is_invalid`, before a listener is bound.
  - `signing_in_again_without_choices_keeps_the_stored_ones`.
  - `get_transcript_upload_carries_the_disclosure_verbatim`: `disclosure` equals
    `transcripts::UPLOAD_DISCLOSURE` byte for byte, on an `unchosen` store as well as a
    connected one.
- These tests pass in `crates/runner/tests/host_secrets.rs`, over `MemoryStore` and the
  recorded fixture streams, with no test sleeping:
  - `host_secrets_holds_all_three_rimaia_tokens_when_connected`: with the runner, desktop
    and loopback items present, the redactor replaces each value, and in solo it is still
    empty (056's case passes unchanged);
  - `a_connected_desktops_run_writes_none_of_its_tokens`: a fixture run whose stream and
    stderr contain all three values writes a transcript, a stderr log and tail messages
    that contain none of them;
  - `a_token_minted_while_the_host_runs_is_redacted_from_later_runs`: after
    `add_host_secret`, the next spawned run redacts it, with no restart;
  - `minting_is_refused_while_a_run_is_in_flight`, with Scope 4's exact sentence, and a
    call that finds a token already stored still updates registrations.
- These tests pass in `crates/runner/tests/connected_reads.rs`. Each runs the converted
  core function twice over one fixture board: once through `InProcessCommands` (and
  `InProcessBoard`) on a temporary solo board, and once through `HttpCommands` (and
  `HttpBoard`) against the real `rimaia-server` router on `127.0.0.1:0` holding the same
  rows, signed in with a desktop token. The two answers are equal unless the case says
  otherwise.
  - `worktree_status_and_diff_measure_from_the_recorded_base_in_both_modes`, with a
    dependency chain whose head moved after the run, so fresh resolution would have
    disagreed;
  - `a_worktree_with_no_run_measures_against_the_default_branch`;
  - `cleanup_refuses_a_task_another_runner_is_running`: the task is `running` on the
    server for a second runner, and removal here is refused with D20's sentence;
  - `cleanup_refuses_when_the_board_cannot_be_reached`, connected only;
  - `cleanup_done_selects_from_the_boards_columns`;
  - `the_worktree_inventory_names_its_tasks_from_the_board`;
  - `the_prompt_preview_is_the_claims_context`: the connected preview is byte-for-byte
    the prompt a claim of the same task composes;
  - `prune_reads_run_kinds_from_the_board`, for a run with no `transcript_uploads` row;
  - `a_connected_preflight_preview_says_the_server_chooses`, with Scope 11's exact
    sentence;
  - and one case named for each further read that 041's or 066's PR lists.

  044's tests of the live fallback's fresh resolution and its warning are rewritten to the
  recorded base, and the PR names every one. No other `tests/worktree.rs` assertion is
  edited.
- `src-tauri/src/commands/` contains no `state.board`. `./scripts/check-command-wiring.sh`
  checks it, and the PR shows the script failing once on a scratch handler that adds one.
- These tests pass in `crates/core/tests/mcp_loopback.rs`:
  - `a_connected_loopback_refuses_a_request_without_the_token`: `401`, `WWW-Authenticate:
    Bearer`, and the exact body;
  - `a_connected_loopback_refuses_a_wrong_or_different_personal_token`;
  - `a_connected_loopback_with_no_token_issued_refuses_everything`;
  - `issuing_a_token_unlocks_the_gate_without_a_restart`;
  - `a_connected_loopback_never_gates_the_run_route`;
  - `a_connected_loopback_lists_local_tools_and_refuses_board_tools_naming_the_server`,
    with Scope 7's exact sentence;
  - `mcp_status_says_when_a_token_is_required`.
- **Solo is unaffected.** Every existing test in `crates/core/tests/mcp*.rs` passes with no
  change except the added `LoopbackGate` (`Open`) and `BoardTools::Local` arguments at each
  `mcp::build` call. No other assertion is edited.
- These tests pass in `crates/core/tests/mcp_registration.rs`, over configuration files in
  a `TempDir` home. `testing::cli`'s `FakeCli` gains a mode that records argv and working
  directory for a call with no task id, and the file is `#![cfg(unix)]` for
  `provider_process.rs`'s reason: the stand-in is a POSIX script.
  - `a_user_scope_registration_is_removed_and_re_added_with_the_header`, with the exact
    argv of both calls, the URL carrying the bound port;
  - `a_local_scope_registration_is_updated_from_its_project_directory`;
  - `a_registration_under_another_name_is_found_by_its_url`;
  - `a_project_scope_registration_is_never_written_and_says_why`, with the exact sentence;
  - `a_hosted_registration_is_never_rewritten` and
    `a_stdio_bridge_is_reported_not_rewritten`, each with its exact sentence;
  - `no_registration_is_added_at_user_scope_as_rimaia`;
  - `a_failed_cli_call_reports_the_clis_words_with_the_token_redacted`;
  - `the_minted_token_appears_only_in_the_report_that_minted_it`: a second call's report
    has `token: null`;
  - `the_registered_url_is_one_the_run_denial_recognises`: `OwnEndpoints::is_own` holds
    for the exact URL written, so D30 point 6's denial still covers it.
- `crates/core/tests/fixtures/cli/mcp-registration/` holds the three recorded CLI facts and
  a `README.md` naming the pinned version, and a replay test is named for each fact.
- No token, verifier, state, code or token hash appears in any `tracing` field, `Debug`
  output or error message in this task, nor in any answer except Scope 9's two.
  `no_connection_type_prints_a_secret_in_debug` covers every new type that holds one.
- The nine commands in Scope 9 are `local` registry rows, with one `generate_handler!`
  entry each and a `local<T>` wrapper each. `./scripts/check-command-wiring.sh` passes, and
  none of the nine has an MCP tool.
- These vitest cases pass, and the 31 files that mock `@tauri-apps/api/core` are edited only
  where a new command or a changed `McpStatus` requires it:
  - `client.test.ts`: `it("renders the chooser when no mode has been chosen")`,
    `it("installs HTTP transports with the desktop token when connected")` (replacing 049's
    loud-failure case), `it("offers to sign in again when the desktop is signed out")`,
    `it("switches to sign in again when the server answers unauthenticated")`, and
    `it("renders the account page and the team switcher when connected")`, with two teams;
  - `commands.test.ts`: `it("sends this desktop's own Run now to run_here and another
    runner's to the board")`;
  - `ModeChooserView.test.tsx`: solo in one click followed by `restart_app`; a pending
    sign-in that can be cancelled; the server's refusal shown in `ErrorBanner`;
  - `ConnectionSection.test.tsx`: both modes, and the disabled state with its reason;
  - `McpSection.test.tsx`: the add line with the bearer header when `requiresToken`, the
    registration offer, and Update Claude Code listing each outcome.
  - `ConnectForm.test.tsx`: the disclosure rendered verbatim from `get_transcript_upload`;
    *Upload full transcripts* and *Strict / local* selected, with the recommendation's
    exact sentence; `connect_to_server` called with both choices as chosen; and Sign in
    again rendering neither choice and sending neither field;
  - `ConnectionSection.test.tsx` also covers the transcripts control: rendered when
    connected, calling `set_transcript_upload`, and absent in solo;
  - `InstructionsSection.test.tsx` passes unchanged after the run environment options
    move to `src/lib/runEnvironment.ts`.
- 028's fixture coverage test passes, with rows for the nine new commands. 028 gains two
  scenarios: `first-launch`, which renders `ModeChooserView`, and `connected`, which
  renders the connected `ConnectionSection` and the registration offer. `npm run
  screenshot` captures both, and the implementer inspects the images.
- D35 describes what landed, or carries a dated amendment for each point where the code
  differs. `docs/seam-contract.md`'s "How to use this" has 059's row.
- CLAUDE.md has Scope 12's bullet. The runner `.sqlx` cache is regenerated, and the board
  cache is unchanged.
- Every CI check passes: `npm run typecheck`, `npm run test`, `npm run build`,
  `cargo test -p rimaia-core`, the runner and server crates' test and clippy steps as
  040 and 046 added them, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`.
- **Needs a person, carried as a checklist in the PR body.** These need a GitHub OAuth app
  and a local `rimaia-server`:
  - a first launch connects through the real GitHub sign-in, and `rimaia.db` is not
    created;
  - `claude mcp list` shows the updated registration as connected, and an unregistered
    `curl` to `/mcp` gets `401`;
  - Run now on this window runs with `acceptEdits`;
  - Disconnect, then relaunch, and the old solo board is back as it was.

## Notes

**Seam entries to read:** D3 (the typed `runner_settings` accessor), D7 (`events.ts`, for
`connection_changed`), D8 (no new `ErrorCode`; `unauthenticated` and `upgrade_required`
already exist), D10, D11, D15 (a launch starts paused; quitting runs the exit path), D16
(point 7, the bind status), D19 (`InFlight`, read by the idle check and the mint refusal),
D20 point 1 (guard 1, which Scope 11 re-routes and keeps without an override), D20.6's
2026-09-04 amendment and D25 point 6 (tools that are deliberately absent), D27
(`ProviderId::as_str()`), D28 part 6 (`runner_identity`), its "The runner set" (who may
write that row) and its Why (sign-in state stays in memory), D29 (Scope 11's two `runs`
reads say which kinds they mean), D30 points 1 and 6 (`rimaia` stays the operator name;
`OwnEndpoints`; the loopback token as the control that holds), D31 points 2, 4, 8 and 10
(boxed futures; `preview`, which gains a reader), D32 points 3, 6, 7 and 8 and its
2026-10-04 amendment on local handlers (`AppState.board` is `None`; `local<T>`; `Door`;
the reads this task converts), D33 (the runner cache), D34 (`reqwest`'s `rustls` feature,
approved for `src-tauri` in this task; add it there only if the shell itself builds a
`reqwest::Client`, and say in the PR which it was), and **D35, this task's own entry**. D4
and D6 apply as prohibitions.

**Read first, from the tasks before this one:** 056's Scope 6 and 7 (`host_secrets`, the
two key variants it added, `upload_transcripts`, `UPLOAD_DISCLOSURE`) and its "What the
next tasks expect"; 058's pairing questions (Scope 4), whose defaults the desktop matches;
044's hand-off on `status` and `diff_summary`; and the board-read lists in 041's and 066's
PR bodies, which are Scope 11's inventory.

**No migration.** The mode is derived from `runner_identity` and whether the board file
exists. The only new stored value is one `runner_settings` key. If the design seems to need
a column, stop: D28's amendment says that is a stop-and-ask.

**Files to start from:**
- `src-tauri/src/lib.rs` (`setup()`, the `mcp::build` call, and the solo `scheduler::build`
  call that connected mode replaces with `RunnerHost::start`); `src-tauri/src/state.rs`;
  `src-tauri/src/commands/app.rs`, which already uses `tauri_plugin_opener::OpenerExt`;
  `src-tauri/src/commands/mcp.rs`.
- `crates/core/src/mcp/mod.rs` (`build`, the router, `MCP_SERVER_NAME`, `probe`),
  `crates/core/src/mcp/scope.rs`, `crates/core/src/credentials/mod.rs`
  (`CredentialStore`), `crates/core/src/clock.rs` (`Clock::sleep_until`), and
  `crates/core/src/testing/cli.rs` (the fake CLI).
- `src/App.tsx`, `src/main.tsx`, `src/views/WelcomeView.tsx`,
  `src/views/settings/McpSection.tsx`, `src/components/McpAddCommand.tsx`, and
  `src/lib/commands.ts`.
- Created by earlier tasks on this branch: `crates/runner/` (040, 052, 058, and
  `crates/runner/src/host/`), `crates/core/src/identity/` (047), `crates/core/src/api/`
  (046), `src/lib/client.ts` (049), `src/views/SignInView.tsx`, `AccountView.tsx` and
  `TeamSwitcher.tsx` (050), and 055's Claude configuration reader.

**What the chain provides.**
- 047: `/auth/desktop` with its optional `label`, `POST /api/v1/auth/desktop_token`,
  `pair_own_runner` (only for `Door::Desktop`), `create_personal_access_token`,
  `list_api_tokens`, `revoke_api_token`, `identity::secret`, and `FakeIdentityProvider`.
- 049: `installTransports("connected", { serverUrl, token })`, `get_client_capabilities`
  and `subscribeToEventStreamFailure`.
- 050: CORS for the Tauri origins with the `Authorization` header; `SignInView`, the
  account page and the switcher, with the return to sign-in on `unauthenticated`.
- 052 and 053: `HttpBoard`, the interactive `claim(Run { trigger: Manual })` over a
  runner's own token, heartbeats, and leases across the network.
- 055: the Claude configuration reader, `OwnEndpoints`, and the run route `mcp::build`
  mounts.
- 041 and 066: the named core read functions their local handlers call, listed in their
  PRs. 044: the `status`/`diff_summary` fallback read it left for this task.
- 046: `api::dispatch`, `BoardHost` and the solo `Caller` that `InProcessCommands` wraps.
- 056: `secrets::host_secrets` over `CredentialKey::RunnerToken` and `DesktopToken`,
  `Redactor::merged`, `OutboxBoard` (which `RunnerHost::start` wraps around `HttpBoard`),
  `upload_transcripts` with `get_transcript_upload` and `set_transcript_upload`, and
  `transcripts::UPLOAD_DISCLOSURE`.
- 057: unpairing releases pins.
- 058: `RunnerHost`, `HostConfig`, `HostEnd`, the `RunnerConfig` the host builds, the
  hostname function, the keychain key convention, `runner.lock`, `pair`'s two questions
  and their defaults, and the runner-only queue view without a plan half.

**What the next tasks expect.**
- **060:** the loopback endpoint behind its gate, with `BoardTools::Remote` refusing board
  tools. Its first commit builds Scope 7's relay with the tests named there, and adds an
  end-to-end case through a connected desktop's loopback. Its person checklist gains: a
  planning session creates a task through the loopback, and the task's `source` is `mcp`.
- **061:** `get_connection`'s `runnerLabel`, `localRunnerId`, and `run_here` as the target
  its runner picker leaves for this machine.
- **069:** `This machine's limits` does not carry `upload_transcripts`. Its control is in
  Settings → Connection (Scope 10).
- **064:** the final pass over CLAUDE.md's connected-mode lines.

**Size.** Estimated at 4,300–4,900 changed lines, with the relay already moved to 060:
- sign-in, pairing, switching and their tests: about 1,400;
- the pairing choices and host secrets, with tests: 350;
- the host composition, `run_here` and the gate, with tests: 700;
- registration and fixtures: 650;
- `BoardCommands`, the converted reads and `connected_reads.rs`: 700, more if 041's and
  066's lists are longer than the table;
- the shell's setup branches and the nine commands: 450;
- the frontend and its tests: 1,000.

**This is past one session's comfortable size, and the cut is named in advance.** If the
work does not fit, move Scope 8 (Claude Code's registration, its fixtures and
`mcp_registration.rs`, about 650 lines) into a new task numbered after the highest
existing one and ordered directly after this one in `tasks/README.md`. It is
self-contained, and the minting command and its refusal move with it. Until it lands, a
connected desktop's gate is `Locked`, Scope 6's state after **Not now**, so the loopback
refuses everything and opens nothing. That is safe and visibly incomplete, which is the
right way round. Say so in the PR. Nothing else in this task is cut. Scope 11 cannot
wait, because a connected desktop with unconverted reads panics, and the pairing choices
are what ADR-0036 point 5 and ADR-0032 point 6 require of pairing.

**Never cut** the gate, the keychain storage, or the "record the server last" ordering.
Without the gate, connecting silently opens every team to anything on loopback. Without
the ordering, a failed sign-in can leave a machine that believes it is connected with no
token to prove it.
