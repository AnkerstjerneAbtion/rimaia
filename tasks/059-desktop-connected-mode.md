---
id: "059"
title: Desktop connected mode
milestone: v0.5
status: ready
depends_on: ["058"]
adrs: ["0030", "0035", "0027", "0034"]
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
  token and 047's `pair_own_runner`. Its runner then claims, runs and reports through 052's
  `HttpBoard`, exactly as 058's headless runner does. There is one connected runner, hosted
  twice.
- **The frontend talks to the server.** Board commands go over HTTP with the desktop token
  and board events arrive over SSE, through the transports 049 built and left waiting for
  this task (ADR-0034 point 4). `AppState.board` is `None`, and `rimaia.db` is not opened
  (D32 point 3).
- **The loopback operator endpoint stays, behind a personal access token**
  ([ADR-0035](../docs/adr/0035-mcp-when-the-board-is-remote.md) point 4, ADR-0030 point 6).
  Local tools are answered on this machine. Every other tool call is relayed to the server
  as the token's user. The app offers to update Claude Code's existing registration with
  the token, and that costs the user one confirmation.
- **Solo is unaffected.** A solo launch opens no socket to anything off the machine, and the
  loopback endpoint is as open as ADR-0006 left it.

## Why now

058 made the connected runner real without a window. 047 left the server half of the
desktop's sign-in waiting (`/auth/desktop`, `/api/v1/auth/desktop_token`,
`pair_own_runner`), and 049 left its `connected` branch failing loudly, naming this task.
Both are half-built seams until one task joins them. The desktop is also how most people
join a team: they already have the app and a solo board, and the step from there to a
shared board should be one sign-in, not a second install.

The loopback endpoint cannot wait for later. The moment the app connects, anything that can
reach `127.0.0.1:<mcp_port>` reaches every team the user belongs to, not one person's local
board (ADR-0035 point 4). ADR-0030 point 6 makes the token part of connecting. It is not
hardening that can come afterwards. D30 point 6 relies on it too: against a registration
the run-denial resolver cannot see, the token is the control that holds.

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
still does not depend on the runner.

**2. Setup, once per mode.** `src-tauri/src/lib.rs`'s `setup()` branches on
`resolve_mode`, and each branch builds everything once:

- **`solo`:** today's path, unchanged. The board opens in process, `AppState.board` is
  `Some`, the board port is 036's `InProcessBoard`, and the loopback endpoint is open.
- **`connected`:** `rimaia.db` is neither opened nor created. The board port is 052's
  `HttpBoard`, built from `runner_identity` and the runner token by the same function
  058's binary calls. Do not build a second connected runner. Local handlers that need
  board state reach it through the board port, or by dispatching a board command over HTTP
  with the desktop token (D32 point 8). Every `AppState` field that held a board
  `ServiceContext` is `None` here, or moves under `board`, and the compiler finds the
  readers. The loopback `/mcp` is gated (Scope 6).
- **`unchosen`:** `runner.db` is opened and migrated. Nothing else is: no board, no
  scheduler, no MCP listener. The window opens on the chooser (Scope 10).

**Changing mode restarts the app.** The choice is written first. Then a local command,
`restart_app`, calls `AppHandle::restart()`. That is the relaunch D34 already names in
place of `tauri-plugin-process`, and 063 reuses it. `setup()` builds the scheduler, the
MCP listener, the board port and `AppState` once. A mode is a property of all four. A
restart keeps one construction path per mode, instead of adding a teardown path that
nothing else uses.

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
- **The URL the browser opens** is `<origin>/auth/desktop` with `redirect_uri`, `state`,
  `code_challenge` and `code_challenge_method=S256`, plus a `label` if 047's route takes
  one, built with `reqwest::Url::parse_with_params`. The verifier and the state come from
  047's `mint_verifier` and `pkce_challenge`. Opening the browser is an injected
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
  token. The label is the machine's host label: use 058's default-label function if it has
  one, otherwise add one that reads `COMPUTERNAME` on Windows and runs `hostname` as an
  argument vector elsewhere, and falls back to D28 part 3's `This computer`. No crate is
  added (D34). `provider` is the configured provider's `ProviderId::as_str()`.
- **Store the tokens in the keychain** (ADR-0030 point 4), through ADR-0020's
  `CredentialStore`, under accounts keyed by the paired runner's id:
  `rimaia-desktop:<runner_id>` for the desktop token and
  `rimaia-loopback-mcp:<runner_id>` for the personal access token from Scope 8. The runner
  token goes wherever 058 keeps it, through 058's accessor. Keying by runner id is what
  stops two development data directories (ADR-0023) from sharing one keychain item. If
  `CredentialStore`'s signature after 054 cannot take a non-repository account, add a
  sibling method. Do not encode a fake repository id.
- **Record the server last.** Write `runner_identity` (`runner_id`, `server_url` = the
  origin) in one runner-store transaction, *after* both tokens are stored. A sign-in that
  fails anywhere before this step leaves the machine in the mode it was in, and deletes
  any keychain item it already wrote.
- **Nothing secret reaches a log line.** No log line, `Debug` output, error message or
  returned DTO carries a token, code, verifier or state. The only exception is
  `get_server_connection`'s answer (Scope 9), which is the desktop token that 049's
  transport is built to carry.

**5. Switching later.** Settings → Connection offers "Connect to a team server" in solo and
"Disconnect" when connected.

- **Both are refused unless the runner is idle.** The queue must be stopped, `InFlight`
  empty, and `held_leases` empty. The refusal is `invalid`, with exactly:
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
  2. Delete the three keychain items.
  3. Delete the `runner_identity` row.
  4. Create and migrate `rimaia.db` if it does not exist.
  5. Restart. The next launch resolves to `solo`, and its solo path writes
     `runner_identity` from `solo_identity`.

  If the server cannot be reached, steps 2–5 still happen. The result then names each
  credential that is still listed on the server's account page, so the user can revoke it
  there.
- **Signing in again.** A connected launch can find the desktop token missing from the
  keychain, or a locked keychain, or the server can answer `unauthenticated` to a board
  command. In each case the window shows "Signed out of <origin>" with **Sign in again**.
  It never falls back to solo, for D32 point 3's reason: a transport bug should fail on
  the first click, not write to a board nobody reads. Signing in again replaces the desktop
  token. It pairs again only when `list_api_tokens` no longer shows a runner token for
  `runner_identity.runner_id`.

**6. The loopback endpoint, gated.** In `crates/core/src/mcp/mod.rs`, `mcp::build` gains a
`LoopbackAuth` argument: `Open` in solo, and `Token { hash }` when connected.

- **Only the operator route is gated.** When the gate is `Token`, `/mcp` requires
  `Authorization: Bearer rmp_…`. The presented token's `identity::secret::hash` must equal
  the stored one, compared in constant time. Anything else gets `401` with
  `WWW-Authenticate: Bearer` and D8's body:
  `{"code":"unauthenticated","message":"This Rimaia is connected to a team server. Its MCP
  endpoint needs the token from Settings → MCP."}`. `/mcp/run/{token}` is never gated by
  this: it has its own token and its own table (D30, 055).
- **The bind does not change.** It is still the `127.0.0.1` literal and the configured
  port, and a busy port is still a status, not a startup failure (D16 point 7).
- **`McpStatus` gains `requiresToken: bool`**, on both sides, so the add line and Test
  connection can say what the endpoint expects. In connected mode, Test connection sends
  the keychain's loopback token.
- **A refused token locks the gate.** When the server refuses the loopback token (Scope 7),
  the gate forgets the hash, deletes the keychain item, and refuses every request until
  Settings → MCP issues a new token. At every connected launch the app also checks that
  the stored token's id is still in `list_api_tokens`. A token revoked from the account
  page therefore stops working locally at the next launch at the latest, and at the next
  relayed call at the earliest.

**7. The relay: local tools here, board tools on the server.** When connected, the loopback
server does not serve 041's board router, because there is no board to serve it from. It
serves 041's local router, and relays everything else:

- **`tools/list`** answers the server's board tools followed by this machine's local
  tools. The board tools are fetched from `<origin>/mcp` on each call. The transport is
  stateless (ADR-0006), so there is nothing to keep in step.
- **`tools/call`** for a name in the local router is answered here, and never forwarded.
  Those are ADR-0035 point 6's machine tools. They include `plan_task_strategy` and
  `plan_tasks_strategy`, which start the planner locally over the `HttpBoard` port. Any
  other name is forwarded to `<origin>/mcp` with `Authorization: Bearer` carrying the
  loopback token. That is ADR-0035 point 4's "as its signed-in user": the server resolves
  the same user, applies the token's team restriction, and records `Door::Mcp`, so a plan
  created from a Claude Code session keeps `tasks.source = mcp` (ADR-0019).
- **Why the hosted MCP endpoint and not the board commands.** The server's `/mcp` is
  where ADR-0035 point 2's `team` argument, its refusal when the team is ambiguous, and
  `list_teams` are implemented, once. Relaying through `/api/v1` with the desktop token
  would need a second copy of those rules in the relay, and it would record the plan's
  source as `ui`.
- **Mechanism.** Core defines `trait McpUpstream` (list and call, both `BoxFuture`) in
  `crates/core/src/mcp/relay.rs`, and a `BoardTools::{Local(ServiceContext),
  Relay(Arc<dyn McpUpstream>)}` argument to `mcp::build`. `rimaia-runner` implements it
  in `connection/mcp_upstream.rs` over rmcp 3.1.4's streamable-HTTP client, whose
  `auth_header` carries the token. The implementation is in the runner crate because
  that crate already enables reqwest's TLS (052), and core's reqwest stays plain HTTP to
  loopback (D34).
- **The upstream's failures become tool errors, with exact wording:**
  - `401`: `Rimaia's server at <origin> refused this machine's MCP token. Issue a new one in
    Rimaia → Settings → MCP.` This also locks the gate (Scope 6).
  - `404` (a server without 060): `Rimaia's server at <origin> does not serve MCP yet.`
  - Connection failure: `Could not reach Rimaia's server at <origin>: <error>.`
- **The server name stays `rimaia`.** `get_info` still reports `MCP_SERVER_NAME`, and
  nothing registers `rimaia-run` in a user's configuration (D30 point 1).

**8. One step to update Claude Code's registration.** New:
`crates/core/src/mcp/registration.rs`. It lives in core because it spawns a local program
and makes no network call.

- **Minting.** `update_claude_code_registration` (Scope 9) calls 047's
  `create_personal_access_token` with the desktop token, `{ label: "Claude Code on
  <host label>" }`, and no team restriction. It does this only when no loopback token is
  stored. The secret goes into the keychain (Scope 4), and its id goes into one new
  `runner_settings` key, `loopback_mcp_token_id`, with a typed accessor (D3). No migration
  is needed.
- **Finding the registrations.** Registrations are found by URL, not by name. Use 055's
  reader of Claude Code's configuration, the function behind
  `ClaudeProvider::inherited_mcp_servers` (D30 point 6). Call it directly, not through
  whichever provider runs tasks: this is Claude Code as an MCP client of Rimaia, which
  `src/components/McpAddCommand.tsx`'s comment already distinguishes from the agent CLI.
  Every registration for which `OwnEndpoints::is_own` holds, whatever it is named, is a
  candidate.
- **Updating each one** takes two argument vectors, with no shell (CLAUDE.md):
  `claude mcp remove <name> --scope <scope>`, then `claude mcp add --transport http --scope
  <scope> <name> http://127.0.0.1:<bound port>/mcp --header "Authorization: Bearer <token>"`.
  - **User scope** runs from the home directory.
  - **Local scope** (`projects.<path>.mcpServers`) runs with `current_dir` set to that
    project path.
  - **Project scope** (`<repo>/.mcp.json`) and **managed** registrations are never written.
    A token in `.mcp.json` would be committed to the repository, and a managed file belongs
    to an administrator. Each is reported as not updated, with exactly: `Not updated:
    <path> is shared through the repository, and a token written there would be committed.
    Register Rimaia at user scope instead.` (and the managed variant naming the file).
  - **No registration at all** means one is added at user scope, under `rimaia`.
- **The token on argv is accepted, and the reason is recorded in a comment.** Every
  process that can read another process's argv on this machine runs as the same user, and
  can read `~/.claude.json`, which is where Claude Code stores the header anyway. Editing
  `~/.claude.json` directly is refused: Claude Code rewrites that file while it runs, and
  two writers would race.
- **The report**, `RegistrationReport`, gives each candidate's name, scope and outcome
  (`updated`, `added`, `not_updated` with the sentence, or `failed` with the CLI's own
  stderr). The token is redacted from stderr before it goes into the report, with the
  exact-value redaction from D25.
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
tool. Two of them carry a secret, which is D25 point 6's exception. The rest ask a person
at this machine to open a browser or decide where the machine reports, which is the
desktop-referent exception D20.6's 2026-09-04 amendment names. A run able to reconfigure
which server its runner answers to is the laundering path ADR-0032 point 6 exists to
close.

| Command | Answers |
| --- | --- |
| `get_connection` | `DesktopConnection { state: "unchosen" \| "solo" \| "connected" \| "signed_out", serverUrl, login, runnerLabel, loopbackTokenIssued }`. Never a secret |
| `get_server_connection` | `{ serverUrl, token }` for 049's `installTransports("connected", …)`. `invalid` unless connected |
| `choose_solo` | Creates and migrates `rimaia.db`, then answers. The frontend calls `restart_app` |
| `connect_to_server` | `{ serverUrl }` → `{ login, runnerLabel }` once Scopes 3–4 complete |
| `cancel_connect` | Ends a pending sign-in. Answers `null` whether or not one was pending |
| `disconnect_from_server` | Scope 5. Answers the list of credentials still on the server, which may be empty |
| `update_claude_code_registration` | Scope 8's `RegistrationReport` |
| `restart_app` | `AppHandle::restart()` |

`get_client_capabilities` is unchanged. It already answers `connected` once
`runner_identity` has a `server_url` (049). 028's fixture table gains a row for each new
command, answering the solo shape.

**10. The frontend.**

- **`src/lib/client.ts`'s `bootstrapClient()`** asks `get_connection` first. `unchosen`
  renders the chooser. `connected` calls `getServerConnection()` and
  `installTransports("connected", { serverUrl, token })`, replacing 049's loud failure.
  `signed_out` renders the sign-in-again view. `solo` is today's path.
- **`src/views/ModeChooserView.tsx`** shows two choices, in ADR-0024's calm register:
  - *This computer only*: the board stays in a file here, with no account, and nothing is
    sent anywhere.
  - *A team server*: an address field and **Sign in with your browser**.

  While the browser is open, the view says so and offers **Cancel**. Refusals render in
  `ErrorBanner`. On success it shows Scope 8's offer, *Claude Code's registration of
  Rimaia needs a token now*, with **Update** and **Not now**, then the report, then
  restarts.
- **Settings → Connection** (`src/views/settings/ConnectionSection.tsx`) shows the mode,
  the server, the signed-in login and this runner's label, with Connect or Disconnect.
  Both are disabled while the runner is not idle, and the reason is shown beside them.
- **Settings → MCP and the Welcome flow's last step** render `McpAddCommand` with
  `--header "Authorization: Bearer rmp_…"` when `requiresToken`. They also show **Update
  Claude Code** there. The full token is shown once, in the report of the call that minted
  it, and never again (ADR-0030 point 3).
- The Welcome flow is otherwise unchanged. It renders after either choice, through
  whichever transports are installed.

**11. Documentation.** CLAUDE.md's Gotchas gains one bullet. A connected desktop's loopback
`/mcp` requires the personal access token that Settings → MCP issues, and
`/mcp/run/{token}` does not. To develop connected mode, run a local `rimaia-server` and the
app with its own `RIMAIA_DATA_DIR`. Keychain items are keyed by runner id so that two
data directories do not share them. CI gains no command. This task adds queries only to the
runner store, so the runner `.sqlx` cache is regenerated with D33's recipe, and the board
cache is not touched.

## Out of scope

- **The hosted `/mcp` itself**, and its `team` argument, `list_teams`, assignee and
  planning-as-a-request: 060. Until 060 lands, a relayed board tool answers Scope 7's
  "does not serve MCP yet" error. That is correct behaviour, not a gap to paper over with
  a direct `/api/v1` call.
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
  (ADR-0035 point 1).
- **Desktop notifications about runs on other machines.** The shell keeps notifying about
  this machine's runs from the runner's own events.
- **Assignment, consent, runners and pinned cards in the interface**: 061. **The signed
  updater**: 063, which reuses `restart_app`.
- **Browser-mode states** for local-only actions: 050.

## Acceptance criteria

- `resolve_mode` is a pure function over `(board_file_exists, runner_identity)`, and has
  these tests in `crates/runner/tests/connection.rs`:
  `an_existing_board_is_solo_and_never_sees_the_chooser`,
  `no_board_and_no_identity_is_unchosen`, and
  `a_server_url_is_connected_whether_or_not_a_board_exists`.
- These tests pass in `crates/runner/tests/connection.rs`. They run against the real
  `rimaia-server` router on `127.0.0.1:0` with a temporary board, 047's
  `FakeIdentityProvider`, a `TestClock`, and a test `CredentialStore`. A closure plays the
  browser by following the redirects with `reqwest`. No test sleeps.
  - `a_desktop_sign_in_opens_exactly_this_url`: with an injected state and verifier, the
    opened URL is byte-for-byte the expected `/auth/desktop` URL, with an S256 challenge
    and a `127.0.0.1` redirect.
  - `the_desktop_signs_in_pairs_its_runner_and_records_the_server`. The keychain holds an
    `rmd_` token under `rimaia-desktop:<runner_id>`. The runner token is where 058 keeps
    it. `runner_identity` holds the paired runner id and the normalised origin.
    `list_api_tokens` shows exactly one desktop token and one runner token, and the runner
    is labelled with the host label.
  - `a_callback_with_the_wrong_state_ends_the_sign_in_and_stores_nothing`.
  - `a_sign_in_nobody_finishes_expires_after_ten_minutes`, driven by advancing the
    `TestClock`.
  - `a_cancelled_sign_in_stores_nothing_and_closes_its_listener`.
  - `the_callback_listener_binds_loopback_and_answers_only_the_callback`.
  - `a_server_address_that_is_not_https_is_refused_before_anything_opens`, with the exact
    sentence from Scope 3. The `http://` forms of `127.0.0.1`, `localhost` and `[::1]` are
    accepted.
  - `a_refused_protocol_version_asks_for_an_update`, with the exact sentence.
  - `a_failure_after_the_exchange_leaves_the_mode_unchanged_and_the_keychain_empty`.
  - `switching_is_refused_while_this_runner_holds_a_lease`, with the exact sentence.
  - `disconnecting_revokes_this_machines_tokens_and_forgets_the_server`: afterwards the
    server's `Authenticate` refuses all three tokens, and the runner row has `unpaired_at`.
  - `disconnecting_from_an_unreachable_server_still_disconnects_and_names_what_is_left`.
  - `disconnecting_keeps_this_machines_runner_settings_and_checkouts`.
  - `signing_in_again_keeps_a_runner_the_server_still_knows`, and
    `signing_in_again_pairs_again_when_the_server_does_not`.
- These relay tests pass, with a real rmcp upstream on `127.0.0.1:0` that serves core's
  board router over a test context and records each request's `Authorization` header:
  - `tools_list_is_the_servers_board_tools_then_this_machines_local_tools`;
  - `board_tools_are_forwarded_with_the_loopback_token`;
  - `local_tools_are_answered_here_and_never_forwarded`: the upstream sees no call;
  - `a_refused_token_locks_the_loopback_until_a_new_one_is_issued`;
  - `an_unreachable_server_is_a_tool_error_naming_it`, and
    `a_server_without_mcp_is_a_tool_error_naming_it`, each with Scope 7's exact wording.
- These tests pass in `crates/core/tests/mcp_loopback.rs`:
  - `a_connected_loopback_refuses_a_request_without_the_token`: `401`, `WWW-Authenticate:
    Bearer`, and the exact body;
  - `a_connected_loopback_refuses_a_wrong_or_different_personal_token`;
  - `a_connected_loopback_never_gates_the_run_route`;
  - `mcp_status_says_when_a_token_is_required`.
- **Solo is unaffected.** Every existing test in `crates/core/tests/mcp*.rs` passes with no
  change except the added `LoopbackAuth::Open` and `BoardTools::Local` arguments at each
  `mcp::build` call. No other assertion is edited.
- These tests pass in `crates/core/tests/mcp_registration.rs`, using a fake `claude` from
  `testing::cli` that records argv and the working directory, over configuration files in
  a `TempDir` home:
  - `a_user_scope_registration_is_removed_and_re_added_with_the_header`, with the exact
    argv of both calls;
  - `a_local_scope_registration_is_updated_from_its_project_directory`;
  - `a_registration_under_another_name_is_found_by_its_url`;
  - `a_project_scope_registration_is_never_written_and_says_why`, with the exact sentence;
  - `no_registration_is_added_at_user_scope_as_rimaia`;
  - `a_failed_cli_call_reports_the_clis_words_with_the_token_redacted`;
  - `the_registered_url_is_one_the_run_denial_recognises`: `OwnEndpoints::is_own` holds
    for the exact URL written, so D30 point 6's denial still covers it.
- `crates/core/tests/fixtures/cli/mcp-registration/` holds the three recorded CLI facts and
  a `README.md` naming the pinned version, and a replay test is named for each fact.
- No token, verifier, state, code or token hash appears in any `tracing` field, `Debug`
  output, error message or report in this task. `no_connection_type_prints_a_secret_in_debug`
  covers every new type that holds one.
- The eight commands in Scope 9 are `local` registry rows, with one `generate_handler!`
  entry each and a `local<T>` wrapper each. `./scripts/check-command-wiring.sh` passes, and
  none of the eight has an MCP tool.
- These vitest cases pass, and the 31 files that mock `@tauri-apps/api/core` are edited only
  where a new command or a changed `McpStatus` requires it:
  - `client.test.ts`: `it("renders the chooser when no mode has been chosen")`,
    `it("installs HTTP transports with the desktop token when connected")` (replacing 049's
    loud-failure case), and `it("offers to sign in again when the desktop is signed out")`;
  - `ModeChooserView.test.tsx`: solo in one click followed by `restart_app`; a pending
    sign-in that can be cancelled; the server's refusal shown in `ErrorBanner`; the
    registration offer, and the report after it;
  - `ConnectionSection.test.tsx`: both modes, and the disabled state with its reason;
  - `McpSection.test.tsx`: the add line with the bearer header when `requiresToken`, and
    Update Claude Code listing each outcome.
- 028's fixture coverage test passes, with rows for the eight new commands.
  `npm run screenshot` renders the chooser and Settings → Connection. The implementer
  inspects both images.
- CLAUDE.md has Scope 11's bullet. The runner `.sqlx` cache is regenerated, and the board
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
  - after 060 lands, a planning session creates a task through the loopback, and the task's
    `source` is `mcp`;
  - Disconnect, then relaunch, and the old solo board is back as it was.

## Notes

**Seam entries to read:** D8 (no new `ErrorCode`; `unauthenticated` and `upgrade_required`
already exist), D10, D11, D16 (point 7, the bind status), D20.6's 2026-09-04 amendment and
D25 point 6 (tools that are deliberately absent), D28 part 6 (`runner_identity`; sign-in
state stays in memory), D30 points 1 and 6 (`rimaia` stays the operator name;
`OwnEndpoints`; the loopback token as the control that holds), D31 points 8 and 10
(`HttpBoard` is built in `setup()` when connected), D32 points 3, 6, 7 and 8
(`AppState.board` is `None`; `local<T>`; `Door`; local handlers reach the board over HTTP
with the desktop token), D33 (the runner cache), and D34 (`reqwest`'s `rustls` feature,
approved for `src-tauri` in this task; add it there only if the shell itself builds a
`reqwest::Client`, and say in the PR which it was). D4 and D6 apply as prohibitions.

**No migration.** The mode is derived from `runner_identity` and whether the board file
exists. The only new stored value is one `runner_settings` key. If the design seems to need
a column, stop: D28's amendment says that is a stop-and-ask.

**Files to start from:**
- `src-tauri/src/lib.rs` (`setup()`, and the `mcp::build` and `scheduler::build` calls);
  `src-tauri/src/state.rs`; `src-tauri/src/commands/app.rs`, which already uses
  `tauri_plugin_opener::OpenerExt`; `src-tauri/src/commands/mcp.rs`.
- `crates/core/src/mcp/mod.rs` (`build`, the router, `MCP_SERVER_NAME`, `probe`),
  `crates/core/src/mcp/scope.rs`, `crates/core/src/credentials/mod.rs`
  (`CredentialStore`), `crates/core/src/clock.rs` (`Clock::sleep_until`), and
  `crates/core/src/testing/cli.rs` (the fake CLI).
- `src/App.tsx`, `src/main.tsx`, `src/views/WelcomeView.tsx`,
  `src/views/settings/McpSection.tsx`, `src/components/McpAddCommand.tsx`, and
  `src/lib/commands.ts`.
- Created by earlier tasks on this branch: `crates/runner/` (040, 052, 058),
  `crates/core/src/identity/` (047), `crates/core/src/api/` (046), `src/lib/client.ts`
  (049), and 055's Claude configuration reader.

**What the chain provides.**
- 047: `/auth/desktop`, `POST /api/v1/auth/desktop_token`, `pair_own_runner` (only for
  `Door::Desktop`), `create_personal_access_token`, `list_api_tokens`, `revoke_api_token`,
  `identity::secret`, and `FakeIdentityProvider`.
- 049: `installTransports("connected", { serverUrl, token })`, and the
  `get_client_capabilities` rule.
- 050: CORS for the Tauri origins with the `Authorization` header.
- 052 and 053: `HttpBoard`, heartbeats, and leases across the network.
- 055: the Claude configuration reader, `OwnEndpoints`, and the run-scoped proxy on
  `/mcp/run/{token}`.
- 057: unpairing releases pins.
- 058: the headless runner loop, built from `runner_identity` and the runner token by one
  function, and wherever it keeps that token.

**What the next tasks expect.**
- **060:** the relay as tested here. It mounts `/mcp` on the server, and adds an end-to-end
  case through a connected desktop's loopback, which is when Scope 7's "does not serve MCP
  yet" stops being the answer.
- **061:** `get_connection`'s `runnerLabel`, and `localRunnerId`.
- **063:** `restart_app`.
- **064:** the final pass over CLAUDE.md's connected-mode lines.

**Size, and where to cut.** Estimated at 3,500–4,200 changed lines, which is near the
ceiling for one session:
- sign-in, pairing, switching and their tests: about 1,400;
- the gate and the relay, with tests: 900;
- registration and fixtures: 600;
- the shell's setup branches and the eight commands: 500;
- the frontend and its tests: 900.

If it runs over, cut the relay (Scope 7) into the first commit of 060. There is no hosted
`/mcp` to relay to before 060 anyway. 059 then ships the gate with every non-local tool
refused by the loopback itself, with `Rimaia's server at <origin> does not serve MCP yet.`
**Never cut** the gate, the keychain storage, or the "record the server last" ordering.
Without the gate, connecting silently opens every team to anything on loopback. Without
the ordering, a failed sign-in can leave a machine that believes it is connected with no
token to prove it.
