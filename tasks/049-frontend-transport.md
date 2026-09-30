---
id: "049"
title: A frontend transport per command kind
milestone: v0.5
status: ready
depends_on: ["048", "028"]
adrs: ["0034", "0024"]
size: L
---

# A frontend transport per command kind

## Goal

Give `src/lib/commands.ts` and `src/lib/events.ts` the transports
[ADR-0034](../docs/adr/0034-one-api-for-the-web-and-the-desktop.md) point 4 decides, chosen
per command kind and per mode, so the same React build can talk to the solo shell, to a
server and a local shell at once, or to a server alone:

| Mode | Board commands | Local commands | Events |
| --- | --- | --- | --- |
| Solo desktop | `invoke` | `invoke` | Tauri `listen` |
| Connected desktop | HTTP, bearer desktop token | `invoke` | SSE for board events, `listen` for local ones |
| Browser | HTTP, session cookie and CSRF header | refused, except `get_client_capabilities` | SSE |

Components keep calling the same functions. The two exceptions this task makes are the two
ADR-0034 names: the folder picker moves behind a local command, so no component imports a
Tauri plugin, and `get_client_capabilities` arrives, so the UI can later ask what this client
can do instead of guessing from the platform.

**This task builds the transports and proves them with a mocked `fetch`.** It does not put
the app in front of a real server. Serving the bundle, signing in and the browser states of
local-only actions are 050's. Entering connected mode is 059's.

## Why now

046 split `call<T>()` into `board<T>()` and `local<T>()` (seam-contract D32 point 6). Both
still call `invoke`, so the registry's classification exists in TypeScript but changes no
behaviour. 047 made sessions, CSRF and desktop tokens real, and 048 serves
`GET /api/v1/events`. The server can now answer every board command and stream every board
event. Nothing on the client can reach it.

Everything after this in the backlog needs it. 050 is the browser app, which is this
transport plus a sign-in page. 059 is the connected desktop, which is this transport plus a
token. 061's assignment and runner screens read `get_client_capabilities`. Doing the
transport inside any of them would mean designing it while also designing a page.

028 built the place this goes. `setCommandTransport` and `setEventTransport` exist, and the
fixture mode is the one alternative installed through them. 028's Notes say 049 "adds
implementations to it rather than replacing it", and this task holds to that.

## Scope

**1. Commands choose a transport by kind.** In `src/lib/commands.ts`:

- The installed transport becomes a pair, `{ board: CommandTransport, local:
  CommandTransport }`. `setCommandTransports(pair)` installs both. 028's
  `setCommandTransport(transport)` keeps its name and signature and installs the same
  transport for both kinds, which is what the fixture entry wants. The default, with nothing
  installed, is `invoke` for both, looked up at call time as 028 made it. **Every test file
  that mocks `@tauri-apps/api/core` must keep passing unedited**, except the two Scope 4
  names, and none of them runs a bootstrap.
- `board<T>()` sends through `board`, `local<T>()` through `local`, and both keep
  `toRimaiaError`. A wrapper never learns which transport answered it.
- **The HTTP transport** is a factory in `commands.ts`, the only module D32 point 6 allows to
  send a command over HTTP: `httpCommandTransport({ baseUrl, credential })`, where
  `credential` is `{ kind: "session", csrf: () => string | null }` or `{ kind: "bearer",
  token: string }`. For a command it sends `POST <baseUrl>/api/v1/<name>`, calling
  `globalThis.fetch` at call time so the test mock in Scope 6 is the one it reaches:
  - **Body:** `JSON.stringify(args ?? {})`. That is the object `invoke` receives as its
    second argument, byte for byte, with its `camelCase` keys (D32 point 3).
  - **Headers on every request:** `Content-Type: application/json` and `Rimaia-Protocol:
    <PROTOCOL_VERSION>`. The value is the literal 046 put in `commands.ts`, which the wiring
    script compares with `rimaia_core::api::PROTOCOL_VERSION`. There is no second literal
    anywhere. The credential's headers and these two are built by one exported function,
    `transportHeaders(credential)`, called afresh for every request. The SSE transport calls
    the same function at every open, so a header 050 adds there (the team) reaches both.
  - **Session mode:** `credentials: "same-origin"` and `X-Rimaia-CSRF: <csrf()>`, and no
    `Authorization` header. `csrf()` is called on every request, never cached, so a new
    sign-in is picked up without reinstalling anything. When it returns `null` the header is
    left out, and the server answers `unauthenticated`, which is the truth: there is no
    session. The session cookie is `HttpOnly` (ADR-0030 point 2), so the frontend never
    reads it. What `csrf` reads in the browser is Scope 5's.
  - **Bearer mode:** `Authorization: Bearer <token>` and `credentials: "omit"`, and no CSRF
    header. The server refuses a request carrying both (D32 point 7), so the transport never
    sends both.
  - **A `200`** is parsed as JSON. A unit answer is `null` (D32 point 3), which a
    `Promise<void>` wrapper resolves with. A `200` whose body does not parse, such as an
    HTML fallback page, rejects with `{ code: "internal", message: "The server answered
    HTTP 200 without a JSON body." }`. It is never resolved as `undefined`.
  - **Any other status** is parsed as `{ code, message }` and rejected unchanged, whatever
    the status line said. The frontend branches on `code` alone (D8, D32 point 3). A body
    that is not that shape, such as a proxy's HTML page, becomes `{ code: "internal",
    message: "The server answered HTTP <status> without an error body." }`.
  - **A `fetch` that throws** (DNS, refused connection, CORS) becomes `{ code: "internal",
    message: "Could not reach the server at <origin>: <thrown message>." }`.
- **The browser's local transport.** Every local command rejects with `{ code: "invalid",
  message: "<name> is only available in the desktop app." }` without making a request (D32
  point 6). The one exception is `get_client_capabilities`, which it answers itself with the
  browser's fixed answer (point 3 below).
- `runningInDesktopShell()` is exported from `commands.ts` and wraps `isTauri()` from
  `@tauri-apps/api/core`. It lives there because `commands.ts` is the only module allowed to
  import that package (D7, 028). It is called only by the bootstrap, never at module load
  and never on a command's path, because the existing mocks of that package provide
  `invoke` and nothing else.

**2. Events choose a transport by source.** In `src/lib/events.ts`:

- **One exported table, `EVENT_SOURCES`**, maps each event name a subscribe wrapper uses to
  the one source it arrives from, `"board"` or `"local"`. The rows are 048 Scope 3's table,
  copied:

  | Event | Source |
  | --- | --- |
  | `tasks:changed`, `repositories:changed`, `runs:changed` | board |
  | `settings:changed` (team and user settings only, after 048) | board |
  | `runs:tail` (opt-in per run, below) | board |
  | `plan-pass:progress` | board |
  | `runner:changed` (048's `subscribeToRunnerChanged`), `schedules:changed` | local |

  **No row names two sources.** 048 split `settings:changed` precisely so that none has to,
  and a two-source row would deliver every payload twice in solo and fixture mode, where one
  transport serves both. 048 publishes `plan-pass:progress` on the board's live channel, so
  a connected desktop that still plans locally before 060 does not see its own pass's
  progress over SSE. That is 059's and 060's to settle, not a reason for a second source
  here.
- The installed event transport becomes a pair as well, `{ board: EventTransport, local:
  EventTransport | null }`, with `setEventTransports(pair)` beside 028's
  `setEventTransport`, which again installs one transport for both. `subscribe<P>()`
  subscribes through the source its row names, once. A source whose transport is `null`
  (local, in the browser) subscribes to nothing, and the wrapper still resolves to an
  `UnlistenFn`. A browser component that subscribes to `schedules:changed` therefore gets
  silence, not an error.
- **028's `EventTransport` gains an optional third argument**, `options?: { runId?: string
  }`: `(event, onPayload, options?) => Promise<UnlistenFn>`. Tauri `listen` and the fixture
  transport ignore it. Only the SSE transport reads it, for the tail (below).
- **The SSE transport** is `sseEventTransport({ baseUrl, credential })`, built on
  `@microsoft/fetch-event-source` `2.0.1`, pinned exactly and imported by `events.ts` alone
  (D34). It sends `transportHeaders(credential)` from `commands.ts`, `Rimaia-Protocol` and
  the CSRF header included, because 048's route admits a browser only with the CSRF header.
  It passes `fetch: (input, init) => globalThis.fetch(input, init)` explicitly. The library
  otherwise uses `window.fetch`, and whether a stubbed global is visible through `window` is
  a detail of vitest's environment the tests should not depend on.
  - **One stream per client** (ADR-0034 point 3). The first subscription opens
    `GET <baseUrl>/api/v1/events`. Later subscriptions share that stream, and it is aborted
    when the last `UnlistenFn` is called. A frame's `event` field is the event name. Its
    `data` is the JSON payload the Tauri forwarder would have sent, and each listener
    registered for that name receives it. Every open, first or later, is a new
    `fetchEventSource` call with headers built at that moment.
  - **An empty id list is delivered as an empty id list.** Every stream 048 opens starts
    with `tasks:changed []`, `repositories:changed []`, `runs:changed []` and
    `settings:changed null`, meaning "re-read everything" (ADR-0018, ADR-0034 point 3). The
    existing wrappers' doc comments already tell subscribers what that means, and the
    transport must not filter it.
  - **Failures are classified in `onopen`, and `onerror` obeys the class.** In the library,
    an exception thrown from `onopen` or `onclose` reaches `onerror`, and a number returned
    from `onerror` is a retry. So:
    - `onopen` accepts a `200` whose content type is `text/event-stream`, and resets the
      attempt counter.
    - `onopen` throws a `FatalStreamError` carrying the body's `{ code, message }` for any
      `4xx`. 048 refuses before the stream with `401` (credential), `404` (a tail id it
      cannot show) and `400` (more than eight tails). It serves the stream under version
      skew, because the stream is a read, so it never answers `426`. `426` is fatal all the
      same, defensively, as D34 asks. None of these changes by asking again. A body that is
      not that shape becomes the same `internal` error the HTTP transport makes.
    - `onopen` throws a retriable error for a `5xx`, and for a `200` that is not
      `text/event-stream`.
    - `onerror` rethrows a `FatalStreamError`, which ends the library's loop, and returns
      `reconnectDelay(attempt)` for anything else.
    - `onclose` throws a retriable error. When a server ends a stream cleanly, the library
      calls `onclose` and stops, and reconnects only if `onclose` throws. 048 ends streams
      on purpose, on revalidation and when a caller's teams change, and expects the client
      to come back. Without this, a browser board stops refreshing after the first
      revalidation, and nothing says so.
  - **A fatal failure closes the stream and keeps its subscribers.** Every subscriber still
    holds its `UnlistenFn`, and the refcount is unchanged. The failure is reported to every
    subscriber of `subscribeToEventStreamFailure(onFailure)`, which is new in `events.ts`,
    as `EventStreamFailure { error: RimaiaError, fatal: true, droppedTails: [] }`. 050 turns
    `unauthenticated` into the sign-in page and `upgrade_required` into a reload. This task
    only reports them. **`reconnectEventStream()`**, also new, reopens the stream with the
    current credential and headers when it has subscribers, closing any stream still open,
    and does nothing when it has none. 050 calls it after a sign-in and on a team switch.
    Nothing reopens a fatally closed stream on its own.
  - **Backoff.** `reconnectDelay(attempt)` is a pure function exported for its test:
    `RECONNECT_BASE_MS = 1000`, doubling per attempt to `RECONNECT_CAP_MS = 30000`, and the
    attempt counter resets to 0 when a stream opens. A dropped connection on a laptop that
    slept must not hammer the server, and it must not wait a minute after waking either.
  - **The run tail is asked for at connect** (048 Scope 6). The stream carries a run's tail
    only if its URL named the run: `GET /api/v1/events?tail=<run_id>`, repeated, at most
    `MAX_TAILS_PER_STREAM = 8`. Changing the set means reconnecting. So:
    - `subscribeToRunsTail(runId, onTail)` takes the run it watches and passes `{ runId }`
      to the transport. It keeps a client-side filter, `payload.runId === runId`, because
      in solo `listen` admits every run's tail, as it does today.
    - The SSE transport keeps a refcounted set of watched run ids. A subscription that adds
      an id, and an unlisten that removes an id's last subscriber, each schedule a
      reconnect with the new `?tail=` list. Changes are coalesced with `queueMicrotask`, so
      a view mounting three run cards reconnects once, not three times. Every reconnect
      brings 048's four-event burst, which is harmless because it is a re-read.
    - **A ninth distinct run id is refused** at subscription with `{ code: "invalid",
      message: "At most 8 run tails can be watched at once." }`, and nothing reconnects.
      The server would refuse the whole stream with `400`, taking every board event with
      it, so the client refuses the one subscription instead. `ActiveRunCard.tsx` already
      treats a refused subscription as "show the seeded snapshot", and keeps doing so.
      Watching the eight most recent instead was declined: it silently freezes a card that
      is still on screen.
    - **A `404` on an open that named tails** is not fatal at first. The transport drops
      every tail id in that request (it cannot tell which one was refused), reopens once
      without them, and reports `EventStreamFailure { error, fatal: false, droppedTails }`.
      A dropped id stays out of every later `?tail=` list until its last subscriber
      unlistens, so it cannot turn each reconnect into a `404`. A `404` on an open with no
      tails is fatal like any other `4xx`. A stale id arises only when a run's task is
      deleted or when 050 switches team, and in both cases the card holding it is about to
      unmount, so dropping its healthy neighbours too costs a few seconds of tail, not
      board events.
    - `ActiveRunCard.tsx` passes its `runId`. That is the one component edit this task is
      allowed besides `RepositoryAddForm.tsx`. Its own filter may stay.
  - **Hidden documents.** Keep the library's default `openWhenHidden: false`. A background
    tab or a minimised window closes its stream, and on becoming visible it reconnects and
    receives 048's burst, so every open view re-reads. No view needs events while it cannot
    be seen, and a server holding one idle stream per background tab gains nothing.

**3. `get_client_capabilities`, a local command.** ADR-0034 point 5 defines it as the answer
only the client knows.

- **The type**, in `src/types.ts` and in `rimaia-core` at `crates/core/src/api/capabilities.rs`:

  ```ts
  export type ClientMode = "solo" | "connected" | "browser";
  export interface ClientCapabilities {
    mode: ClientMode;
    localRunnerId: string | null;
    canOpenInEditor: boolean;
    canRevealFiles: boolean;
    canChooseFolders: boolean;
  }
  ```

  The Rust side is a `#[serde(rename_all = "camelCase")]` struct with a `ClientMode` enum,
  not a string ("Enums, not strings"). Its constructor is a pure
  `ClientCapabilities::desktop(local_runner_id, server_url: Option<&str>)`: `mode` is
  `solo` when `server_url` is `None` and `connected` otherwise, and every `can*` is `true`.
  It lives in core, not the shell, because CI runs `cargo test -p rimaia-core` and nothing
  runs tests in `src-tauri`. It takes plain values because core must not depend on
  `rimaia-runner`.
- **The desktop answer** is a `#[tauri::command]` in `src-tauri/src/commands/app.rs`, which
  reads `runner_identity` (D28 part 6: `runner_id`, `server_url`) through the runner store
  that 041 keeps in `AppState`. It follows D32 point 8's rule that a local handler never
  reads the board's `ServiceContext`. If neither 040 nor 041 exposes a reader for
  `runner_identity`, add `identity::current(&RunnerStore)` in `crates/runner` and
  regenerate the runner cache with D33 point 3's recipe. Add nothing to the board.
- **The browser answer** is fixed, and is returned by the browser's local transport without
  a request: `{ mode: "browser", localRunnerId: null, canOpenInEditor: false,
  canRevealFiles: false, canChooseFolders: false }`.
- **A registry row**, `local("get_client_capabilities")`, its entry in the single
  `generate_handler!` list, the wrapper `getClientCapabilities()` using `local<T>`, and a
  row in 028's fixture table answering the solo shape. 063 will add `canUpdate`, and D34
  already expects that.
- **No MCP tool**, for this command or for `choose_folder` in Scope 4, under D20.6's
  2026-09-04 desktop-referent exception. `get_client_capabilities` answers a fact only the
  calling client knows, and an MCP client is not that client. `choose_folder` opens a native
  dialog, and an MCP client passes a path to `register_repository` directly. ADR-0021
  point 1 calls a command without a tool a defect unless the exception is recorded, so
  Scope 8's seam entry records both.

**4. The folder picker moves behind a local command.** `RepositoryAddForm.tsx` is the only
component that imports a Tauri plugin (ADR-0034 Context).

- **The command:** `choose_folder`, taking `{ title }` and returning `string | null`, where
  `null` means the user cancelled. It lives in `src-tauri/src/commands/app.rs` and uses the
  `tauri_plugin_dialog::DialogExt` the shell already imports for the startup dialog:
  `app.dialog().file().set_title(title).pick_folder(callback)`, with the callback sending
  through a `tokio::sync::oneshot` the async command awaits. It does **not** use
  `blocking_pick_folder`. That call blocks the thread it runs on, and the `lib.rs` comment
  on the startup dialog explains how blocking on the wrong thread deadlocks the window. A
  path that is not valid UTF-8 is `Error::invalid`, naming the path lossily.
- **The component:** `RepositoryAddForm.tsx` calls `chooseFolder("Choose a repository")`
  from `commands.ts` and behaves exactly as it does today on a path, on a cancel and on a
  failure. Its `ErrorBanner` shows the refusal if the picker fails.
- **The permission and the package go.** With no JavaScript caller left, `dialog:allow-open`
  is removed from `src-tauri/capabilities/default.json`, so the webview holds one IPC
  permission fewer. The page it hosts is now able to talk to a network server. The
  `@tauri-apps/plugin-dialog` npm package is removed from `package.json`. The Cargo crate
  and the plugin init stay, because the startup dialog and `choose_folder` use them. D6 gets
  a one-line amendment saying its npm half has gone and why. `opener:default` and
  `notification:default` are not this task's.
- **The two tests that mocked the plugin change.** `WelcomeView.test.tsx` and
  `RepositoriesSection.test.tsx` mock `@tauri-apps/plugin-dialog`. They now answer
  `choose_folder` through the `invoke` mock they already have. They are the only existing
  test files, apart from `commands.test.ts` and `events.test.ts`, that this task edits.

**5. The bootstrap.** A new `src/lib/client.ts` exports `installTransports(mode,
connection?)`, which is pure apart from calling the four setters, and `bootstrapClient()`,
which `src/main.tsx` awaits before its first render:

- `runningInDesktopShell()` is `false`: browser mode. `installTransports("browser")`
  installs HTTP and SSE at `window.location.origin` with a session credential for board
  commands and events, the browser refusal as the local command transport, and `null` as
  the local event transport. The credential's `csrf` is `csrfFromCookie()`, exported from
  `client.ts`: it reads the `rimaia_csrf` cookie from `document.cookie` on every call, and
  returns `null` when there is none. That is the mechanism 047 Scope 11 decided: a readable
  cookie holding `csrf_for(secret)`, sent back as `X-Rimaia-CSRF`. The value is
  base64url, so it is used as read, with no decoding.
- `runningInDesktopShell()` is `true`: ask for the capabilities through
  `getClientCapabilities()`, which reaches `invoke` through the default local transport.
  `client.ts` never imports `invoke` itself (D7, and Scope 7's boundary test). For `solo`,
  leave the defaults installed, which is today's behaviour. For `connected`,
  `installTransports("connected", { serverUrl, token })` installs HTTP and SSE with the
  bearer credential for board kinds and `invoke`/`listen` for local ones. Nothing in this
  task supplies `{ serverUrl, token }`. 059 adds the local command that does. Until then a
  `connected` answer rejects with exactly `{ code: "internal", message: "This desktop is
  set to connect to a server, which this build cannot do yet (task 059)." }` and installs
  nothing, rather than falling back to solo and writing to a board nobody reads (D32 point
  3's argument).
- **What `main.tsx` renders when `bootstrapClient()` rejects.** `App` has not mounted, so
  it cannot show the error. `main.tsx` renders `ErrorBanner` alone in the root, with the
  rejection passed through `toRimaiaError`, and never renders `App`. The banner has no
  dismiss button, because there is nothing behind it. `main.tsx` holds no other logic, so
  the failure it renders is the one `client.test.ts` pins.
- **No component calls a setter or reads the mode.** The fixture entry, `src/dev/main.tsx`,
  still installs its own transports through 028's setters and never calls
  `bootstrapClient()`, so fixture mode renders exactly what it rendered before.

**6. An HTTP mock for the test suite**, beside the `@tauri-apps/api/core` mocks. D34
declines `msw`: "049's HTTP mock replaces `fetch` at the same boundary where the suite
already mocks `@tauri-apps/api/core`."

- `src/test/http.ts` exports `mockHttp()`. It installs a `fetch` with `vi.stubGlobal` and
  returns a handle:
  - `answer(name, value | (args) => value)`;
  - `refuse(name, status, { code, message })`, and `refuseRaw(name, status, body)` for a
    non-JSON body;
  - `unreachable(message)`, which makes `fetch` throw;
  - `requests`, each one recorded as `{ name, url, method, headers, credentials, body }`
    with `body` parsed;
  - `events()`, which answers `GET /api/v1/events` with a controllable stream. Its handle
    offers `send(event, payload)`, `end()`, which closes the current body cleanly (the
    library's `onclose`), `fail()`, which errors the current body (the library's
    `onerror`), and `failOpen(status, body, contentType?)`, which answers the next open
    instead of streaming. `opens` lists every open with its URL, so a test reads the
    `?tail=` list, and `open` is true while a stream is live. Keeping `end()` and `fail()`
    apart is what stops a reconnect test passing for the wrong one of the two reasons a
    stream stops.
- An unknown name answers `404 { code: "not_found" }`, as the server does, so a test that
  forgets a route fails on the answer instead of hanging.
- Teardown is `vi.unstubAllGlobals()` in the test file's `afterEach`, the same way the
  files that mock `invoke` reset `mockInvoke`.
- **Nothing mocks `commands.ts` or `events.ts` themselves**, for the reason
  `StorageSection.test.tsx` gives. The new tests go through the wrappers, as a component
  does.

**7. Guard the boundary.** A new `src/lib/boundary.test.ts` reads every non-test file under
`src/` and asserts:

- `@tauri-apps/api/core` is imported only by `commands.ts`;
- `@tauri-apps/api/event` and `@microsoft/fetch-event-source` are imported only by
  `events.ts`;
- no file imports `@tauri-apps/plugin-*`;
- no file outside `src/lib/` calls a transport setter or `bootstrapClient`, except
  `src/main.tsx` and `src/dev/main.tsx`.

This is how "no component imports a Tauri plugin" stays true after the one that did is
fixed.

**8. The records.**

- A seam-contract entry, with the next free `D` number, in the four-part shape: *Task 049's
  cross-cutting choices*. It records, each with its reason and the alternative it declined:
  the `ClientCapabilities` field names and the browser's fixed answer; `installTransports`,
  `bootstrapClient` and `csrfFromCookie`; `transportHeaders` as the one place request
  headers are built; `EVENT_SOURCES` with one source per row; `subscribeToEventStreamFailure`,
  `EventStreamFailure` and `reconnectEventStream`, with the fatal and retriable classes;
  the tail's refcounted set, its cap and the `404` rule; `RECONNECT_BASE_MS` and
  `RECONNECT_CAP_MS`; the `mockHttp()` API; and the two commands with no MCP tool under
  D20.6's desktop-referent exception. It binds 050, 054, 056, 059, 061 and 063.
- A row for 049 in the seam contract's "How to use this" table: D6 · D7 · D8 · D14 · D20 ·
  D28 · D32 · D33 · D34, 048's entry, and this task's own.
- The one-line D6 amendment from Scope 4.

## Out of scope

- **Serving the bundle, the sign-in page, the account page, the team switcher and CORS for
  the Tauri origins** (050). This task makes no request that could succeed against a real
  server from `npm run dev`. Vite answers `/api/v1/*` with its own 404 page, which the
  transport reports as an `internal` error naming the status. That is correct behaviour, not
  something to work around with a dev proxy. A proxy is 050's to add, if 050 wants one.
- **Browser states for local-only actions.** Hiding "Open worktree", making runner settings
  read-only and showing each runner's doctor result are 050's. So is a
  `useClientCapabilities` hook. This task ships the command and its wrapper, and changes no
  component's rendering. ADR-0024's fluid layout is 050's as well.
- **Connected mode itself** (059): the desktop token, the server URL, the local command that
  returns them, and `AppState.board` becoming `None`.
- **Reacting to `unauthenticated` or `upgrade_required`** beyond rejecting the command and
  reporting the stream failure. What the app does next, including when to call
  `reconnectEventStream()`, is 050's.
- **Showing a dropped tail.** A `fatal: false` failure is reported, and no component
  renders it in this task.
- **New server routes or event names.** If the transport needs one, 046 or 048 missed it.
  Stop and say so.
- **Request timeouts, retries of commands and offline queuing.** A command that fails
  rejects, as an `invoke` that fails does today.
- **Removing `@tauri-apps/plugin-opener` or `@tauri-apps/plugin-notification`** from
  `package.json`, even though no frontend file imports them.

## Acceptance criteria

- **Commands, in `src/lib/commands.test.ts`** (extended), with `mockHttp()` and the
  existing `invoke` mock:
  - `it("posts a board command to /api/v1/<name> with the invoke arguments as its body")`.
    The body equals `JSON.stringify(args)`, and is `{}` for a command with no arguments.
  - `it("sends Rimaia-Protocol with the PROTOCOL_VERSION literal on every request")`.
  - `it("sends the CSRF header and no Authorization header with a session credential")`.
    `credentials` is `"same-origin"`.
  - `it("reads the CSRF header from the rimaia_csrf cookie on every request")`. Browser mode
    is installed with `installTransports("browser")`. The test sets `document.cookie` to
    `rimaia_csrf=first`, sends a command, sets it to `rimaia_csrf=second`, and sends again.
    The two requests carry exactly `first` and `second`. With the cookie cleared, the next
    request carries no `X-Rimaia-CSRF` header at all.
  - `it("sends a bearer token, omits cookies and sends no CSRF header with a bearer credential")`.
  - `it("resolves a null answer as void")`.
  - `it("turns a 200 without a JSON body into an internal error")`. The message is exactly
    `The server answered HTTP 200 without a JSON body.`
  - `it("rejects with the server's code and message unchanged")`. This covers `invalid`
    (400), `unauthenticated` (401), `not_found` (404), `conflict` (409), `upgrade_required`
    (426) and `internal` (500).
  - `it("turns an error body that is not JSON into an internal error naming the status")`.
    The message is exactly `The server answered HTTP 502 without an error body.`
  - `it("turns a fetch that throws into an internal error naming the server")`.
  - `it("gives a wrapper the same answer and the same error through invoke and HTTP")`.
    Three wrappers, one read, one write and one refusal, run through both transports with
    the same seeded values, and the results are compared with `toEqual`.
  - `it("sends every command through invoke when nothing is installed")`.
  - `it("routes board commands to HTTP and local commands to invoke in connected mode")`.
  - `it("refuses a local command in the browser without making a request")`. The message
    is exactly `run_doctor is only available in the desktop app.` and `requests` is empty.
  - `it("answers get_client_capabilities in the browser without a request")`, with exactly
    the fixed browser object in Scope 3.
  - `it("still sends both kinds through a transport installed with setCommandTransport")`.
    This is 028's fixture path.
- **Events, in `src/lib/events.test.ts`** (extended), with `vi.useFakeTimers()` and no
  real waiting:
  - `it("opens one event stream for every subscriber and aborts it after the last unlisten")`.
  - `it("delivers a frame's payload to every wrapper subscribed to its event name")`.
  - `it("delivers an empty id list after a reconnect as an empty id list")`.
  - `it("sends the credential and Rimaia-Protocol on the event stream request")`.
  - `it("stops reconnecting and reports unauthenticated when the stream answers 401")`.
  - `it("stops reconnecting and reports the refusal when the stream answers 400, 404 or 426")`,
    one case per status, each opened with no tails. After each, advancing the fake clock
    past `RECONNECT_CAP_MS` makes no further request.
  - `it("reconnects after a failed stream once the backoff delay has passed")`, driven by
    `fail()`. It advances the fake clock by exactly `reconnectDelay(0)` and asserts the
    second request, and asserts none before that.
  - `it("reconnects after the server ends the stream cleanly")`, driven by `end()`, with the
    same clock assertions. This is the case the library's default gets wrong.
  - `it("retries a 5xx and a 200 that is not an event stream with backoff")`.
  - `it("doubles the reconnect delay from one second to a thirty second cap")` and
    `it("resets the reconnect delay after a stream opens")`, against `reconnectDelay`.
  - `it("keeps subscribers after a fatal failure and reopens on reconnectEventStream")`: a
    `401`, then `reconnectEventStream()` with a new cookie value. The new open carries the
    new CSRF header, and a frame sent on it reaches the original subscriber.
  - `it("reconnects with both run ids when a second tail subscriber arrives")`: the second
    open's URL carries exactly `?tail=<first>&tail=<second>`, and a tail for either run
    reaches only its own subscriber.
  - `it("reconnects without a run id once its last tail subscriber unlistens")`.
  - `it("reconnects once for tail subscriptions made in the same tick")`.
  - `it("refuses a ninth watched run without reconnecting")`, with the exact message in
    Scope 2.
  - `it("drops stale tail ids on a 404 and keeps delivering board events")`: an open naming
    a tail answers `404`, the next open names none, a `tasks:changed` frame reaches its
    subscriber, and one `EventStreamFailure` with `fatal: false` names the dropped id.
  - `it("delivers each payload once when one transport serves both sources")`, with 028's
    `setEventTransport`.
  - `it("subscribes board events over SSE and local events through listen in connected mode")`.
  - `it("resolves a local-only subscription in the browser without subscribing to anything")`.
  - `it("has a source row for every event a subscribe wrapper uses")`, and each row equals
    the table in Scope 2.
- **The bootstrap, in `src/lib/client.test.ts`** (new). `runningInDesktopShell()` is driven
  through the `@tauri-apps/api/core` mock, which here provides `isTauri` beside `invoke`:
  - `it("installs HTTP and SSE with a session credential at the page's origin in the browser")`.
    A board command reaches `mockHttp()` at `window.location.origin`, and `invoke` is never
    called.
  - `it("leaves invoke and listen installed in solo")`. A board command reaches `invoke`,
    and `fetch` is never called.
  - `it("fails loudly naming task 059 when the shell answers connected")`. The rejection is
    exactly the error in Scope 5, and a board command sent afterwards still reaches
    `invoke`, which shows no transport was installed.
- **Local commands:**
  - In `crates/core/src/api/capabilities.rs`:
    `a_desktop_without_a_server_is_solo_and_can_do_every_local_action` and
    `a_desktop_with_a_server_url_is_connected_and_names_its_local_runner`, run by
    `cargo test -p rimaia-core`. A serialization test asserts the exact JSON keys in Scope 3.
  - `RepositoryAddForm.tsx` imports nothing from `@tauri-apps`. `WelcomeView.test.tsx` and
    `RepositoriesSection.test.tsx` pass with `choose_folder` answered through the `invoke`
    mock. They cover a chosen path, a cancel (`null`) and a refusal, which today's tests
    cover through the plugin mock.
  - `src-tauri/capabilities/default.json` no longer lists `dialog:allow-open`. `package.json`
    no longer lists `@tauri-apps/plugin-dialog`, and lists
    `@microsoft/fetch-event-source` at exactly `2.0.1`. No other dependency changes.
- **The boundary**, in `src/lib/boundary.test.ts`:
  `it("imports @tauri-apps only from commands.ts and events.ts, and no plugin at all")`,
  `it("imports fetch-event-source only from events.ts")` and
  `it("installs transports only from main.tsx, the fixture entry and src/lib")`.
- **Nothing else moved:**
  - Every test file that mocks `@tauri-apps/api/core` passes, and none shows a change in
    `git diff --stat` except `commands.test.ts`, `WelcomeView.test.tsx` and
    `RepositoriesSection.test.tsx`. `client.test.ts` is new. `ActiveRunCard`'s tests pass
    unedited, because the wrapper's own `runId` filter admits exactly the snapshots the
    card's filter did.
  - `./scripts/check-command-wiring.sh` passes with the two new local rows.
  - 028's fixture coverage test passes with rows for `get_client_capabilities` and
    `choose_folder`.
  - 028's bundle test still finds no fixture in the production build.
- **The screenshots do not change.** `npm run screenshot -- --label before` on the commit
  before this task and `npm run screenshot` after it produce byte-identical PNGs, checked
  with `shasum` over both label directories. This task changes transports, not pixels, and
  028's determinism is what makes the comparison meaningful. The PR says it was run.
- **Manual, recorded in the PR body:** `RIMAIA_DATA_DIR=/tmp/rimaia-049 npm run tauri dev`
  starts in solo mode. The board, Runs view and Settings behave as before, and "Add
  repository" opens the native folder picker and registers the chosen folder.
- **The records:** the seam entry and the "How to use this" row from Scope 8 are in the
  diff, and the entry's *Binds* line names 050, 054, 056, 059, 061 and 063.
- `CLAUDE.md`'s Testing section names `src/test/http.ts` beside the `invoke` mocks as the
  way a test reaches the HTTP transport, and repeats that neither wrapper module is mocked.
- Every CI command passes, with `SQLX_OFFLINE=true` exported, exactly as `ci.yml` runs
  them.

## Notes

**Read first.** ADR-0034 in full: point 4 is the table this task implements, point 3 the
stream, point 5 the capabilities. ADR-0030 points 2 to 4 (cookie, CSRF, bearer tokens,
the desktop token). ADR-0037 point 4 (the protocol header, and why a browser is not
exempt). ADR-0018 (the empty-id recovery). ADR-0024 only for its constraint: nothing here
may change a pixel. 048's task file, Scopes 3, 6, 7 and 8 and its "What the next tasks
expect" (the wire table, the burst, `?tail=`, revalidation, the refusals). 047's Scope 11
cookie table (`rimaia_csrf`). Seam entries:

- **048's entry**, *Task 048's cross-cutting choices*: the wire table, `?tail=` and its cap,
  the opening burst, and streams that end on revalidation;
- **D32**, points 3, 6 and 7 above all: wire format, status table, doors, cookie and header
  names, `PROTOCOL_VERSION`, and the browser refusal;
- **D34**, the `@microsoft/fetch-event-source` row and its `onopen` rule, and the declined
  `msw`;
- **D7**, the two modules that own the boundary;
- **D8**, which `code` means what;
- **D14** and its amendments, the tail's catch-up read, which is unchanged: a component
  still seeds from `getRunTail` before subscribing;
- **D20.6** and its 2026-09-04 amendment, the desktop-referent exception both new commands
  sit under;
- **D28** part 6, `runner_identity`;
- **D33** point 3, only if Scope 3 needs a runner query;
- **D6** and its amendments, which this task amends by one line.

**No migration.** If Scope 3 adds `identity::current`, that is one runner query and a
regenerated `crates/runner/.sqlx/`, not a migration.

**Files to start from.**

- `src/lib/commands.ts`: after 046, `board<T>`, `local<T>`, `PROTOCOL_VERSION`,
  `toRimaiaError`, and 028's `CommandTransport` and `setCommandTransport`.
- `src/lib/events.ts`: 028's `subscribe<P>()`, `EventTransport` and `setEventTransport`.
  The doc comments on the empty-id contract stay as they are.
- `src/lib/commands.test.ts`, and `src/views/settings/StorageSection.test.tsx` for why the
  wrappers are never mocked.
- `src/components/RepositoryAddForm.tsx`, `src/views/WelcomeView.test.tsx` and
  `src/views/settings/RepositoriesSection.test.tsx`.
- `src/components/runs/ActiveRunCard.tsx`, which passes its `runId` to
  `subscribeToRunsTail`.
- `src/main.tsx`, and `src/dev/main.tsx` from 028.
- `src/test/setup.ts`, `vitest.config.ts`.
- `src-tauri/src/lib.rs`: the `DialogExt` import, the startup dialog's comment on threads,
  and the single `generate_handler!` list from 046.
- `src-tauri/src/commands/app.rs`, `src-tauri/capabilities/default.json`.
- `crates/core/src/api/registry.rs` and `crates/core/src/api/mod.rs` from 046.
- `scripts/check-command-wiring.sh` from 046.

**What the chain provides.**

- **046:** the registry, `board<T>`/`local<T>`, `PROTOCOL_VERSION`, the route per board row,
  and the status table.
- **047:** `rimaia_session`, the readable `rimaia_csrf` cookie and the `X-Rimaia-CSRF`
  header it is sent as (Scope 11); `rmd_` desktop tokens; `ErrorCode`'s `unauthenticated`.
  `upgrade_required` is 046's.
- **048:** `GET /api/v1/events` filtered by team, the four-event burst on every open,
  refusals as `400`, `401` and `404` before the stream, streams that end on revalidation,
  `?tail=` with its cap of eight, `runner:changed` and `subscribeToRunnerChanged`, and the
  split of machine-local events off the team channel.
- **028:** the two setters, the fixture transports, the fixture coverage test, the bundle
  test and `npm run screenshot`.

**Check Scope 2 against 048's landed seam entry before writing `EVENT_SOURCES`.** The rows,
the refusal statuses and the tail mechanism here are copied from 048's task file. If the
landed entry differs, 048 wins. Follow it, and record the difference in Scope 8's entry. If
the difference would change a test's expected value, stop and ask rather than choosing.

**What comes next expects.**

- **050:** browser mode working behind `bootstrapClient()`, `subscribeToEventStreamFailure`
  for `unauthenticated` and `upgrade_required`, `reconnectEventStream()` to call after a
  sign-in and on a team switch, `transportHeaders` as the one place to add the team header,
  and `getClientCapabilities()` to hang browser states on. 050's CORS allow-list must admit the `Authorization`,
  `Rimaia-Protocol` and `Content-Type` request headers from the Tauri origins, because this
  transport sends all three and each triggers a preflight.
- **059:** `installTransports("connected", { serverUrl, token })` exactly as tested here, a
  `client.test.ts` whose loud-failure case it replaces, and a `get_client_capabilities`
  that already answers `connected` once `runner_identity` has a `server_url`. Also the
  connected desktop's local plan-pass progress before 060, which Scope 2 leaves to it.
- **054:** `choose_folder` behind a local command, and `mockHttp()`.
- **056:** `getClientCapabilities()` and `localRunnerId`.
- **061:** `localRunnerId`, to tell "your runner" from someone else's.
- **063:** adds `canUpdate` to `ClientCapabilities`, on both sides.

**Size.** Roughly 2,400–3,200 lines: about 750 of transport code across `commands.ts`,
`events.ts` and `client.ts`, 250 for the HTTP mock, 150 of Rust, and the rest tests. That
fits one session. If it runs long, cut at the seam between Scopes 1, 2 and 5–7 on one side
and Scopes 3 and 4 on the other. Land the transports, the mock and the boundary test first,
and move `get_client_capabilities` and `choose_folder` to a follow-up task that 050 then
depends on. The boundary test's "no plugin" assertion goes with them. **Never cut** the
fatal `4xx` handling on the stream, the reconnect after a clean close, the backoff, or the
both-transports comparison. The first and third turn a signed-out tab into a request loop
against the server, the second leaves a browser board that silently stops refreshing after
048's first revalidation, and without the fourth nothing on the frontend shows the
transports agree.
